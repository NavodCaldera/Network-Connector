//! Downloads with Range resume, tariff gating, per-chunk hash checks and the nc-blob cache.
//!
//! A download writes to a private `.part` file. If the connection drops it resumes from the bytes already on
//! disk with `Range: bytes={n}-`, re-authorising the remaining bytes with the budget gate each time. It stops
//! at the tariff window end, on a network change (e.g. Wi-Fi to cellular), on Stop, and when the server sends
//! more than was reserved. Inputs are then kept in the encrypted nc-blob cache keyed by their plaintext hash,
//! so a later lease of the same input needs no transfer.

use crate::budget::{BudgetGate, Grant, Refusal};
use crate::coord::{CoordError, Coordinator};
use crate::probe::{Clock, PlatformProbe};
use nc_blob::{BlobKey, BlobManifest, BlobStore, ChunkId, MAX_CHUNK_SIZE, MIN_CHUNK_SIZE};
use nc_protocol::pb::v1::TransferRef;
use nc_tariff::TrafficClass;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Read size per step; checks (Stop, window, network) run between steps.
const STEP: usize = 64 * 1024;
/// How often a dropped connection is resumed within one transfer.
const MAX_RESUMES: u32 = 3;

/// Why a transfer stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferError {
    /// Stop or consent withdrawal.
    Stopped,
    /// Window closed / not open / network changed.
    Window(&'static str),
    /// Not enough budget, or the server sent more than reserved.
    Budget,
    /// Hash or size mismatch.
    Verify,
    /// Coordinator refused or stayed unreachable.
    Coord(CoordError),
    /// Local disk/cache failure.
    Io,
}

/// Everything a transfer needs.
pub struct Ctx<'a> {
    /// Coordinator.
    pub coord: &'a dyn Coordinator,
    /// Budget gate.
    pub budget: &'a BudgetGate,
    /// Platform probe (network checks).
    pub probe: &'a dyn PlatformProbe,
    /// Clock.
    pub clock: &'a dyn Clock,
    /// Stop requested?
    pub stopped: &'a dyn Fn() -> bool,
    /// Private staging folder.
    pub staging: &'a Path,
}

fn refusal(r: Refusal) -> TransferError {
    match r {
        Refusal::Window => TransferError::Window("TARIFF_WINDOW"),
        Refusal::Budget => TransferError::Budget,
    }
}

fn part_path(staging: &Path, sha_hex: &str) -> PathBuf {
    staging.join(format!("{sha_hex}.part"))
}

fn same_network(g: &Grant, ctx: &Ctx<'_>) -> bool {
    let now = ctx.probe.network();
    now.interface == g.network.interface && now.connection_id == g.network.connection_id
}

/// Download one transfer (`r.size` bytes, `r.sha256` lowercase hex) and return the verified bytes.
pub fn download(
    ctx: &Ctx<'_>,
    r: &TransferRef,
    class: TrafficClass,
) -> Result<Vec<u8>, TransferError> {
    let part = part_path(ctx.staging, &r.sha256);
    let mut have = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    if have > r.size {
        let _ = std::fs::remove_file(&part);
        have = 0;
    }
    let mut resumes = 0;
    while have < r.size {
        if (ctx.stopped)() {
            return Err(TransferError::Stopped);
        }
        let remaining = r.size - have;
        let grant = ctx
            .budget
            .authorize(class, remaining, ctx.clock.now(), &ctx.probe.network())
            .map_err(refusal)?;
        let (moved, result) = stream_into(ctx, r, &part, have, &grant);
        ctx.budget.settle(grant, moved);
        have += moved;
        match result {
            Ok(()) => {}
            Err(TransferError::Coord(e)) if e.is_transient() && resumes < MAX_RESUMES => {
                resumes += 1;
                tracing::info!(
                    resumed_from = have,
                    "download interrupted; resuming with Range"
                );
            }
            Err(e) => return Err(e),
        }
    }
    let bytes = std::fs::read(&part).map_err(|_| TransferError::Io)?;
    let _ = std::fs::remove_file(&part);
    if bytes.len() as u64 != r.size || hex::encode(Sha256::digest(&bytes)) != r.sha256 {
        return Err(TransferError::Verify);
    }
    Ok(bytes)
}

