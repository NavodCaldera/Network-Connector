//! Finished results kept across a closed upload window (laptop-gap G4, task 1.L3; PLAN.md §6).
//!
//! When a job finishes but the tariff profile does not allow the result upload now (window closed, or the
//! period budget cannot cover it), the output is *parked* here instead of being thrown away: encrypted with
//! nc-blob (XChaCha20-Poly1305 per chunk, hash-checked on read) under a key derived from the device key, so a
//! restart can still read it. The index file holds ids, hashes and sizes only, never job data.
//!
//! Owner limits: the store has a hard byte quota (the owner's cache quota), [`ParkedResults::park`] also refuses
//! anything that would take parked bytes over the owner's disk cap, and the parked bytes are reported as disk in
//! use to the policy engine, so new work is admitted only within what is left. Nothing here moves network bytes;
//! the upload itself is still gated by the budget, so the night budget is never exceeded.

use nc_blob::{BlobId, BlobKey, BlobManifest, BlobStore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Parked results older than this are dropped (the coordinator has long since given the task to someone else).
pub const PARKED_TTL_MS: i64 = 48 * 3600 * 1000;

/// Most results parked at once.
pub const MAX_PARKED: usize = 16;

/// Why a result could not be parked or read.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParkError {
    /// Over the owner's quota, disk cap or count limit.
    #[error("no room to keep the result within the owner's limits")]
    Full,
    /// Disk or encoding failure.
    #[error("parked result I/O failed")]
    Io,
}

/// One parked result (ids, hashes and sizes only).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Parked {
    /// Coordinator task id.
    pub task_id: String,
    /// SHA-256 of the signed job manifest the result was computed for (hex).
    pub job_digest: String,
    /// Lease the result belongs to right now.
    pub lease_id: String,
    /// Attempt the result belongs to right now.
    pub attempt_id: String,
    /// Output SHA-256 (hex).
    pub sha256: String,
    /// Output size in bytes.
    pub size: u64,
    /// When it was parked (Unix ms).
    pub parked_at_ms: i64,
    /// nc-blob manifest of the encrypted copy (`None` for an empty output).
    manifest: Option<BlobManifest>,
}

#[derive(Serialize, Deserialize, Default)]
struct IndexFile {
    version: u32,
    items: Vec<Parked>,
}

/// The parked-result store.
pub struct ParkedResults {
    store: BlobStore,
    key: BlobKey,
    index_path: PathBuf,
    items: BTreeMap<String, Parked>,
}

impl std::fmt::Debug for ParkedResults {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParkedResults")
            .field("parked", &self.items.len())
            .finish_non_exhaustive()
    }
}

/// Derive the parked-result key from a secret only the device holds (see `AgentConfig::backend_with`).
pub fn derive_key(secret: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"nc-agent/parked-results/key/v1");
    h.update(secret);
    h.finalize().into()
}

