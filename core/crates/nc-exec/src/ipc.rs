//! Framing between the executor library and its own `nc-exec-worker` process (stdin/stdout pipes).
//!
//! This is a private, same-binary-version IPC inside one device, not a network wire format, so it is not in
//! `proto/` (AGENT.md §1 keeps wire formats there). Every length is bounds-checked against [`ceiling`] before
//! any allocation, because the worker side reads it and the parent side reads the worker's reply.

use std::io::{self, Read, Write};
use std::time::Duration;

use crate::limits::{ceiling, Limits};

const REQ_MAGIC: &[u8; 4] = b"NCX1";
const RESP_MAGIC: &[u8; 4] = b"NCR1";
/// Longest diagnostic message the worker may send (no job data is ever put in it).
pub const MAX_MESSAGE: u64 = 1024;

/// Job sent to the worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerRequest {
    /// Limits to apply.
    pub limits: Limits,
    /// SHA-256 the module must hash to; the worker re-checks it.
    pub module_sha256: [u8; 32],
    /// Module bytes (binary Wasm).
    pub module: Vec<u8>,
    /// Input bytes, readable through `nc.input_read`.
    pub input: Vec<u8>,
}

/// How a run ended inside the worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum WorkerStatus {
    /// `run` returned normally.
    Exited = 0,
    /// Fuel ran out.
    OutOfFuel = 1,
    /// Epoch deadline (worker-side wall clock) reached.
    EpochDeadline = 2,
    /// Linear memory or table growth beyond the limit.
    MemoryLimit = 3,
    /// Wasm stack overflow.
    StackOverflow = 4,
    /// Output or checkpoint over its cap.
    OutputLimit = 5,
    /// Any other trap (unreachable, bad memory access, ...).
    Trap = 6,
    /// Module refused before running (bad hash, invalid, forbidden import, wrong exports).
    Refused = 7,
}

impl WorkerStatus {
    fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::Exited,
            1 => Self::OutOfFuel,
            2 => Self::EpochDeadline,
            3 => Self::MemoryLimit,
            4 => Self::StackOverflow,
            5 => Self::OutputLimit,
            6 => Self::Trap,
            7 => Self::Refused,
            _ => return None,
        })
    }
}

/// Reply from the worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerResponse {
    /// How the run ended.
    pub status: WorkerStatus,
    /// Value returned by `run` (only meaningful for [`WorkerStatus::Exited`]).
    pub exit_code: i32,
    /// Fuel consumed.
    pub fuel_used: u64,
    /// Time spent compiling, in microseconds.
    pub compile_us: u64,
    /// Time spent running, in microseconds.
    pub run_us: u64,
    /// Output written through `nc.output_write`.
    pub output: Vec<u8>,
    /// Last checkpoint written through `nc.checkpoint`.
    pub checkpoint: Vec<u8>,
    /// Short diagnostic (trap kind, refusal reason). Never contains job data.
    pub message: String,
}

fn put_u64(w: &mut impl Write, v: u64) -> io::Result<()> {
    w.write_all(&v.to_le_bytes())
}

fn put_bytes(w: &mut impl Write, b: &[u8]) -> io::Result<()> {
    put_u64(w, b.len() as u64)?;
    w.write_all(b)
}

fn get_u64(r: &mut impl Read) -> io::Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

fn bad(msg: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

fn get_bytes(r: &mut impl Read, max: u64) -> io::Result<Vec<u8>> {
    let len = get_u64(r)?;
    if len > max {
        return Err(bad("length over cap"));
    }
    let len = usize::try_from(len).map_err(|_| bad("length over cap"))?;
    let mut v = vec![0u8; len];
    r.read_exact(&mut v)?;
    Ok(v)
}

impl WorkerRequest {
    /// Serialise to `w`.
    pub fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        w.write_all(REQ_MAGIC)?;
        let l = &self.limits;
        put_u64(w, l.fuel)?;
        put_u64(w, u64::try_from(l.wall.as_millis()).unwrap_or(u64::MAX))?;
        put_u64(w, l.memory_bytes)?;
        put_u64(w, l.stack_bytes)?;
        put_u64(w, l.output_bytes)?;
        put_u64(w, l.checkpoint_bytes)?;
        w.write_all(&self.module_sha256)?;
        put_bytes(w, &self.module)?;
        put_bytes(w, &self.input)?;
        w.flush()
    }

    /// Parse from `r`, refusing anything over the hard ceilings.
    pub fn read_from(r: &mut impl Read) -> io::Result<Self> {
        let mut magic = [0u8; 4];
        r.read_exact(&mut magic)?;
        if &magic != REQ_MAGIC {
            return Err(bad("bad request magic"));
        }
        let limits = Limits {
            fuel: get_u64(r)?,
            wall: Duration::from_millis(get_u64(r)?),
            memory_bytes: get_u64(r)?,
            stack_bytes: get_u64(r)?,
            output_bytes: get_u64(r)?,
            checkpoint_bytes: get_u64(r)?,
        };
        limits.validate().map_err(bad)?;
        let mut module_sha256 = [0u8; 32];
        r.read_exact(&mut module_sha256)?;
        let module = get_bytes(r, ceiling::MODULE_BYTES)?;
        let input = get_bytes(r, ceiling::INPUT_BYTES)?;
        Ok(Self {
            limits,
            module_sha256,
            module,
            input,
        })
    }
}

