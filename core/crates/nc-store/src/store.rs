//! The lease state machine over SQLite.

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};

use crate::migrations;

/// Task id (16 bytes, from the manifest).
pub type TaskId = [u8; 16];
/// Attempt id (16 random bytes, unique per lease).
pub type AttemptId = [u8; 16];

/// Task states (PLAN.md §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskState {
    /// Waiting for a lease (possibly after backoff).
    Queued,
    /// Leased to a worker, not started.
    Leased,
    /// Running on the worker.
    Running,
    /// Result uploaded, awaiting verification.
    Uploaded,
    /// Result verified, awaiting commit.
    Verified,
    /// Done. Terminal.
    Committed,
    /// Out of attempts. Terminal.
    Failed,
}

impl TaskState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Leased => "leased",
            Self::Running => "running",
            Self::Uploaded => "uploaded",
            Self::Verified => "verified",
            Self::Committed => "committed",
            Self::Failed => "failed",
        }
    }

    fn parse(s: &str) -> Result<Self, StoreError> {
        Ok(match s {
            "queued" => Self::Queued,
            "leased" => Self::Leased,
            "running" => Self::Running,
            "uploaded" => Self::Uploaded,
            "verified" => Self::Verified,
            "committed" => Self::Committed,
            "failed" => Self::Failed,
            _ => return Err(StoreError::Corrupt),
        })
    }

    /// Whether the task holds a live lease that can expire.
    pub fn is_leased(self) -> bool {
        matches!(self, Self::Leased | Self::Running)
    }
}

/// Retry rules for one task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Attempts allowed in total (≥ 1).
    pub max_attempts: u32,
    /// Backoff after the first failed attempt; doubles each time.
    pub base_backoff_ms: i64,
    /// Backoff ceiling.
    pub max_backoff_ms: i64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            base_backoff_ms: 1_000,
            max_backoff_ms: 10 * 60 * 1_000,
        }
    }
}

impl RetryPolicy {
    /// Delay before the next lease after `attempts` attempts have ended.
    pub fn backoff_ms(&self, attempts: u32) -> i64 {
        let shift = attempts.saturating_sub(1).min(30);
        self.base_backoff_ms
            .saturating_mul(1i64 << shift)
            .min(self.max_backoff_ms)
    }
}

/// A granted lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lease {
    /// Task.
    pub task_id: TaskId,
    /// Fresh attempt id; every later call must present it.
    pub attempt_id: AttemptId,
    /// 1-based attempt number.
    pub attempt_no: u32,
    /// Lease expiry (Unix ms).
    pub expires_ms: i64,
}

/// Result of [`Store::commit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitOutcome {
    /// This call committed the task.
    Committed,
    /// The task was already committed by this same attempt; nothing changed.
    AlreadyCommitted,
}

/// Snapshot of one task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskView {
    /// State.
    pub state: TaskState,
    /// Attempts started so far.
    pub attempts: u32,
    /// Attempts allowed.
    pub max_attempts: u32,
    /// Current attempt (set while leased/running/uploaded/verified).
    pub current_attempt: Option<AttemptId>,
    /// Lease expiry while leased/running.
    pub lease_expires_ms: Option<i64>,
    /// Earliest time the task may be leased again.
    pub next_eligible_ms: i64,
    /// Result hash once uploaded.
    pub result_sha256: Option<[u8; 32]>,
    /// Attempt that committed the task.
    pub committed_attempt: Option<AttemptId>,
}

