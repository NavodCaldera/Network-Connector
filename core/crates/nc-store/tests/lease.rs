#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Lease state machine: unit tests and property tests (AGENT.md §1 rule 8).

use std::collections::HashMap;

use nc_store::{AttemptId, CommitOutcome, RetryPolicy, Store, StoreError, TaskId, TaskState};
use proptest::prelude::*;

const W: &[u8] = b"worker-1";
const LEASE: i64 = 1_000;

fn tid(n: u8) -> TaskId {
    [n; 16]
}

fn store_with(n: u8, policy: RetryPolicy) -> Store {
    let mut s = Store::open_in_memory().unwrap();
    for i in 0..n {
        s.enqueue(&tid(i), b"manifest", policy, 0).unwrap();
    }
    s
}

fn run_to_verified(s: &mut Store, now: i64) -> (TaskId, AttemptId) {
    let l = s.lease_next(W, LEASE, now).unwrap().unwrap();
    s.start(&l.task_id, &l.attempt_id, now + 1).unwrap();
    s.upload(&l.task_id, &l.attempt_id, &[7; 32], now + 2)
        .unwrap();
    s.verify(&l.task_id, &l.attempt_id, true, now + 3).unwrap();
    (l.task_id, l.attempt_id)
}

#[test]
fn happy_path_and_idempotent_commit() {
    let mut s = store_with(1, RetryPolicy::default());
    let (t, a) = run_to_verified(&mut s, 10);
    assert_eq!(s.commit(&t, &a, 20).unwrap(), CommitOutcome::Committed);
    assert_eq!(
        s.commit(&t, &a, 30).unwrap(),
        CommitOutcome::AlreadyCommitted
    );
    let v = s.task(&t).unwrap();
    assert_eq!(v.state, TaskState::Committed);
    assert_eq!(v.committed_attempt, Some(a));
    assert_eq!(v.result_sha256, Some([7; 32]));
    assert!(s.lease_next(W, LEASE, 40).unwrap().is_none());
}

#[test]
fn late_result_after_expiry_is_rejected_and_cannot_commit() {
    let mut s = store_with(1, RetryPolicy::default());
    let l = s.lease_next(W, LEASE, 0).unwrap().unwrap();
    s.start(&l.task_id, &l.attempt_id, 1).unwrap();
    // Lease ends at 1000; the upload arrives at 1000.
    assert!(matches!(
        s.upload(&l.task_id, &l.attempt_id, &[1; 32], LEASE),
        Err(StoreError::LeaseExpired)
    ));
    let v = s.task(&l.task_id).unwrap();
    assert_eq!(v.state, TaskState::Queued);
    assert_eq!(v.next_eligible_ms, LEASE + 1_000);
    // Second try after backoff succeeds; the old attempt can never commit.
    let (t, a2) = run_to_verified(&mut s, 2_000);
    assert!(matches!(
        s.commit(&t, &l.attempt_id, 2_010),
        Err(StoreError::StaleAttempt)
    ));
    assert_eq!(s.commit(&t, &a2, 2_010).unwrap(), CommitOutcome::Committed);
    assert!(matches!(
        s.commit(&t, &l.attempt_id, 2_020),
        Err(StoreError::StaleAttempt)
    ));
}

#[test]
fn renew_keeps_lease_alive() {
    let mut s = store_with(1, RetryPolicy::default());
    let l = s.lease_next(W, LEASE, 0).unwrap().unwrap();
    s.start(&l.task_id, &l.attempt_id, 500).unwrap();
    assert_eq!(
        s.renew(&l.task_id, &l.attempt_id, LEASE, 900).unwrap(),
        1_900
    );
    assert_eq!(s.expire_leases(1_500).unwrap(), 0);
    s.upload(&l.task_id, &l.attempt_id, &[1; 32], 1_800)
        .unwrap();
}

#[test]
fn backoff_doubles_and_attempts_run_out() {
    let p = RetryPolicy {
        max_attempts: 3,
        base_backoff_ms: 100,
        max_backoff_ms: 150,
    };
    let mut s = store_with(1, p);
    let t = tid(0);
    let mut now = 0;
    for i in 1..=3u32 {
        let l = s.lease_next(W, LEASE, now).unwrap().unwrap();
        assert_eq!(l.attempt_no, i);
        s.fail(&t, &l.attempt_id, now + 1).unwrap();
        let v = s.task(&t).unwrap();
        if i < 3 {
            assert_eq!(v.state, TaskState::Queued);
            assert_eq!(v.next_eligible_ms, now + 1 + p.backoff_ms(i));
            assert!(s
                .lease_next(W, LEASE, v.next_eligible_ms - 1)
                .unwrap()
                .is_none());
            now = v.next_eligible_ms;
        } else {
            assert_eq!(v.state, TaskState::Failed);
        }
    }
    assert_eq!(p.backoff_ms(1), 100);
    assert_eq!(p.backoff_ms(2), 150);
    assert!(s.lease_next(W, LEASE, 1_000_000).unwrap().is_none());
    assert_eq!(s.attempt_count(&t).unwrap(), 3);
}

