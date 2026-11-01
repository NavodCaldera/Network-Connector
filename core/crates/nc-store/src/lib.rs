//! `nc-store`: SQLite task queue with leases and attempts (PLAN.md §3 "Task state machine").
//!
//! ```text
//! queued → leased → running → uploaded → verified → committed
//!             ↘        ↘          ↘ (rejected)
//!              expired / failed → queued again after backoff, or failed for good after max attempts
//! ```
//!
//! Rules enforced here:
//! - every lease gets a fresh random attempt id; only the **current** attempt may move a task;
//! - a lease that has passed its expiry is treated as expired the moment it is touched (and by
//!   [`Store::expire_leases`]); a late result from it is rejected;
//! - commit is **idempotent and attempt-checked**: committing the committed attempt again returns
//!   [`CommitOutcome::AlreadyCommitted`] and changes nothing; any other attempt is rejected;
//! - each transition runs in one `BEGIN IMMEDIATE` transaction.
//!
//! Time is passed in by the caller (`now_ms`) so behaviour is deterministic and testable.

mod migrations;
mod store;

pub use store::{
    AttemptId, CommitOutcome, Lease, RetryPolicy, Store, StoreError, TaskId, TaskState, TaskView,
};
