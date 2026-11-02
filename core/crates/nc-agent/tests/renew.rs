#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Lease renewal (task 1.P8) against the in-process fake coordinator, and the persistent usage ledger.

mod common;

use common::*;
use nc_agent::budget::{BudgetGate, NetworkBudget};
use nc_agent::coord::CoordError;
use nc_agent::probe::Clock;
use nc_store::Store;
use nc_tariff::TrafficClass;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn noon_s() -> u64 {
    noon().timestamp() as u64
}

fn set_clock(kit: &Kit, clock: Arc<dyn Clock>) {
    kit.coord.s.lock().unwrap().clock = Some(clock);
}

fn set_mode(kit: &Kit, mode: RenewMode) {
    kit.coord.s.lock().unwrap().renew_mode = mode;
}

/// After the session: nothing uploaded or completed, no Fail sent, and the local record is closed.
fn assert_dropped(kit: &Kit) {
    let s = kit.coord.s.lock().unwrap();
    assert!(s.puts.is_empty(), "result must not be uploaded");
    assert!(s.completes.is_empty(), "result must not be completed");
    assert!(s.fails.is_empty(), "no Fail for a lease that is gone");
    drop(s);
    let mut j = kit.journal();
    assert!(j.pending().unwrap().is_empty(), "local failure recorded");
}

#[test]
fn long_job_is_renewed_and_completes_and_renews_are_charged() {
    let kit = Kit::new(Mode::Sleep(6_000));
    // 25x: the 6 s run is 150 s of lease time, far past the 60 s TTL. A faster clock left too little
    // real slack per renew and flaked on slow CI runners.
    let clock: Arc<dyn Clock> = Arc::new(FastClock::new(noon(), 25));
    set_clock(&kit, clock.clone());
    kit.queue_job_expiring(b"\0asm-module", b"hello", noon_s() + 60);
    let usage_path = kit.dir.path().join("budget.sqlite");
    let gate = BudgetGate::with_usage_store(
        &NetworkBudget::Metered(metered(1 << 30)),
        clock.now(),
        Store::open(&usage_path).unwrap(),
    )
    .unwrap();
    let agent = kit.agent_with_gate(gate, clock.clone());
    agent.start_helper_session(kit.policy()).unwrap();
    until(|| agent.today().jobs_done == 1);
    agent.stop_helper_session();
    drop(agent);

    let s = kit.coord.s.lock().unwrap();
    assert_eq!(s.completes.len(), 1);
    assert_eq!(s.puts.len(), 1);
    assert!(s.fails.is_empty());
    let renews = s.renews.len();
    assert!(renews >= 3, "renewed {renews} times");
    let lease = &s.renews[0];
    assert!(s
        .renews
        .iter()
        .all(|r| r.lease_id == lease.lease_id && r.attempt_id == lease.attempt_id));
    // The fake extended the lease past the original 60 s.
    let exp = *s.expiry.get(&lease.lease_id).unwrap();
    assert!(exp > noon_s() + 60, "expiry moved to {exp}");
    let controls = s
        .calls
        .iter()
        .filter(|c| *c == "register" || *c == "heartbeat" || *c == "renew")
        .count() as u64;
    let module_len = b"\0asm-module".len() as u64;
    let blob_bytes: u64 = s.blobs.values().map(|b| b.len() as u64).sum();
    drop(s);
    eprintln!("renews during the long job: {renews}");

    // Every renew is charged as one 4 KiB control call; the ledger persisted it all.
    let mut store = Store::open(&usage_path).unwrap();
    let used = store
        .budget_usage("tariff:test", "2026-11-01", "2026-11-30")
        .unwrap();
    // Module + input downloads, then the 5-byte output upload: one offset query and one chunk, each charged
    // with the per-request upload overhead.
    let transfers = blob_bytes + 5 + 2 * nc_agent::transfer::UPLOAD_OVERHEAD;
    assert!(blob_bytes >= module_len);
    assert_eq!(used, controls * 4096 + transfers);
}

#[test]
fn lost_lease_stops_the_job_and_nothing_is_uploaded() {
    for (status, code) in [
        (409, "lease_expired"),
        (409, "attempt_mismatch"),
        (404, "not_found"),
    ] {
        let kit = Kit::new(Mode::UntilCancel);
        set_mode(
            &kit,
            RenewMode::Error(CoordError::Status {
                status,
                code: code.into(),
            }),
        );
        kit.queue_job(b"module", b"input");
        let agent = kit.agent();
        agent.start_helper_session(kit.policy()).unwrap();
        // The lease may be lost while downloading (the job never starts) or while running (it is cancelled).
        until(|| kit.coord.count("renew") >= 1);
        until(|| agent.activity() == nc_agent::Activity::Idle);
        agent.stop_helper_session();
        assert_eq!(kit.coord.count("renew"), 1, "{status} {code}: no retry");
        if kit.runner.runs.load(Ordering::SeqCst) > 0 {
            assert!(kit.runner.cancelled_at.lock().unwrap().is_some());
        }
        assert_dropped(&kit);
    }
}

#[test]
fn other_409_codes_are_not_treated_as_lease_loss() {
    let kit = Kit::new(Mode::Sleep(300));
    set_mode(
        &kit,
        RenewMode::Error(CoordError::Status {
            status: 409,
            code: "something_else".into(),
        }),
    );
    kit.queue_job(b"module", b"input");
    let agent = kit.agent();
    agent.start_helper_session(kit.policy()).unwrap();
    until(|| agent.today().jobs_done == 1);
    agent.stop_helper_session();
    assert!(kit.coord.count("renew") >= 1);
    assert_eq!(kit.coord.count("complete"), 1);
}