#[test]
fn rejected_verification_retries() {
    let mut s = store_with(1, RetryPolicy::default());
    let l = s.lease_next(W, LEASE, 0).unwrap().unwrap();
    s.start(&l.task_id, &l.attempt_id, 1).unwrap();
    s.upload(&l.task_id, &l.attempt_id, &[1; 32], 2).unwrap();
    s.verify(&l.task_id, &l.attempt_id, false, 3).unwrap();
    let v = s.task(&l.task_id).unwrap();
    assert_eq!(v.state, TaskState::Queued);
    assert_eq!(v.result_sha256, None);
}

#[test]
fn duplicate_enqueue_and_unknown_task() {
    let mut s = store_with(1, RetryPolicy::default());
    assert!(matches!(
        s.enqueue(&tid(0), b"x", RetryPolicy::default(), 0),
        Err(StoreError::Duplicate)
    ));
    assert!(matches!(s.task(&tid(9)), Err(StoreError::UnknownTask)));
}

#[test]
fn file_store_persists_and_migrations_are_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nc.db");
    let a = {
        let mut s = Store::open(&path).unwrap();
        assert_eq!(s.schema_version().unwrap(), 1);
        s.enqueue(&tid(1), b"m", RetryPolicy::default(), 0).unwrap();
        s.lease_next(W, LEASE, 0).unwrap().unwrap()
    };
    let mut s = Store::open(&path).unwrap();
    assert_eq!(s.schema_version().unwrap(), 1);
    assert_eq!(s.task(&tid(1)).unwrap().current_attempt, Some(a.attempt_id));
    assert_eq!(s.manifest(&tid(1)).unwrap(), b"m");
}

#[test]
fn newer_schema_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nc.db");
    drop(Store::open(&path).unwrap());
    {
        let c = rusqlite_conn(&path);
        c.pragma_update(None, "user_version", 99).unwrap();
    }
    assert!(matches!(
        Store::open(&path),
        Err(StoreError::SchemaTooNew(99))
    ));
}

fn rusqlite_conn(path: &std::path::Path) -> rusqlite::Connection {
    rusqlite::Connection::open(path).unwrap()
}

// ---------------------------------------------------------------------------------------------------------
// Property tests

#[derive(Debug, Clone)]
enum Op {
    Lease(i64),
    Start(u8, u8),
    Renew(u8, u8, i64),
    Upload(u8, u8),
    Verify(u8, u8, bool),
    Commit(u8, u8),
    Fail(u8, u8),
    Advance(i64),
    Expire,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (1i64..400).prop_map(Op::Lease),
        2 => (0u8..3, 0u8..6).prop_map(|(t, a)| Op::Start(t, a)),
        1 => (0u8..3, 0u8..6, 1i64..400).prop_map(|(t, a, l)| Op::Renew(t, a, l)),
        2 => (0u8..3, 0u8..6).prop_map(|(t, a)| Op::Upload(t, a)),
        2 => (0u8..3, 0u8..6, any::<bool>()).prop_map(|(t, a, ok)| Op::Verify(t, a, ok)),
        3 => (0u8..3, 0u8..6).prop_map(|(t, a)| Op::Commit(t, a)),
        1 => (0u8..3, 0u8..6).prop_map(|(t, a)| Op::Fail(t, a)),
        2 => (0i64..500).prop_map(Op::Advance),
        1 => Just(Op::Expire),
    ]
}

