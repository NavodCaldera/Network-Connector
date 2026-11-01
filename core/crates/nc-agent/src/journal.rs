//! Crash-safe record of coordinator leases in nc-store (AGENT.md §1 rule 8).
//!
//! Each coordinator attempt is one nc-store task keyed by the coordinator attempt id. The local state machine
//! (`queued → leased → running → uploaded → verified → committed`) is advanced as the helper works, so after a
//! crash the agent knows, per attempt, whether to report a failure (never ran or ran without a result) or to
//! resend the same Complete (result uploaded). nc-store's attempt-checked, idempotent commit means a resent
//! Complete is counted once.
//!
//! nc-store has no "list unfinished" query yet, so a small index file (attempt ids only, no job data) lists
//! the attempts to look at after a restart.

use nc_store::{CommitOutcome, RetryPolicy, Store, StoreError, TaskState};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Local lease length. The coordinator lease is the authority; this only bounds a stuck local record.
const LOCAL_LEASE_MS: i64 = 6 * 3600 * 1000;
const WORKER: &[u8] = b"self";

/// Journal failure.
#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    /// nc-store refused or failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Index file I/O.
    #[error("journal index I/O failed")]
    Io,
    /// The ids in the lease are not UUIDs, or the store handed back another task.
    #[error("journal record is inconsistent")]
    Inconsistent,
}

/// What we keep per attempt (ids and output reference only; never job data).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Coordinator lease id.
    pub lease_id: String,
    /// Coordinator attempt id.
    pub attempt_id: String,
    /// Coordinator task id.
    pub task_id: String,
    /// Output hash once uploaded.
    #[serde(default)]
    pub output_sha256: Option<String>,
    /// Output size once uploaded.
    #[serde(default)]
    pub output_size: Option<u64>,
}

/// Handle to one recorded attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Local {
    key: [u8; 16],
    attempt: [u8; 16],
}

/// The journal.
pub struct Journal {
    store: Store,
    index_path: PathBuf,
    index: BTreeMap<String, (Entry, [u8; 16])>,
}

impl std::fmt::Debug for Journal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Journal")
            .field("open", &self.index.len())
            .finish_non_exhaustive()
    }
}

#[derive(Serialize, Deserialize, Default)]
struct IndexFile {
    version: u32,
    entries: Vec<(Entry, String)>,
}

fn uuid_bytes(s: &str) -> Result<[u8; 16], JournalError> {
    uuid::Uuid::parse_str(s)
        .map(|u| *u.as_bytes())
        .map_err(|_| JournalError::Inconsistent)
}

impl Journal {
    /// Open (or create) the journal in `dir`.
    pub fn open(dir: &Path) -> Result<Self, JournalError> {
        std::fs::create_dir_all(dir).map_err(|_| JournalError::Io)?;
        let store = Store::open(&dir.join("leases.sqlite"))?;
        Self::with_store(store, dir.join("leases.index.json"))
    }