/// Stream from `offset` into `part`. Returns the bytes written and how the stream ended.
fn stream_into(
    ctx: &Ctx<'_>,
    r: &TransferRef,
    part: &Path,
    offset: u64,
    grant: &Grant,
) -> (u64, Result<(), TransferError>) {
    let mut reader = match ctx.coord.get_blob(&r.path, offset) {
        Ok(rd) => rd,
        Err(e) => return (0, Err(TransferError::Coord(e))),
    };
    let mut file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(part)
    {
        Ok(f) => f,
        Err(_) => return (0, Err(TransferError::Io)),
    };
    let want = r.size - offset;
    let mut moved = 0u64;
    let mut buf = vec![0u8; STEP];
    while moved < want {
        if (ctx.stopped)() {
            return (moved, Err(TransferError::Stopped));
        }
        if grant.until.is_some_and(|u| ctx.clock.now() >= u) {
            return (moved, Err(TransferError::Window("WINDOW_ENDED")));
        }
        if !same_network(grant, ctx) {
            return (moved, Err(TransferError::Window("NETWORK_CHANGED")));
        }
        let n = match reader.read(&mut buf) {
            Ok(0) => {
                return (
                    moved,
                    Err(TransferError::Coord(CoordError::Network("eof".into()))),
                )
            }
            Ok(n) => n,
            Err(_) => {
                return (
                    moved,
                    Err(TransferError::Coord(CoordError::Network("io".into()))),
                )
            }
        };
        // Never accept (or pay for) more than reserved.
        let n = (n as u64)
            .min(want - moved)
            .min(grant.bytes - moved.min(grant.bytes));
        if n == 0 {
            return (moved, Err(TransferError::Budget));
        }
        if file
            .write_all(&buf[..usize::try_from(n).unwrap_or(0)])
            .is_err()
        {
            return (moved, Err(TransferError::Io));
        }
        moved += n;
    }
    if file.flush().is_err() {
        return (moved, Err(TransferError::Io));
    }
    // If the server keeps sending past the declared size, that is a protocol violation; we stop reading.
    (moved, Ok(()))
}

/// Encrypted local cache of verified inputs (nc-blob). The key lives only in memory, so a restart starts
/// with an unreadable cache, which is swept.
pub struct BlobCache {
    store: BlobStore,
    key: BlobKey,
    known: HashMap<[u8; 32], BlobManifest>,
}

impl std::fmt::Debug for BlobCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlobCache").finish_non_exhaustive()
    }
}

/// One-chunk manifest for an input of `size` bytes with plaintext hash `sha`.
fn single_chunk(sha: [u8; 32], size: u64) -> Option<BlobManifest> {
    let chunk = u32::try_from(size)
        .ok()?
        .clamp(MIN_CHUNK_SIZE, MAX_CHUNK_SIZE);
    if size > u64::from(MAX_CHUNK_SIZE) || size == 0 {
        return None;
    }
    let m = BlobManifest {
        version: 1,
        total_len: size,
        chunk_size: chunk,
        chunk_ids: vec![ChunkId(sha)],
    };
    m.validate().ok()?;
    Some(m)
}

impl BlobCache {
    /// Open the cache at `dir` with a fresh random key and sweep anything left from earlier runs.
    pub fn open(dir: &Path, quota_bytes: u64) -> Result<Self, TransferError> {
        let mut store = BlobStore::open(dir, quota_bytes).map_err(|_| TransferError::Io)?;
        let _ = store.gc(&[]);
        let mut k = [0u8; 32];
        getrandom::fill(&mut k).map_err(|_| TransferError::Io)?;
        Ok(Self {
            store,
            key: BlobKey::new(k),
            known: HashMap::new(),
        })
    }

    /// Cached plaintext for (`sha`, `size`), hash-checked by nc-blob.
    pub fn get(&self, sha: [u8; 32], size: u64) -> Option<Vec<u8>> {
        let m = self.known.get(&sha)?;
        if m.total_len != size {
            return None;
        }
        self.store.read_chunk(m, 0, &self.key).ok()
    }

    /// Store verified plaintext. nc-blob re-checks the chunk hash.
    pub fn put(&mut self, sha: [u8; 32], bytes: &[u8]) -> Result<(), TransferError> {
        let m = single_chunk(sha, bytes.len() as u64).ok_or(TransferError::Verify)?;
        match self.store.put_chunk(&m, 0, bytes, &self.key) {
            Ok(()) => {
                self.known.insert(sha, m);
                Ok(())
            }
            Err(nc_blob::Error::HashMismatch) => Err(TransferError::Verify),
            // Quota full: the job can still run from memory; just do not cache.
            Err(_) => Ok(()),
        }
    }
}

/// Remove leftover `.part` files (start-up sweep).
pub fn sweep_staging(staging: &Path) {
    if let Ok(rd) = std::fs::read_dir(staging) {
        for e in rd.flatten() {
            if e.path().extension().is_some_and(|x| x == "part") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// Read everything from a reader with a hard cap (used by tests and the requester).
pub fn read_capped(mut r: impl Read, cap: u64) -> Result<Vec<u8>, TransferError> {
    let mut out = Vec::new();
    r.by_ref()
        .take(cap + 1)
        .read_to_end(&mut out)
        .map_err(|_| TransferError::Io)?;
    if out.len() as u64 > cap {
        return Err(TransferError::Budget);
    }
    Ok(out)
}