/// Store errors. Rejections of stale or late work are errors by design: the caller must drop that work.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// SQLite failure.
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// Database written by a newer build.
    #[error("database schema version {0} is newer than this build")]
    SchemaTooNew(u32),
    /// A row did not match the schema's invariants.
    #[error("database row is corrupt")]
    Corrupt,
    /// No such task.
    #[error("unknown task")]
    UnknownTask,
    /// Task already exists.
    #[error("task already exists")]
    Duplicate,
    /// The attempt is not the task's current attempt (superseded, expired earlier, or never existed).
    #[error("stale attempt")]
    StaleAttempt,
    /// The attempt's lease had expired; the task has been re-queued or failed.
    #[error("lease expired")]
    LeaseExpired,
    /// The transition is not allowed from the task's state.
    #[error("not allowed in state {0:?}")]
    WrongState(TaskState),
    /// Bad argument.
    #[error("invalid argument: {0}")]
    Invalid(&'static str),
    /// No system randomness for attempt ids.
    #[error("no system randomness")]
    Random,
}

/// The device task store.
pub struct Store {
    conn: Connection,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").finish_non_exhaustive()
    }
}

struct Row {
    state: TaskState,
    attempts: u32,
    max_attempts: u32,
    base_backoff_ms: i64,
    max_backoff_ms: i64,
    current_attempt: Option<AttemptId>,
    lease_expires_ms: Option<i64>,
    next_eligible_ms: i64,
    result_sha256: Option<[u8; 32]>,
    committed_attempt: Option<AttemptId>,
}

fn fixed<const N: usize>(v: Option<Vec<u8>>) -> Result<Option<[u8; N]>, StoreError> {
    v.map(|b| b.as_slice().try_into().map_err(|_| StoreError::Corrupt))
        .transpose()
}

fn load(tx: &Transaction<'_>, task: &TaskId) -> Result<Row, StoreError> {
    let raw = tx
        .query_row(
            "SELECT state, attempts, max_attempts, base_backoff_ms, max_backoff_ms, current_attempt,
                    lease_expires_ms, next_eligible_ms, result_sha256, committed_attempt
             FROM tasks WHERE task_id = ?1",
            [task.as_slice()],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, u32>(1)?,
                    r.get::<_, u32>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, Option<Vec<u8>>>(5)?,
                    r.get::<_, Option<i64>>(6)?,
                    r.get::<_, i64>(7)?,
                    r.get::<_, Option<Vec<u8>>>(8)?,
                    r.get::<_, Option<Vec<u8>>>(9)?,
                ))
            },
        )
        .optional()?
        .ok_or(StoreError::UnknownTask)?;
    Ok(Row {
        state: TaskState::parse(&raw.0)?,
        attempts: raw.1,
        max_attempts: raw.2,
        base_backoff_ms: raw.3,
        max_backoff_ms: raw.4,
        current_attempt: fixed(raw.5)?,
        lease_expires_ms: raw.6,
        next_eligible_ms: raw.7,
        result_sha256: fixed(raw.8)?,
        committed_attempt: fixed(raw.9)?,
    })
}

/// End the current attempt with `attempt_state` and re-queue with backoff, or fail for good.
fn end_attempt(
    tx: &Transaction<'_>,
    task: &TaskId,
    row: &Row,
    attempt_state: &str,
    now: i64,
) -> Result<(), StoreError> {
    if let Some(a) = row.current_attempt {
        tx.execute(
            "UPDATE attempts SET state = ?1, ended_ms = ?2 WHERE attempt_id = ?3",
            params![attempt_state, now, a.as_slice()],
        )?;
    }
    let policy = RetryPolicy {
        max_attempts: row.max_attempts,
        base_backoff_ms: row.base_backoff_ms,
        max_backoff_ms: row.max_backoff_ms,
    };
    let (state, next) = if row.attempts >= row.max_attempts {
        (TaskState::Failed, row.next_eligible_ms)
    } else {
        (
            TaskState::Queued,
            now.saturating_add(policy.backoff_ms(row.attempts)),
        )
    };
    tx.execute(
        "UPDATE tasks SET state = ?1, current_attempt = NULL, lease_expires_ms = NULL,
                          result_sha256 = NULL, next_eligible_ms = ?2, updated_ms = ?3
         WHERE task_id = ?4",
        params![state.as_str(), next, now, task.as_slice()],
    )?;
    Ok(())
}