    /// Journal over an existing store (tests use an in-memory store).
    pub fn with_store(store: Store, index_path: PathBuf) -> Result<Self, JournalError> {
        let index = match std::fs::read(&index_path) {
            Ok(b) => {
                let f: IndexFile = serde_json::from_slice(&b).map_err(|_| JournalError::Io)?;
                let mut m = BTreeMap::new();
                for (e, local_attempt) in f.entries {
                    let a: [u8; 16] = hex::decode(&local_attempt)
                        .ok()
                        .and_then(|v| v.try_into().ok())
                        .ok_or(JournalError::Inconsistent)?;
                    m.insert(e.attempt_id.clone(), (e, a));
                }
                m
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(_) => return Err(JournalError::Io),
        };
        Ok(Self {
            store,
            index_path,
            index,
        })
    }

    fn save(&self) -> Result<(), JournalError> {
        let f = IndexFile {
            version: 1,
            entries: self
                .index
                .values()
                .map(|(e, a)| (e.clone(), hex::encode(a)))
                .collect(),
        };
        let bytes = serde_json::to_vec(&f).map_err(|_| JournalError::Io)?;
        let tmp = self.index_path.with_extension("tmp");
        std::fs::write(&tmp, bytes).map_err(|_| JournalError::Io)?;
        std::fs::rename(&tmp, &self.index_path).map_err(|_| JournalError::Io)
    }

    /// Record a new coordinator lease. Must happen before any transfer or execution.
    pub fn begin(&mut self, entry: Entry, now_ms: i64) -> Result<Local, JournalError> {
        let key = uuid_bytes(&entry.attempt_id)?;
        uuid_bytes(&entry.lease_id)?;
        let record = serde_json::to_vec(&entry).map_err(|_| JournalError::Io)?;
        self.store.enqueue(
            &key,
            &record,
            RetryPolicy {
                max_attempts: 1,
                base_backoff_ms: 0,
                max_backoff_ms: 0,
            },
            now_ms,
        )?;
        let lease = self
            .store
            .lease_next(WORKER, LOCAL_LEASE_MS, now_ms)?
            .ok_or(JournalError::Inconsistent)?;
        if lease.task_id != key {
            return Err(JournalError::Inconsistent);
        }
        self.index
            .insert(entry.attempt_id.clone(), (entry, lease.attempt_id));
        self.save()?;
        Ok(Local {
            key,
            attempt: lease.attempt_id,
        })
    }

    /// The job is about to run.
    pub fn start(&mut self, l: Local, now_ms: i64) -> Result<(), JournalError> {
        Ok(self.store.start(&l.key, &l.attempt, now_ms)?)
    }

    /// The output is uploaded to the coordinator; a Complete is about to be sent.
    pub fn uploaded(
        &mut self,
        l: Local,
        sha256: [u8; 32],
        size: u64,
        now_ms: i64,
    ) -> Result<(), JournalError> {
        self.store.upload(&l.key, &l.attempt, &sha256, now_ms)?;
        let attempt = self.attempt_hex(l)?;
        if let Some((e, _)) = self.index.get_mut(&attempt) {
            e.output_sha256 = Some(hex::encode(sha256));
            e.output_size = Some(size);
        }
        self.save()
    }

    /// The coordinator accepted the Complete. Idempotent: a second call returns `AlreadyCommitted`.
    pub fn completed(&mut self, l: Local, now_ms: i64) -> Result<CommitOutcome, JournalError> {
        let view = self.store.task(&l.key)?;
        if view.state == TaskState::Uploaded {
            self.store.verify(&l.key, &l.attempt, true, now_ms)?;
        }
        let out = self.store.commit(&l.key, &l.attempt, now_ms)?;
        self.forget(l)?;
        Ok(out)
    }

    /// The attempt ended without a committed result (reported or not).
    pub fn failed(&mut self, l: Local, now_ms: i64) -> Result<(), JournalError> {
        match self.store.fail(&l.key, &l.attempt, now_ms) {
            Ok(()) | Err(StoreError::WrongState(_) | StoreError::LeaseExpired) => {}
            Err(e) => return Err(e.into()),
        }
        self.forget(l)
    }

    fn attempt_hex(&self, l: Local) -> Result<String, JournalError> {
        self.index
            .iter()
            .find(|(_, (_, a))| *a == l.attempt)
            .map(|(k, _)| k.clone())
            .ok_or(JournalError::Inconsistent)
    }

    fn forget(&mut self, l: Local) -> Result<(), JournalError> {
        if let Ok(k) = self.attempt_hex(l) {
            self.index.remove(&k);
        }
        self.save()
    }

    /// Attempts left unfinished by a previous run (or by a Complete that could not be sent yet).
    pub fn pending(&mut self) -> Result<Vec<(Entry, Local, TaskState)>, JournalError> {
        let mut out = Vec::new();
        let items: Vec<_> = self.index.values().cloned().collect();
        for (e, attempt) in items {
            let key = uuid_bytes(&e.attempt_id)?;
            let state = self.store.task(&key)?.state;
            out.push((e, Local { key, attempt }, state));
        }
        Ok(out)
    }

    /// Number of open records.
    pub fn open_count(&self) -> usize {
        self.index.len()
    }

    /// Local state of an attempt (tests and status).
    pub fn state(&mut self, l: Local) -> Result<TaskState, JournalError> {
        Ok(self.store.task(&l.key)?.state)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn entry(n: u8) -> Entry {
        let id = |b: u8| uuid::Uuid::from_bytes([b; 16]).to_string();
        Entry {
            lease_id: id(n),
            attempt_id: id(n.wrapping_add(100)),
            task_id: id(n.wrapping_add(200)),
            output_sha256: None,
            output_size: None,
        }
    }

    #[test]
    fn survives_reopen_and_commits_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut j = Journal::open(dir.path()).unwrap();
        let l = j.begin(entry(1), 1000).unwrap();
        j.start(l, 1001).unwrap();
        j.uploaded(l, [7; 32], 3, 1002).unwrap();
        drop(j);
        let mut j = Journal::open(dir.path()).unwrap();
        let p = j.pending().unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].2, TaskState::Uploaded);
        assert_eq!(p[0].0.output_size, Some(3));
        assert_eq!(j.completed(p[0].1, 1003).unwrap(), CommitOutcome::Committed);
        assert_eq!(j.open_count(), 0);
        // A second commit of the same attempt changes nothing.
        assert_eq!(
            j.store.commit(&p[0].1.key, &p[0].1.attempt, 1004).unwrap(),
            CommitOutcome::AlreadyCommitted
        );
    }

    #[test]
    fn same_attempt_cannot_begin_twice() {
        let dir = tempfile::tempdir().unwrap();
        let mut j = Journal::open(dir.path()).unwrap();
        j.begin(entry(1), 1000).unwrap();
        assert!(j.begin(entry(1), 1001).is_err());
    }

    proptest! {
        /// Whatever order of crash points, after recovery every attempt ends committed exactly once or failed,
        /// and nothing stays open.
        #[test]
        fn recovery_terminates_every_attempt(stages in proptest::collection::vec(0u8..4, 1..8)) {
            let dir = tempfile::tempdir().unwrap();
            let mut j = Journal::open(dir.path()).unwrap();
            for (i, s) in stages.iter().enumerate() {
                let l = j.begin(entry(i as u8), 10).unwrap();
                if *s >= 1 { j.start(l, 11).unwrap(); }
                if *s >= 2 { j.uploaded(l, [i as u8; 32], 1, 12).unwrap(); }
                if *s >= 3 { j.completed(l, 13).unwrap(); }
            }
            drop(j);
            let mut j = Journal::open(dir.path()).unwrap();
            let mut committed = 0;
            for (_, l, state) in j.pending().unwrap() {
                if state == TaskState::Uploaded {
                    prop_assert_eq!(j.completed(l, 20).unwrap(), CommitOutcome::Committed);
                    committed += 1;
                } else {
                    j.failed(l, 20).unwrap();
                }
            }
            prop_assert_eq!(j.open_count(), 0);
            prop_assert_eq!(committed, stages.iter().filter(|s| **s == 2).count());
        }
    }
}