impl WorkerResponse {
    /// Serialise to `w`.
    pub fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        w.write_all(RESP_MAGIC)?;
        w.write_all(&[self.status as u8])?;
        w.write_all(&self.exit_code.to_le_bytes())?;
        put_u64(w, self.fuel_used)?;
        put_u64(w, self.compile_us)?;
        put_u64(w, self.run_us)?;
        put_bytes(w, &self.output)?;
        put_bytes(w, &self.checkpoint)?;
        let msg = self.message.as_bytes();
        put_bytes(w, &msg[..msg.len().min(MAX_MESSAGE as usize)])?;
        w.flush()
    }

    /// Parse from `r`, refusing output or checkpoint over the caps the parent asked for.
    pub fn read_from(r: &mut impl Read, max_output: u64, max_checkpoint: u64) -> io::Result<Self> {
        let mut magic = [0u8; 4];
        r.read_exact(&mut magic)?;
        if &magic != RESP_MAGIC {
            return Err(bad("bad response magic"));
        }
        let mut st = [0u8; 1];
        r.read_exact(&mut st)?;
        let status = WorkerStatus::from_u8(st[0]).ok_or_else(|| bad("bad status"))?;
        let mut ec = [0u8; 4];
        r.read_exact(&mut ec)?;
        let fuel_used = get_u64(r)?;
        let compile_us = get_u64(r)?;
        let run_us = get_u64(r)?;
        let output = get_bytes(r, max_output)?;
        let checkpoint = get_bytes(r, max_checkpoint)?;
        let message = String::from_utf8_lossy(&get_bytes(r, MAX_MESSAGE)?).into_owned();
        Ok(Self {
            status,
            exit_code: i32::from_le_bytes(ec),
            fuel_used,
            compile_us,
            run_us,
            output,
            checkpoint,
            message,
        })
    }

    /// Size of the fixed part plus the given payload sizes; the parent uses it to cap pipe reads.
    pub fn max_encoded_len(max_output: u64, max_checkpoint: u64) -> u64 {
        4 + 1 + 4 + 8 * 3 + 8 + max_output + 8 + max_checkpoint + 8 + MAX_MESSAGE
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip() {
        let req = WorkerRequest {
            limits: Limits::default(),
            module_sha256: [7; 32],
            module: vec![1, 2, 3],
            input: vec![4, 5],
        };
        let mut buf = Vec::new();
        req.write_to(&mut buf).unwrap();
        assert_eq!(WorkerRequest::read_from(&mut buf.as_slice()).unwrap(), req);
    }

    #[test]
    fn response_over_cap_is_refused() {
        let resp = WorkerResponse {
            status: WorkerStatus::Exited,
            exit_code: 0,
            fuel_used: 1,
            compile_us: 0,
            run_us: 0,
            output: vec![0; 100],
            checkpoint: vec![],
            message: String::new(),
        };
        let mut buf = Vec::new();
        resp.write_to(&mut buf).unwrap();
        assert!(WorkerResponse::read_from(&mut buf.as_slice(), 99, 0).is_err());
        assert_eq!(
            WorkerResponse::read_from(&mut buf.as_slice(), 100, 0).unwrap(),
            resp
        );
    }

    #[test]
    fn huge_length_prefix_does_not_allocate() {
        let mut buf = REQ_MAGIC.to_vec();
        for v in [1u64, 1000, 1 << 20, 1 << 16, 0, 0] {
            buf.extend_from_slice(&v.to_le_bytes());
        }
        buf.extend_from_slice(&[0; 32]);
        buf.extend_from_slice(&u64::MAX.to_le_bytes());
        assert!(WorkerRequest::read_from(&mut buf.as_slice()).is_err());
    }
}
