//! Schema migrations, tracked with `PRAGMA user_version`. Append only; never edit a shipped migration.

use rusqlite::Connection;

use crate::StoreError;

/// Migration `i` moves the schema from version `i` to `i + 1`.
const MIGRATIONS: &[&str] = &[
    // v1: tasks and attempts.
    r#"
    CREATE TABLE tasks (
        task_id           BLOB PRIMARY KEY CHECK (length(task_id) = 16),
        manifest          BLOB NOT NULL,
        state             TEXT NOT NULL CHECK (state IN
                              ('queued','leased','running','uploaded','verified','committed','failed')),
        attempts          INTEGER NOT NULL DEFAULT 0,
        max_attempts      INTEGER NOT NULL CHECK (max_attempts > 0),
        base_backoff_ms   INTEGER NOT NULL,
        max_backoff_ms    INTEGER NOT NULL,
        current_attempt   BLOB,
        lease_expires_ms  INTEGER,
        next_eligible_ms  INTEGER NOT NULL,
        result_sha256     BLOB,
        committed_attempt BLOB,
        updated_ms        INTEGER NOT NULL
    );
    CREATE INDEX tasks_ready ON tasks(state, next_eligible_ms);
    CREATE TABLE attempts (
        attempt_id    BLOB PRIMARY KEY CHECK (length(attempt_id) = 16),
        task_id       BLOB NOT NULL REFERENCES tasks(task_id),
        attempt_no    INTEGER NOT NULL,
        worker        BLOB NOT NULL,
        state         TEXT NOT NULL CHECK (state IN
                          ('active','expired','failed','rejected','committed')),
        leased_ms     INTEGER NOT NULL,
        ended_ms      INTEGER,
        result_sha256 BLOB,
        UNIQUE (task_id, attempt_no)
    );
    "#,
];

/// Newest schema version this build knows.
pub const LATEST: u32 = MIGRATIONS.len() as u32;

/// Bring `conn` to [`LATEST`]. Refuses a database written by a newer build.
pub fn migrate(conn: &mut Connection) -> Result<u32, StoreError> {
    let current: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if current > LATEST {
        return Err(StoreError::SchemaTooNew(current));
    }
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", i as u32 + 1)?;
        tx.commit()?;
    }
    Ok(LATEST)
}