/// Pick an attempt id issued for task `t` (index wraps); a never-issued random id if none yet.
fn pick(issued: &HashMap<u8, Vec<AttemptId>>, t: u8, a: u8) -> AttemptId {
    match issued.get(&t) {
        Some(v) if !v.is_empty() => v[a as usize % v.len()],
        _ => [0xEE; 16],
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    #[test]
    fn lease_invariants_hold(ops in prop::collection::vec(op(), 1..80)) {
        let policy = RetryPolicy { max_attempts: 4, base_backoff_ms: 50, max_backoff_ms: 400 };
        let mut s = store_with(3, policy);
        let mut now: i64 = 0;
        let mut issued: HashMap<u8, Vec<AttemptId>> = HashMap::new();
        let mut committed: HashMap<u8, AttemptId> = HashMap::new();

        for op in ops {
            // Snapshot before the op, for attempt-check assertions.
            let target = match &op {
                Op::Start(t, a) | Op::Renew(t, a, _) | Op::Upload(t, a) | Op::Verify(t, a, _)
                | Op::Commit(t, a) | Op::Fail(t, a) => Some((*t, pick(&issued, *t, *a))),
                _ => None,
            };
            let before = target.map(|(t, _)| s.task(&tid(t)).unwrap());

            match op {
                Op::Lease(ms) => {
                    if let Some(l) = s.lease_next(W, ms, now).unwrap() {
                        let t = l.task_id[0];
                        // A task is only leased if it is not committed, and attempt ids are never reused.
                        prop_assert!(!committed.contains_key(&t));
                        prop_assert!(!issued.values().flatten().any(|a| *a == l.attempt_id));
                        prop_assert!(l.attempt_no <= policy.max_attempts);
                        prop_assert_eq!(l.expires_ms, now + ms);
                        issued.entry(t).or_default().push(l.attempt_id);
                    }
                }
                Op::Start(t, _) | Op::Renew(t, _, _) | Op::Upload(t, _) | Op::Verify(t, _, _)
                | Op::Fail(t, _) | Op::Commit(t, _) => {
                    let (_, a) = target.unwrap();
                    let b = before.clone().unwrap();
                    let res: Result<Option<CommitOutcome>, StoreError> = match op {
                        Op::Start(..) => s.start(&tid(t), &a, now).map(|_| None),
                        Op::Renew(_, _, l) => s.renew(&tid(t), &a, l, now).map(|_| None),
                        Op::Upload(..) => s.upload(&tid(t), &a, &[t; 32], now).map(|_| None),
                        Op::Verify(_, _, ok) => s.verify(&tid(t), &a, ok, now).map(|_| None),
                        Op::Fail(..) => s.fail(&tid(t), &a, now).map(|_| None),
                        Op::Commit(..) => s.commit(&tid(t), &a, now).map(Some),
                        _ => unreachable!(),
                    };
                    let is_current = b.current_attempt == Some(a);
                    let lease_live = !b.state.is_leased() || b.lease_expires_ms.is_some_and(|e| now < e);
                    match res {
                        Ok(Some(CommitOutcome::Committed)) => {
                            // No double completion; only the current, verified attempt commits.
                            prop_assert!(!committed.contains_key(&t));
                            prop_assert!(is_current);
                            prop_assert_eq!(b.state, TaskState::Verified);
                            committed.insert(t, a);
                        }
                        Ok(Some(CommitOutcome::AlreadyCommitted)) => {
                            prop_assert_eq!(committed.get(&t), Some(&a));
                        }
                        Ok(None) => {
                            // Every successful transition was by the current attempt with a live lease.
                            prop_assert!(is_current);
                            prop_assert!(lease_live);
                        }
                        Err(StoreError::LeaseExpired) => {
                            prop_assert!(is_current && !lease_live);
                            let v = s.task(&tid(t)).unwrap();
                            prop_assert!(matches!(v.state, TaskState::Queued | TaskState::Failed));
                            prop_assert_eq!(v.current_attempt, None);
                        }
                        Err(StoreError::StaleAttempt) => {
                            prop_assert!(!is_current);
                        }
                        Err(StoreError::WrongState(_)) => {}
                        Err(e) => prop_assert!(false, "unexpected error {e:?}"),
                    }
                }
                Op::Advance(ms) => now += ms,
                Op::Expire => {
                    s.expire_leases(now).unwrap();
                    // Leases expire: nothing stays leased past its expiry after a sweep.
                    for t in 0..3 {
                        let v = s.task(&tid(t)).unwrap();
                        if v.state.is_leased() {
                            prop_assert!(v.lease_expires_ms.unwrap() > now);
                        }
                    }
                }
            }

            for t in 0..3u8 {
                let v = s.task(&tid(t)).unwrap();
                prop_assert!(v.attempts <= policy.max_attempts);
                prop_assert_eq!(s.attempt_count(&tid(t)).unwrap(), v.attempts);
                // Once committed, a task stays committed by the same attempt.
                if let Some(a) = committed.get(&t) {
                    prop_assert_eq!(v.state, TaskState::Committed);
                    prop_assert_eq!(v.committed_attempt, Some(*a));
                } else {
                    prop_assert!(v.state != TaskState::Committed);
                }
            }
        }

        // Commit is idempotent: replaying every successful commit changes nothing.
        for (t, a) in &committed {
            let before = s.task(&tid(*t)).unwrap();
            prop_assert_eq!(s.commit(&tid(*t), a, now + 1).unwrap(), CommitOutcome::AlreadyCommitted);
            prop_assert_eq!(s.task(&tid(*t)).unwrap(), before);
        }
        // Every other attempt of a committed task is refused.
        for (t, a) in &committed {
            for other in issued.get(t).into_iter().flatten().filter(|x| *x != a) {
                prop_assert!(matches!(s.commit(&tid(*t), other, now + 2), Err(StoreError::StaleAttempt)));
            }
        }
    }

    /// Leases expire: any lease left alone is gone (re-queued or failed) once time passes its expiry.
    #[test]
    fn untouched_leases_always_expire(lease_ms in 1i64..10_000, extra in 0i64..10_000, n in 1u8..4) {
        let mut s = store_with(n, RetryPolicy::default());
        let mut leases = Vec::new();
        while let Some(l) = s.lease_next(W, lease_ms, 0).unwrap() {
            leases.push(l);
        }
        prop_assert_eq!(leases.len(), n as usize);
        let at = lease_ms + extra;
        prop_assert_eq!(s.expire_leases(at).unwrap(), n as usize);
        for l in leases {
            let v = s.task(&l.task_id).unwrap();
            prop_assert_eq!(v.state, TaskState::Queued);
            prop_assert!(matches!(s.start(&l.task_id, &l.attempt_id, at), Err(StoreError::StaleAttempt)));
        }
    }
}