fn set_state(
    tx: &Transaction<'_>,
    task: &TaskId,
    state: TaskState,
    now: i64,
) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE tasks SET state = ?1, updated_ms = ?2 WHERE task_id = ?3",
        params![state.as_str(), now, task.as_slice()],
    )?;
    Ok(())
}

fn random_id() -> Result<AttemptId, StoreError> {
    let mut id = [0u8; 16];
    getrandom::fill(&mut id).map_err(|_| StoreError::Random)?;
    Ok(id)
}

impl Store {
    /// Open (or create) the database file and migrate it.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init(conn)
    }

    /// In-memory database (tests).
    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self, StoreError> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        migrations::migrate(&mut conn)?;
        Ok(Self { conn })
    }

    /// Schema version of the open database.
    pub fn schema_version(&self) -> Result<u32, StoreError> {
        Ok(self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }

    fn tx(&mut self) -> Result<Transaction<'_>, StoreError> {
        Ok(self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?)
    }

    /// Add a task in state `queued`.
    pub fn enqueue(
        &mut self,
        task: &TaskId,
        manifest: &[u8],
        policy: RetryPolicy,
        now: i64,
    ) -> Result<(), StoreError> {
        if policy.max_attempts == 0 || policy.base_backoff_ms < 0 || policy.max_backoff_ms < 0 {
            return Err(StoreError::Invalid("retry policy"));
        }
        let tx = self.tx()?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE task_id = ?1)",
            [task.as_slice()],
            |r| r.get(0),
        )?;
        if exists {
            return Err(StoreError::Duplicate);
        }
        tx.execute(
            "INSERT INTO tasks (task_id, manifest, state, max_attempts, base_backoff_ms, max_backoff_ms,
                                next_eligible_ms, updated_ms)
             VALUES (?1, ?2, 'queued', ?3, ?4, ?5, ?6, ?6)",
            params![
                task.as_slice(),
                manifest,
                policy.max_attempts,
                policy.base_backoff_ms,
                policy.max_backoff_ms,
                now
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Expire every lease whose time has passed. Returns how many were expired.
    pub fn expire_leases(&mut self, now: i64) -> Result<usize, StoreError> {
        let tx = self.tx()?;
        let n = Self::expire_in(&tx, now)?;
        tx.commit()?;
        Ok(n)
    }

    fn expire_in(tx: &Transaction<'_>, now: i64) -> Result<usize, StoreError> {
        let ids: Vec<Vec<u8>> = {
            let mut st = tx.prepare(
                "SELECT task_id FROM tasks
                 WHERE state IN ('leased','running') AND lease_expires_ms <= ?1",
            )?;
            let rows = st.query_map([now], |r| r.get(0))?;
            rows.collect::<Result<_, _>>()?
        };
        for id in &ids {
            let task: TaskId = id.as_slice().try_into().map_err(|_| StoreError::Corrupt)?;
            let row = load(tx, &task)?;
            end_attempt(tx, &task, &row, "expired", now)?;
        }
        Ok(ids.len())
    }

    /// Lease the next ready task to `worker` for `lease_ms`. Expires stale leases first.
    pub fn lease_next(
        &mut self,
        worker: &[u8],
        lease_ms: i64,
        now: i64,
    ) -> Result<Option<Lease>, StoreError> {
        if lease_ms <= 0 {
            return Err(StoreError::Invalid("lease_ms"));
        }
        let tx = self.tx()?;
        Self::expire_in(&tx, now)?;
        let next: Option<Vec<u8>> = tx
            .query_row(
                "SELECT task_id FROM tasks WHERE state = 'queued' AND next_eligible_ms <= ?1
                 ORDER BY next_eligible_ms, task_id LIMIT 1",
                [now],
                |r| r.get(0),
            )
            .optional()?;
        let Some(id) = next else {
            tx.commit()?;
            return Ok(None);
        };
        let task: TaskId = id.as_slice().try_into().map_err(|_| StoreError::Corrupt)?;
        let row = load(&tx, &task)?;
        let attempt_id = random_id()?;
        let attempt_no = row.attempts + 1;
        let expires_ms = now.saturating_add(lease_ms);
        tx.execute(
            "INSERT INTO attempts (attempt_id, task_id, attempt_no, worker, state, leased_ms)
             VALUES (?1, ?2, ?3, ?4, 'active', ?5)",
            params![
                attempt_id.as_slice(),
                task.as_slice(),
                attempt_no,
                worker,
                now
            ],
        )?;
        tx.execute(
            "UPDATE tasks SET state = 'leased', attempts = ?1, current_attempt = ?2,
                              lease_expires_ms = ?3, updated_ms = ?4
             WHERE task_id = ?5",
            params![
                attempt_no,
                attempt_id.as_slice(),
                expires_ms,
                now,
                task.as_slice()
            ],
        )?;
        tx.commit()?;
        Ok(Some(Lease {
            task_id: task,
            attempt_id,
            attempt_no,
            expires_ms,
        }))
    }

    /// Load the row and check that `attempt` is current and its lease (if any) is live.
    /// An expired lease is ended in the same transaction, which is committed before the error is returned.
    fn checked<'a>(
        &'a mut self,
        task: &TaskId,
        attempt: &AttemptId,
        now: i64,
    ) -> Result<(Transaction<'a>, Row), StoreError> {
        let tx = self.tx()?;
        let row = load(&tx, task)?;
        if row.current_attempt.as_ref() != Some(attempt) {
            return Err(StoreError::StaleAttempt);
        }
        if row.state.is_leased() && row.lease_expires_ms.is_none_or(|e| now >= e) {
            end_attempt(&tx, task, &row, "expired", now)?;
            tx.commit()?;
            return Err(StoreError::LeaseExpired);
        }
        Ok((tx, row))
    }

    /// `leased → running`.
    pub fn start(
        &mut self,
        task: &TaskId,
        attempt: &AttemptId,
        now: i64,
    ) -> Result<(), StoreError> {
        let (tx, row) = self.checked(task, attempt, now)?;
        if row.state != TaskState::Leased {
            return Err(StoreError::WrongState(row.state));
        }
        set_state(&tx, task, TaskState::Running, now)?;
        tx.commit()?;
        Ok(())
    }

    /// Extend a live lease to `now + lease_ms`.
    pub fn renew(
        &mut self,
        task: &TaskId,
        attempt: &AttemptId,
        lease_ms: i64,
        now: i64,
    ) -> Result<i64, StoreError> {
        if lease_ms <= 0 {
            return Err(StoreError::Invalid("lease_ms"));
        }
        let (tx, row) = self.checked(task, attempt, now)?;
        if !row.state.is_leased() {
            return Err(StoreError::WrongState(row.state));
        }
        let expires = now.saturating_add(lease_ms);
        tx.execute(
            "UPDATE tasks SET lease_expires_ms = ?1, updated_ms = ?2 WHERE task_id = ?3",
            params![expires, now, task.as_slice()],
        )?;
        tx.commit()?;
        Ok(expires)
    }

    /// `running → uploaded`, recording the result hash. Late uploads (lease expired) are rejected.
    pub fn upload(
        &mut self,
        task: &TaskId,
        attempt: &AttemptId,
        result_sha256: &[u8; 32],
        now: i64,
    ) -> Result<(), StoreError> {
        let (tx, row) = self.checked(task, attempt, now)?;
        if row.state != TaskState::Running {
            return Err(StoreError::WrongState(row.state));
        }
        tx.execute(
            "UPDATE tasks SET state = 'uploaded', result_sha256 = ?1, lease_expires_ms = NULL,
                              updated_ms = ?2 WHERE task_id = ?3",
            params![result_sha256.as_slice(), now, task.as_slice()],
        )?;
        tx.execute(
            "UPDATE attempts SET result_sha256 = ?1 WHERE attempt_id = ?2",
            params![result_sha256.as_slice(), attempt.as_slice()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// `uploaded → verified` if `accepted`, else the attempt is rejected and the task retried.
    pub fn verify(
        &mut self,
        task: &TaskId,
        attempt: &AttemptId,
        accepted: bool,
        now: i64,
    ) -> Result<(), StoreError> {
        let (tx, row) = self.checked(task, attempt, now)?;
        if row.state != TaskState::Uploaded {
            return Err(StoreError::WrongState(row.state));
        }
        if accepted {
            set_state(&tx, task, TaskState::Verified, now)?;
        } else {
            end_attempt(&tx, task, &row, "rejected", now)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// `verified → committed`. Idempotent for the committing attempt; every other attempt is rejected.
    pub fn commit(
        &mut self,
        task: &TaskId,
        attempt: &AttemptId,
        now: i64,
    ) -> Result<CommitOutcome, StoreError> {
        let tx = self.tx()?;
        let row = load(&tx, task)?;
        if row.state == TaskState::Committed {
            return if row.committed_attempt.as_ref() == Some(attempt) {
                Ok(CommitOutcome::AlreadyCommitted)
            } else {
                Err(StoreError::StaleAttempt)
            };
        }
        if row.current_attempt.as_ref() != Some(attempt) {
            return Err(StoreError::StaleAttempt);
        }
        if row.state != TaskState::Verified {
            return Err(StoreError::WrongState(row.state));
        }
        tx.execute(
            "UPDATE tasks SET state = 'committed', committed_attempt = ?1, current_attempt = NULL,
                              updated_ms = ?2 WHERE task_id = ?3 AND state = 'verified'",
            params![attempt.as_slice(), now, task.as_slice()],
        )?;
        tx.execute(
            "UPDATE attempts SET state = 'committed', ended_ms = ?1 WHERE attempt_id = ?2",
            params![now, attempt.as_slice()],
        )?;
        tx.commit()?;
        Ok(CommitOutcome::Committed)
    }

    /// The current attempt failed (worker error, crash, Stop). Re-queue with backoff or fail for good.
    pub fn fail(&mut self, task: &TaskId, attempt: &AttemptId, now: i64) -> Result<(), StoreError> {
        let (tx, row) = self.checked(task, attempt, now)?;
        if matches!(
            row.state,
            TaskState::Committed | TaskState::Failed | TaskState::Queued
        ) {
            return Err(StoreError::WrongState(row.state));
        }
        end_attempt(&tx, task, &row, "failed", now)?;
        tx.commit()?;
        Ok(())
    }

    /// Snapshot of a task.
    pub fn task(&mut self, task: &TaskId) -> Result<TaskView, StoreError> {
        let tx = self.tx()?;
        let r = load(&tx, task)?;
        tx.commit()?;
        Ok(TaskView {
            state: r.state,
            attempts: r.attempts,
            max_attempts: r.max_attempts,
            current_attempt: r.current_attempt,
            lease_expires_ms: r.lease_expires_ms,
            next_eligible_ms: r.next_eligible_ms,
            result_sha256: r.result_sha256,
            committed_attempt: r.committed_attempt,
        })
    }

    /// The manifest bytes stored with a task.
    pub fn manifest(&self, task: &TaskId) -> Result<Vec<u8>, StoreError> {
        self.conn
            .query_row(
                "SELECT manifest FROM tasks WHERE task_id = ?1",
                [task.as_slice()],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(StoreError::UnknownTask)
    }

    /// Number of attempts recorded for a task.
    pub fn attempt_count(&self, task: &TaskId) -> Result<u32, StoreError> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM attempts WHERE task_id = ?1",
            [task.as_slice()],
            |r| r.get(0),
        )?)
    }
}
