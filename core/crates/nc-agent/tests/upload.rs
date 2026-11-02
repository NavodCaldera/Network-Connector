#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 1.L6 (laptop-gap G5): resumable chunked uploads on the agent side against a fake coordinator that
//! keeps upload sessions like the real one (proto/README.md "Resumable chunked upload (1.L5)"). Covers a link
//! drop mid-upload, resume after a restart, an offset mismatch, `upload_busy`, the budget running out
//! mid-upload, a switch to cellular, a blob hash mismatch and Stop.

mod common;

use common::*;
use nc_agent::budget::{BudgetGate, NetworkBudget, Refusal};
use nc_agent::coord::{ChunkReply, CoordError, Coordinator};
use nc_agent::probe::{FakeProbe, PlatformProbe};
use nc_agent::transfer::{UploadError, UploadGate, Uploader, UPLOAD_OVERHEAD};
use nc_protocol::pb::v1::*;
use nc_tariff::TrafficClass;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

const MIB: usize = 1024 * 1024;

fn blob(len: usize) -> (String, Vec<u8>) {
    let b: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
    (hex::encode(Sha256::digest(&b)), b)
}

fn no_wait(_: Duration) -> bool {
    false
}

fn ungated(coord: &dyn Coordinator) -> Uploader<'_> {
    Uploader {
        coord,
        gate: None,
        stopped: &|| false,
        wait: &no_wait,
        chunk: MIB,
        retry_base: Duration::from_millis(1),
    }
}

fn gated<'a>(
    coord: &'a dyn Coordinator,
    gate: &'a BudgetGate,
    probe: &'a FakeProbe,
) -> Uploader<'a> {
    Uploader {
        coord,
        gate: Some(UploadGate {
            budget: gate,
            probe,
            clock: Box::leak(Box::new(FixedClock(noon()))),
            class: TrafficClass::ResultUpload,
        }),
        stopped: &|| false,
        wait: &no_wait,
        chunk: MIB,
        retry_base: Duration::from_millis(1),
    }
}

fn chunk_offsets(c: &FakeCoord) -> Vec<u64> {
    c.calls()
        .iter()
        .filter_map(|x| x.strip_prefix("chunk:"))
        .map(|x| x.split('@').nth(1).unwrap().parse().unwrap())
        .collect()
}

#[test]
fn link_drop_mid_upload_then_restart_resumes_without_resending() {
    let c = FakeCoord::default();
    let (h, b) = blob(3 * MIB + 5);
    c.s.lock().unwrap().chunks_left = Some(2);
    let e = ungated(&c).upload(&h, &b).unwrap_err();
    assert!(
        matches!(e, UploadError::Coord(ref x) if x.is_transient()),
        "{e:?}"
    );
    assert_eq!(c.s.lock().unwrap().chunk_bytes, 2 * MIB as u64);
    assert!(c.s.lock().unwrap().puts.is_empty());

    // "Restart": a fresh uploader with no local state; the server offset drives the resume.
    c.s.lock().unwrap().chunks_left = None;
    c.s.lock().unwrap().calls.clear();
    ungated(&c).upload(&h, &b).unwrap();
    let s = c.s.lock().unwrap();
    assert_eq!(
        s.chunk_bytes,
        b.len() as u64,
        "verified bytes were sent again"
    );
    assert_eq!(s.puts.get(&h).unwrap(), &b);
    assert!(s.sessions.is_empty());
    drop(s);
    assert_eq!(chunk_offsets(&c), vec![2 * MIB as u64, 3 * MIB as u64]);
}

#[test]
fn offset_mismatch_follows_the_server_offset() {
    let c = FakeCoord::default();
    let (h, b) = blob(2 * MIB + 1);
    {
        let mut s = c.s.lock().unwrap();
        // An earlier run already got the first MiB through, but this run is told 0 (stale view).
        s.sessions.insert(h.clone(), b[..MIB].to_vec());
        s.offset_lie = Some(0);
    }
    ungated(&c).upload(&h, &b).unwrap();
    let s = c.s.lock().unwrap();
    assert_eq!(s.puts.get(&h).unwrap(), &b);
    assert_eq!(s.chunk_bytes, (b.len() - MIB) as u64);
    drop(s);
    // 0 refused with 409 upload_offset, then continued at the server's offset.
    assert_eq!(chunk_offsets(&c), vec![0, MIB as u64, 2 * MIB as u64]);
}

#[test]
fn busy_session_is_retried_after_a_pause() {
    let c = FakeCoord::default();
    let (h, b) = blob(MIB + 3);
    c.s.lock().unwrap().busy = 2;
    let waits = AtomicUsize::new(0);
    let wait = |_: Duration| {
        waits.fetch_add(1, Ordering::SeqCst);
        false
    };
    let up = Uploader {
        wait: &wait,
        ..ungated(&c)
    };
    up.upload(&h, &b).unwrap();
    assert_eq!(
        waits.load(Ordering::SeqCst),
        2,
        "backed off on each 409 upload_busy"
    );
    assert_eq!(c.s.lock().unwrap().puts.get(&h).unwrap(), &b);
}