impl ParkedResults {
    /// Open (or create) the store in `dir` with a byte quota and a 32-byte key. Blobs that the index does not
    /// reference are removed.
    pub fn open(dir: &Path, quota_bytes: u64, key: [u8; 32]) -> Result<Self, ParkError> {
        std::fs::create_dir_all(dir).map_err(|_| ParkError::Io)?;
        let mut store =
            BlobStore::open(dir.join("blobs"), quota_bytes).map_err(|_| ParkError::Io)?;
        let index_path = dir.join("parked.index.json");
        let items: BTreeMap<String, Parked> = match std::fs::read(&index_path) {
            Ok(b) => serde_json::from_slice::<IndexFile>(&b)
                .map(|f| {
                    f.items
                        .into_iter()
                        .map(|p| (p.task_id.clone(), p))
                        .collect()
                })
                // An unreadable index loses the parked results, not the agent.
                .unwrap_or_default(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(_) => return Err(ParkError::Io),
        };
        let keep: Vec<BlobId> = items
            .values()
            .filter_map(|p| p.manifest.as_ref().and_then(|m| m.id().ok()))
            .collect();
        let _ = store.gc(&keep);
        let mut me = Self {
            store,
            key: BlobKey::new(key),
            index_path,
            items,
        };
        // Drop entries whose bytes are gone or unreadable with this key.
        let bad: Vec<String> = me
            .items
            .keys()
            .filter(|t| me.read(t).is_none())
            .cloned()
            .collect();
        for t in bad {
            me.remove(&t);
        }
        Ok(me)
    }

    fn save(&self) -> Result<(), ParkError> {
        let f = IndexFile {
            version: 1,
            items: self.items.values().cloned().collect(),
        };
        let bytes = serde_json::to_vec(&f).map_err(|_| ParkError::Io)?;
        let tmp = self.index_path.with_extension("tmp");
        std::fs::write(&tmp, bytes).map_err(|_| ParkError::Io)?;
        std::fs::rename(&tmp, &self.index_path).map_err(|_| ParkError::Io)
    }

    /// Bytes parked now (plaintext sizes).
    pub fn bytes(&self) -> u64 {
        self.items.values().map(|p| p.size).sum()
    }

    /// Number of parked results.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Nothing parked.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Keep `output` for the task in `meta`. `disk_cap_bytes` is the owner's disk cap; parked bytes never go
    /// over it. Replaces an earlier parked result for the same task.
    #[allow(clippy::too_many_arguments)]
    pub fn park(
        &mut self,
        task_id: &str,
        job_digest: &str,
        lease_id: &str,
        attempt_id: &str,
        output: &[u8],
        disk_cap_bytes: u64,
        now_ms: i64,
    ) -> Result<(), ParkError> {
        self.remove(task_id);
        let size = output.len() as u64;
        if self.items.len() >= MAX_PARKED || self.bytes().saturating_add(size) > disk_cap_bytes {
            return Err(ParkError::Full);
        }
        let manifest = if output.is_empty() {
            None
        } else {
            let m = BlobManifest::with_default_chunks(output).map_err(|_| ParkError::Full)?;
            let chunk = m.chunk_size as usize;
            for (i, part) in output.chunks(chunk).enumerate() {
                if let Err(e) = self.store.put_chunk(&m, i, part, &self.key) {
                    if let Ok(id) = m.id() {
                        let _ = self.store.remove_blob(id);
                    }
                    return Err(match e {
                        nc_blob::Error::Quota => ParkError::Full,
                        _ => ParkError::Io,
                    });
                }
            }
            Some(m)
        };
        let sha: [u8; 32] = Sha256::digest(output).into();
        self.items.insert(
            task_id.to_string(),
            Parked {
                task_id: task_id.to_string(),
                job_digest: job_digest.to_string(),
                lease_id: lease_id.to_string(),
                attempt_id: attempt_id.to_string(),
                sha256: hex::encode(sha),
                size,
                parked_at_ms: now_ms,
                manifest,
            },
        );
        if self.save().is_err() {
            self.remove(task_id);
            return Err(ParkError::Io);
        }
        Ok(())
    }

    /// The parked result for `task_id`, if any.
    pub fn get(&self, task_id: &str) -> Option<&Parked> {
        self.items.get(task_id)
    }

    /// The parked result currently bound to `attempt_id`, if any.
    pub fn by_attempt(&self, attempt_id: &str) -> Option<&Parked> {
        self.items.values().find(|p| p.attempt_id == attempt_id)
    }

    /// Decrypt and hash-check the parked output for `task_id`.
    pub fn read(&self, task_id: &str) -> Option<Vec<u8>> {
        let p = self.items.get(task_id)?;
        let out = match &p.manifest {
            None => Vec::new(),
            Some(m) => {
                let mut out = Vec::with_capacity(usize::try_from(p.size).ok()?);
                for i in 0..m.chunk_ids.len() {
                    out.extend(self.store.read_chunk(m, i, &self.key).ok()?);
                }
                out
            }
        };
        let sha: [u8; 32] = Sha256::digest(&out).into();
        (out.len() as u64 == p.size && hex::encode(sha) == p.sha256).then_some(out)
    }

    /// Bind the parked result for `task_id` to a new lease (a later lease for the same task).
    pub fn rebind(
        &mut self,
        task_id: &str,
        lease_id: &str,
        attempt_id: &str,
    ) -> Result<(), ParkError> {
        let p = self.items.get_mut(task_id).ok_or(ParkError::Io)?;
        p.lease_id = lease_id.to_string();
        p.attempt_id = attempt_id.to_string();
        self.save()
    }

    /// Forget the parked result for `task_id` and delete its bytes.
    pub fn remove(&mut self, task_id: &str) {
        if let Some(p) = self.items.remove(task_id) {
            if let Some(id) = p.manifest.and_then(|m| m.id().ok()) {
                // The same output may be parked for another task; keep shared bytes.
                let shared = self
                    .items
                    .values()
                    .any(|o| o.manifest.as_ref().and_then(|m| m.id().ok()) == Some(id));
                if !shared {
                    let _ = self.store.remove_blob(id);
                }
            }
            let _ = self.save();
        }
    }

    /// Drop results parked longer than [`PARKED_TTL_MS`].
    pub fn sweep(&mut self, now_ms: i64) {
        let old: Vec<String> = self
            .items
            .values()
            .filter(|p| now_ms.saturating_sub(p.parked_at_ms) > PARKED_TTL_MS)
            .map(|p| p.task_id.clone())
            .collect();
        for t in old {
            self.remove(&t);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const MIB: u64 = 1024 * 1024;

    fn open(dir: &Path, key: u8) -> ParkedResults {
        ParkedResults::open(dir, 64 * MIB, [key; 32]).unwrap()
    }

    #[test]
    fn survives_reopen_with_the_same_key() {
        let d = tempfile::tempdir().unwrap();
        let out = vec![5u8; 3 * MIB as usize + 17];
        let mut p = open(d.path(), 1);
        p.park("t1", "j", "l", "a", &out, 64 * MIB, 10).unwrap();
        p.park("t2", "j", "l2", "a2", b"", 64 * MIB, 10).unwrap();
        drop(p);
        let p = open(d.path(), 1);
        assert_eq!(p.read("t1").unwrap(), out);
        assert_eq!(p.read("t2").unwrap(), b"");
        assert_eq!(p.by_attempt("a").unwrap().task_id, "t1");
        assert_eq!(p.bytes(), out.len() as u64);
    }

    #[test]
    fn a_different_key_cannot_read_and_the_entry_is_dropped() {
        let d = tempfile::tempdir().unwrap();
        let mut p = open(d.path(), 1);
        p.park("t1", "j", "l", "a", b"secret result", 64 * MIB, 10)
            .unwrap();
        drop(p);
        let p = open(d.path(), 2);
        assert!(p.read("t1").is_none());
        assert!(p.is_empty());
    }

    #[test]
    fn bytes_on_disk_are_not_the_plaintext() {
        let d = tempfile::tempdir().unwrap();
        let mut p = open(d.path(), 1);
        let marker = b"PLAINTEXT-MARKER-0123456789".repeat(100);
        p.park("t1", "j", "l", "a", &marker, 64 * MIB, 10).unwrap();
        drop(p); // releases the store lock file
        fn walk(dir: &Path, needle: &[u8]) -> bool {
            std::fs::read_dir(dir).unwrap().flatten().any(|e| {
                let path = e.path();
                if path.is_dir() {
                    walk(&path, needle)
                } else {
                    let b = std::fs::read(&path).unwrap();
                    b.windows(needle.len()).any(|w| w == needle)
                }
            })
        }
        assert!(!walk(d.path(), b"PLAINTEXT-MARKER"));
    }

    #[test]
    fn owner_disk_cap_quota_and_count_are_enforced() {
        let d = tempfile::tempdir().unwrap();
        let mut p = open(d.path(), 1);
        p.park("t1", "j", "l", "a", &[1u8; 1000], 1500, 10).unwrap();
        assert_eq!(
            p.park("t2", "j", "l", "a", &[1u8; 600], 1500, 10),
            Err(ParkError::Full)
        );
        assert!(p.get("t2").is_none());
        // Store quota (ciphertext) smaller than the output.
        let d2 = tempfile::tempdir().unwrap();
        let mut small = ParkedResults::open(d2.path(), 1024, [1; 32]).unwrap();
        assert_eq!(
            small.park("t", "j", "l", "a", &[1u8; 4096], u64::MAX, 10),
            Err(ParkError::Full)
        );
        assert_eq!(small.bytes(), 0);
        // Count limit.
        let d3 = tempfile::tempdir().unwrap();
        let mut many = open(d3.path(), 1);
        for i in 0..MAX_PARKED {
            many.park(&format!("t{i}"), "j", "l", "a", b"x", u64::MAX, 10)
                .unwrap();
        }
        assert_eq!(
            many.park("over", "j", "l", "a", b"x", u64::MAX, 10),
            Err(ParkError::Full)
        );
    }

    #[test]
    fn sweep_drops_old_results_and_rebind_moves_the_lease() {
        let d = tempfile::tempdir().unwrap();
        let mut p = open(d.path(), 1);
        p.park("old", "j", "l", "a", b"1", u64::MAX, 0).unwrap();
        p.park("new", "j", "l", "b", b"2", u64::MAX, PARKED_TTL_MS)
            .unwrap();
        p.rebind("new", "l9", "b9").unwrap();
        p.sweep(PARKED_TTL_MS + 1);
        assert!(p.get("old").is_none());
        assert_eq!(p.by_attempt("b9").unwrap().lease_id, "l9");
        drop(p);
        assert_eq!(open(d.path(), 1).by_attempt("b9").unwrap().task_id, "new");
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(24))]
        /// Whatever sequence of park/remove, parked bytes never exceed the cap and every entry reads back.
        #[test]
        fn never_over_the_cap(ops in proptest::collection::vec((0u8..6, 0usize..3000, any::<bool>()), 1..20)) {
            let d = tempfile::tempdir().unwrap();
            let mut p = open(d.path(), 3);
            let cap = 5000;
            for (t, n, park) in ops {
                let task = format!("t{t}");
                if park {
                    let _ = p.park(&task, "j", "l", "a", &vec![t; n], cap, 1);
                } else {
                    p.remove(&task);
                }
                prop_assert!(p.bytes() <= cap);
            }
            drop(p);
            let p = open(d.path(), 3);
            prop_assert!(p.bytes() <= cap);
            for t in 0u8..6 {
                if let Some(e) = p.get(&format!("t{t}")) {
                    prop_assert_eq!(p.read(&e.task_id).unwrap(), vec![t; e.size as usize]);
                }
            }
        }
    }
}