#[test]
fn network_errors_are_retried_and_the_job_stops_at_expiry() {
    let kit = Kit::new(Mode::UntilCancel);
    set_mode(
        &kit,
        RenewMode::Error(CoordError::Network("connect".into())),
    );
    // 20x: the 60 s lease ends after about 3 s of real time.
    let t0 = Instant::now();
    let clock: Arc<dyn Clock> = Arc::new(FastClock::new(noon(), 20));
    kit.queue_job_expiring(b"module", b"input", noon_s() + 60);
    let agent = kit.agent_with(NetworkBudget::Unmetered, clock);
    agent.start_helper_session(kit.policy()).unwrap();
    until(|| kit.runner.cancelled_at.lock().unwrap().is_some());
    let cancelled = kit.runner.cancelled_at.lock().unwrap().unwrap() - t0;
    until(|| agent.activity() == nc_agent::Activity::Idle);
    agent.stop_helper_session();
    let renews = kit.coord.count("renew");
    eprintln!("job stopped {cancelled:?} after start, after {renews} failed renews");
    assert!(renews >= 2, "network errors retried ({renews})");
    // Not before the lease ended (3 s real), and promptly after it.
    assert!(cancelled >= Duration::from_millis(2_990), "{cancelled:?}");
    assert!(cancelled < Duration::from_millis(3_500), "{cancelled:?}");
    assert_dropped(&kit);
}

#[test]
fn capped_lease_stops_before_expiry() {
    let kit = Kit::new(Mode::Sleep(20_000));
    let t0 = Instant::now();
    let clock: Arc<dyn Clock> = Arc::new(FastClock::new(noon(), 20));
    set_clock(&kit, clock.clone());
    // The server will not extend past noon + 90 s.
    set_mode(&kit, RenewMode::CapAt(noon_s() + 90));
    kit.queue_job_expiring(b"module", b"input", noon_s() + 60);
    let agent = kit.agent_with(NetworkBudget::Unmetered, clock);
    agent.start_helper_session(kit.policy()).unwrap();
    until(|| kit.runner.cancelled_at.lock().unwrap().is_some());
    let cancelled = kit.runner.cancelled_at.lock().unwrap().unwrap() - t0;
    until(|| agent.activity() == nc_agent::Activity::Idle);
    agent.stop_helper_session();
    let renews = kit.coord.count("renew");
    eprintln!("capped job stopped {cancelled:?} after start, after {renews} renews");
    // Stopped at about noon + 85 s (5 s guard), strictly before the capped expiry at noon + 90 s (4.5 s real).
    assert!(cancelled >= Duration::from_millis(4_000), "{cancelled:?}");
    assert!(cancelled < Duration::from_millis(4_500), "{cancelled:?}");
    // Once the cap stopped moving the expiry, renewals stopped too.
    let s = kit.coord.s.lock().unwrap();
    let capped_at = s.renews.len();
    drop(s);
    // Renews run every 50 ms of real time: about 85 over the 4.25 s if they never stopped.
    assert!(capped_at <= 60, "{capped_at} renews");
    assert_dropped(&kit);
}

#[test]
fn used_bytes_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("budget.sqlite");
    let budget = NetworkBudget::Metered(metered(1_000_000));
    let now = noon();
    let before = {
        let gate = BudgetGate::with_usage_store(&budget, now, Store::open(&path).unwrap()).unwrap();
        let before = gate.available_bytes().unwrap();
        let g = gate
            .authorize(TrafficClass::Bulk, 300_000, now, &home())
            .unwrap();
        gate.settle(g, 250_000);
        assert_eq!(gate.available_bytes().unwrap(), before - 250_000);
        before
    };
    // "Restart": a new gate over the same file starts with the bytes already used.
    let gate = BudgetGate::with_usage_store(&budget, now, Store::open(&path).unwrap()).unwrap();
    assert_eq!(gate.available_bytes().unwrap(), before - 250_000);
    // A budget update re-reads the ledger rather than starting from zero.
    gate.update(&budget, now).unwrap();
    assert_eq!(gate.available_bytes().unwrap(), before - 250_000);
    // Next billing period (resets on the 1st): usage starts from zero again.
    let next = chrono::TimeZone::with_ymd_and_hms(&chrono::Utc, 2026, 12, 1, 12, 0, 0).unwrap();
    let gate = BudgetGate::with_usage_store(&budget, next, Store::open(&path).unwrap()).unwrap();
    assert_eq!(gate.available_bytes().unwrap(), before);
    // Without a store nothing is remembered (the old behaviour).
    let gate = BudgetGate::new(&budget, now).unwrap();
    assert_eq!(gate.available_bytes().unwrap(), before);
}

#[test]
fn stop_during_a_renewed_job_is_still_fast() {
    let kit = Kit::new(Mode::UntilCancel);
    kit.queue_job(b"module", b"input");
    let agent = kit.agent();
    agent.start_helper_session(kit.policy()).unwrap();
    until(|| kit.coord.count("renew") >= 2);
    let t = Instant::now();
    agent.stop_helper_session();
    let took = t.elapsed();
    eprintln!("stop with renewals running took {took:?}");
    assert!(took < Duration::from_secs(1));
    assert!(kit.runner.runs.load(Ordering::SeqCst) == 1);
    assert_eq!(kit.coord.count("complete"), 0);
}