#[test]
fn budget_running_out_mid_upload_stops_before_the_next_chunk_and_resumes_later() {
    let c = FakeCoord::default();
    let (h, b) = blob(4 * MIB);
    let probe = FakeProbe::new(good_device(), home());
    // Room for two chunks plus the per-request overhead, not three.
    let remaining = 2 * (MIB as u64 + UPLOAD_OVERHEAD) + UPLOAD_OVERHEAD + 1000;
    let gate = BudgetGate::new(&NetworkBudget::Metered(metered(remaining)), noon()).unwrap();
    let e = gated(&c, &gate, &probe).upload(&h, &b).unwrap_err();
    assert_eq!(e, UploadError::NotNow(Refusal::Budget));
    assert_eq!(c.s.lock().unwrap().chunk_bytes, 2 * MIB as u64);
    let left = gate.available_bytes().unwrap();
    assert!(
        left < MIB as u64 + UPLOAD_OVERHEAD,
        "stopped only when the next chunk did not fit"
    );
    assert!(left <= remaining, "never over budget");

    // Next billing period (fresh budget): only the missing half is sent.
    let gate = BudgetGate::new(&NetworkBudget::Metered(metered(1 << 30)), noon()).unwrap();
    gated(&c, &gate, &probe).upload(&h, &b).unwrap();
    let s = c.s.lock().unwrap();
    assert_eq!(s.chunk_bytes, b.len() as u64);
    assert_eq!(s.puts.get(&h).unwrap(), &b);
}

/// Delegates to a [`FakeCoord`]; after `after` accepted chunks it moves the probe to cellular.
struct SwitchAfter {
    inner: FakeCoord,
    probe: Arc<FakeProbe>,
    after: AtomicUsize,
}
impl Coordinator for SwitchAfter {
    fn register(&self, r: &RegisterRequest) -> Result<(), CoordError> {
        self.inner.register(r)
    }
    fn heartbeat(&self, r: &HeartbeatRequest) -> Result<(), CoordError> {
        self.inner.heartbeat(r)
    }
    fn lease(&self) -> Result<Option<LeaseResponse>, CoordError> {
        self.inner.lease()
    }
    fn fail(&self, r: &LeaseFailRequest) -> Result<LeaseFailResponse, CoordError> {
        self.inner.fail(r)
    }
    fn renew(&self, r: &LeaseRenewRequest) -> Result<LeaseRenewResponse, CoordError> {
        self.inner.renew(r)
    }
    fn complete(&self, r: &CompleteRequest) -> Result<CompleteResponse, CoordError> {
        self.inner.complete(r)
    }
    fn get_blob(&self, p: &str, o: u64) -> Result<Box<dyn Read + Send>, CoordError> {
        self.inner.get_blob(p, o)
    }
    fn put_blob(&self, h: &str, b: &[u8]) -> Result<(), CoordError> {
        self.inner.put_blob(h, b)
    }
    fn upload_offset(&self, h: &str, n: u64) -> Result<u64, CoordError> {
        self.inner.upload_offset(h, n)
    }
    fn put_chunk(&self, h: &str, n: u64, k: u64, c: &[u8]) -> Result<ChunkReply, CoordError> {
        let r = self.inner.put_chunk(h, n, k, c);
        if self.after.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.probe.set_network(cellular());
        }
        r
    }
    fn submit(&self, r: &SubmitJobRequest) -> Result<SubmitJobResponse, CoordError> {
        self.inner.submit(r)
    }
    fn job_status(&self, id: &str) -> Result<JobStatusResponse, CoordError> {
        self.inner.job_status(id)
    }
}

#[test]
fn switch_to_cellular_mid_upload_stops_before_the_next_chunk() {
    let probe = Arc::new(FakeProbe::new(good_device(), home()));
    let c = SwitchAfter {
        inner: FakeCoord::default(),
        probe: probe.clone(),
        after: AtomicUsize::new(1),
    };
    let (h, b) = blob(3 * MIB);
    let gate = BudgetGate::new(&NetworkBudget::Metered(metered(1 << 30)), noon()).unwrap();
    let e = gated(&c, &gate, &probe).upload(&h, &b).unwrap_err();
    assert_eq!(e, UploadError::NotNow(Refusal::Window));
    assert_eq!(c.inner.s.lock().unwrap().chunk_bytes, MIB as u64);
    assert_eq!(probe.network().interface, cellular().interface);
}

#[test]
fn blob_hash_mismatch_restarts_once_then_gives_up() {
    let c = FakeCoord::default();
    let (h, b) = blob(MIB + 10);
    c.s.lock().unwrap().corrupt_final = true;
    assert_eq!(ungated(&c).upload(&h, &b).unwrap_err(), UploadError::Verify);
    // Two full passes from offset 0, nothing published.
    assert_eq!(chunk_offsets(&c), vec![0, MIB as u64, 0, MIB as u64]);
    assert!(c.s.lock().unwrap().puts.is_empty());
}

#[test]
fn local_bytes_that_do_not_match_the_hash_are_never_sent() {
    let c = FakeCoord::default();
    let (h, mut b) = blob(1000);
    b[0] ^= 1;
    assert_eq!(ungated(&c).upload(&h, &b).unwrap_err(), UploadError::Verify);
    assert_eq!(
        ungated(&c).upload("../x", &b).unwrap_err(),
        UploadError::Verify
    );
    assert!(c.calls().is_empty());
}

#[test]
fn empty_blob_uses_the_single_put() {
    let c = FakeCoord::default();
    let (h, b) = blob(0);
    ungated(&c).upload(&h, &b).unwrap();
    assert_eq!(c.calls(), vec![format!("put:{h}")]);
}

#[test]
fn stop_ends_the_upload_between_chunks() {
    let c = FakeCoord::default();
    let (h, b) = blob(3 * MIB);
    let stop = || c.s.lock().unwrap().chunk_bytes >= MIB as u64;
    let up = Uploader {
        stopped: &stop,
        ..ungated(&c)
    };
    assert_eq!(up.upload(&h, &b).unwrap_err(), UploadError::Stopped);
    assert_eq!(c.s.lock().unwrap().chunk_bytes, MIB as u64);
}
