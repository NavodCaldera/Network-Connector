#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `list_unfinished` (crash recovery) and the persistent budget-usage ledger.

use nc_store::{RetryPolicy, Store, StoreError, TaskState};
use proptest::prelude::*;

fn one_try() -> RetryPolicy {
    RetryPolicy {
        max_attempts: 1,
        base_backoff_ms: 0,
        max_backoff_ms: 0,
    }
}

#[test]
fn lists_every_non_terminal_task_and_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.sqlite");
    {
        let mut s = Store::open(&path).unwrap();
        for i in 1..=6u8 {
            s.enqueue(&[i; 16], b"m", one_try(), 10).unwrap();
        }
        // Leases go in task-id order: 1 failed, 2 leased, 3 running, 4 uploaded, 5 committed, 6 queued.
        let mut leases = Vec::new();
        for _ in 2..=6 {
            leases.push(s.lease_next(b"w", 1_000_000, 20).unwrap().unwrap());
        }
        let l: Vec<_> = (1..=5u8)
            .map(|i| *leases.iter().find(|l| l.task_id == [i; 16]).unwrap())
            .collect();
        s.start(&l[2].task_id, &l[2].attempt_id, 30).unwrap();
        s.start(&l[3].task_id, &l[3].attempt_id, 30).unwrap();
        s.upload(&l[3].task_id, &l[3].attempt_id, &[1; 32], 31)
            .unwrap();
        s.start(&l[4].task_id, &l[4].attempt_id, 30).unwrap();
        s.upload(&l[4].task_id, &l[4].attempt_id, &[2; 32], 31)
            .unwrap();
        s.verify(&l[4].task_id, &l[4].attempt_id, true, 32).unwrap();
        s.commit(&l[4].task_id, &l[4].attempt_id, 33).unwrap();
        s.fail(&l[0].task_id, &l[0].attempt_id, 34).unwrap();
        // The crash: the store is dropped with work open.
    }
    let mut s = Store::open(&path).unwrap();
    let u = s.list_unfinished().unwrap();
    let mut states: Vec<_> = u.iter().map(|t| (t.task_id[0], t.state)).collect();
    states.sort_unstable_by_key(|x| x.0);
    assert_eq!(
        states,
        vec![
            (2, TaskState::Leased),
            (3, TaskState::Running),
            (4, TaskState::Uploaded),
            (6, TaskState::Queued),
        ]
    );
    let mut ids: Vec<u8> = u.iter().map(|t| t.task_id[0]).collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![2, 3, 4, 6]);
    for t in &u {
        assert_eq!(t.current_attempt.is_some(), t.state != TaskState::Queued);
        assert_eq!(t.lease_expires_ms.is_some(), t.state.is_leased());
    }
    // Cleaning up: fail each open attempt locally; nothing is left.
    for t in u {
        if let Some(a) = t.current_attempt {
            s.fail(&t.task_id, &a, 100).unwrap();
        }
    }
    let left: Vec<_> = s.list_unfinished().unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].state, TaskState::Queued);
}

#[test]
fn budget_usage_survives_restart_and_is_keyed_by_ledger_and_day() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.sqlite");
    {
        let mut s = Store::open(&path).unwrap();
        s.add_budget_usage("home:1", "2026-11-01", 100).unwrap();
        s.add_budget_usage("home:1", "2026-11-02", 40).unwrap();
        s.add_budget_usage("home:1", "2026-11-02", 2).unwrap();
        s.add_budget_usage("other:1", "2026-11-02", 7).unwrap();
    }
    let mut s = Store::open(&path).unwrap();
    assert_eq!(s.schema_version().unwrap(), 2);
    assert_eq!(
        s.budget_usage("home:1", "2026-11-01", "2026-11-30")
            .unwrap(),
        142
    );
    assert_eq!(
        s.budget_usage("home:1", "2026-11-02", "2026-11-02")
            .unwrap(),
        42
    );
    assert_eq!(
        s.budget_usage("other:1", "2026-11-01", "2026-11-30")
            .unwrap(),
        7
    );
    assert_eq!(
        s.budget_usage("home:1", "2026-12-01", "2026-12-31")
            .unwrap(),
        0
    );
    s.add_budget_usage("home:1", "2026-11-02", u64::MAX)
        .unwrap();
    assert_eq!(
        s.budget_usage("home:1", "2026-11-02", "2026-11-02")
            .unwrap(),
        i64::MAX as u64
    );
}

#[test]
fn budget_usage_rejects_bad_keys() {
    let mut s = Store::open_in_memory().unwrap();
    assert!(matches!(
        s.add_budget_usage("", "2026-11-02", 1),
        Err(StoreError::Invalid(_))
    ));
    assert!(matches!(
        s.add_budget_usage("k", "2026-1-02x", 1),
        Err(StoreError::Invalid(_))
    ));
    assert!(matches!(
        s.budget_usage("k", "nope", "2026-11-02"),
        Err(StoreError::Invalid(_))
    ));
}

proptest! {
    /// Whatever sequence of leases, starts, uploads and fails, `list_unfinished` returns exactly the tasks
    /// whose state is not terminal, each with a current attempt unless still queued.
    #[test]
    fn list_unfinished_matches_task_states(ops in proptest::collection::vec((0u8..6, 0u8..4), 1..40)) {
        let mut s = Store::open_in_memory().unwrap();
        for i in 0..6u8 {
            s.enqueue(&[i; 16], b"m", RetryPolicy { max_attempts: 2, base_backoff_ms: 0, max_backoff_ms: 0 }, 0).unwrap();
        }
        let mut now = 1;
        for (task, op) in ops {
            now += 1;
            let id = [task; 16];
            let v = s.task(&id).unwrap();
            match (op, v.current_attempt) {
                (0, _) => { let _ = s.lease_next(b"w", 1_000_000, now); }
                (1, Some(a)) => { let _ = s.start(&id, &a, now); }
                (2, Some(a)) => { let _ = s.upload(&id, &a, &[3; 32], now); }
                (3, Some(a)) => { let _ = s.fail(&id, &a, now); }
                _ => {}
            }
        }
        let u = s.list_unfinished().unwrap();
        for i in 0..6u8 {
            let v = s.task(&[i; 16]).unwrap();
            let listed = u.iter().find(|t| t.task_id == [i; 16]);
            let terminal = matches!(v.state, TaskState::Committed | TaskState::Failed);
            prop_assert_eq!(listed.is_none(), terminal);
            if let Some(t) = listed {
                prop_assert_eq!(t.state, v.state);
                prop_assert_eq!(t.current_attempt, v.current_attempt);
                prop_assert_eq!(t.current_attempt.is_some(), t.state != TaskState::Queued);
            }
        }
    }
}

#[test]
fn v1_database_is_upgraded_and_keeps_its_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.sqlite");
    {
        let mut s = Store::open(&path).unwrap();
        s.enqueue(&[1; 16], b"m", one_try(), 0).unwrap();
    }
    {
        // Turn it back into a v1 database.
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch("DROP TABLE budget_usage; PRAGMA user_version = 1;")
            .unwrap();
    }
    let mut s = Store::open(&path).unwrap();
    assert_eq!(s.schema_version().unwrap(), 2);
    assert_eq!(s.task(&[1; 16]).unwrap().state, TaskState::Queued);
    s.add_budget_usage("k", "2026-11-02", 5).unwrap();
    assert_eq!(s.budget_usage("k", "2026-11-02", "2026-11-02").unwrap(), 5);
}
