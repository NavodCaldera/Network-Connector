#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 1.L3 (laptop-gap G4): a result that finishes after the upload window closed is kept (encrypted) and
//! uploaded in the next window, under the same lease while it can be renewed, after a restart, or under a new
//! lease for the same task. Simulated window boundary with a settable clock.

mod common;

use chrono::{DateTime, NaiveTime, Utc};
use common::*;
use nc_agent::budget::{BudgetGate, NetworkBudget};
use nc_agent::coord::CoordError;
use nc_agent::probe::Clock;
use nc_agent::Activity;
use nc_protocol::pb::v1::LeaseFailReason;
use nc_tariff::{OutsideWindowRule, TrafficClass};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn parked_activity() -> Activity {
    Activity::Paused(nc_agent::session::PARKED_TEXT.into())
}

/// A clock the test moves by hand.
struct SetClock(Mutex<DateTime<Utc>>);
impl Clock for SetClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap()
    }
}
impl SetClock {
    fn set(&self, t: DateTime<Utc>) {
        *self.0.lock().unwrap() = t;
    }
}

/// 2026-11-02 at `h:m:s` UTC.
fn at(h: u32, m: u32, s: u32) -> DateTime<Utc> {
    noon().date_naive().and_hms_opt(h, m, s).unwrap().and_utc()
}

/// Windows 10:00-12:00:30 and 12:01:30-18:00 UTC; control traffic is allowed outside them (so leases can be
/// renewed), bulk and result uploads are not.
fn profile(remaining: u64) -> NetworkBudget {
    let mut p = metered(remaining);
    p.windows[0].end = NaiveTime::from_hms_opt(12, 0, 30).unwrap();
    let mut second = p.windows[0].clone();
    second.start = NaiveTime::from_hms_opt(12, 1, 30).unwrap();
    second.end = NaiveTime::from_hms_opt(18, 0, 0).unwrap();
    p.windows.push(second);
    p.outside_window = OutsideWindowRule::AllowClasses(vec![TrafficClass::Control]);
    NetworkBudget::Metered(p)
}

fn parked_count(kit: &Kit) -> usize {
    match std::fs::read(kit.dir.path().join("parked").join("parked.index.json")) {
        Ok(b) => serde_json::from_slice::<serde_json::Value>(&b).unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        Err(_) => 0,
    }
}

/// Start at 12:00:20, run a job across the 12:00:30 window end, check it is parked, not uploaded, not failed.
fn park_one(
    kit: &Kit,
    clock: &Arc<SetClock>,
) -> (nc_agent::Agent, nc_protocol::pb::v1::LeaseResponse) {
    kit.coord.s.lock().unwrap().clock = Some(clock.clone());
    let lease = kit.queue_job_expiring(
        b"\0asm-module",
        b"night result",
        at(12, 1, 20).timestamp() as u64,
    );
    let gate = BudgetGate::new(&profile(1 << 30), clock.now()).unwrap();
    let agent = kit.agent_with_gate(gate, clock.clone());
    agent.start_helper_session(kit.policy()).unwrap();
    until(|| kit.runner.runs.load(Ordering::SeqCst) == 1);
    // The window closes while the job runs.
    clock.set(at(12, 0, 40));
    until(|| agent.activity() == parked_activity());
    assert_eq!(parked_count(kit), 1);
    let s = kit.coord.s.lock().unwrap();
    assert!(s.puts.is_empty(), "no upload outside the window");
    assert!(s.completes.is_empty());
    assert!(s.fails.is_empty(), "no attempt burnt");
    drop(s);
    (agent, lease)
}

fn reversed(b: &[u8]) -> Vec<u8> {
    let mut v = b.to_vec();
    v.reverse();
    v
}

#[test]
fn result_crossing_the_window_end_is_parked_and_uploaded_in_the_next_window() {
    let kit = Kit::new(Mode::Sleep(300));
    let clock = Arc::new(SetClock(Mutex::new(at(12, 0, 20))));
    let (agent, lease) = park_one(&kit, &clock);
    // The lease is kept alive by renewals while parked.
    let renews = kit.coord.count("renew");
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        kit.coord.count("renew") > renews,
        "lease renewed while parked"
    );
    assert!(kit.coord.s.lock().unwrap().puts.is_empty());
    // Next window opens.
    clock.set(at(12, 1, 35));
    until(|| agent.today().jobs_done == 1);
    agent.stop_helper_session();
    let s = kit.coord.s.lock().unwrap();
    assert_eq!(s.completes.len(), 1);
    assert_eq!(s.completes[0].attempt_id, lease.attempt_id, "same lease");
    assert_eq!(s.puts.len(), 1);
    assert_eq!(s.puts.values().next().unwrap(), &reversed(b"night result"));
    assert!(s.fails.is_empty());
    drop(s);
    assert_eq!(kit.runner.runs.load(Ordering::SeqCst), 1, "not re-run");
    assert_eq!(parked_count(&kit), 0, "parked copy removed after commit");
    assert!(kit.journal().pending().unwrap().is_empty());
}

#[test]
fn restart_while_parked_resumes_from_the_store() {
    let kit = Kit::new(Mode::Sleep(300));
    let clock = Arc::new(SetClock(Mutex::new(at(12, 0, 20))));
    let (agent, lease) = park_one(&kit, &clock);
    let t = std::time::Instant::now();
    agent.stop_helper_session();
    assert!(
        t.elapsed() < Duration::from_secs(1),
        "Stop within 1 s while parked"
    );
    drop(agent);
    assert!(
        kit.coord.fails().is_empty(),
        "Stop keeps the parked attempt"
    );
    assert_eq!(parked_count(&kit), 1);

    // New process, window open again (the lease was renewed past 12:01:35 by the first run).
    clock.set(at(12, 1, 35));
    let gate = BudgetGate::new(&profile(1 << 30), clock.now()).unwrap();
    let agent = kit.agent_with_gate(gate, clock.clone());
    agent.start_helper_session(kit.policy()).unwrap();
    until(|| kit.coord.count("complete") == 1);
    agent.stop_helper_session();
    let s = kit.coord.s.lock().unwrap();
    assert_eq!(s.completes[0].attempt_id, lease.attempt_id);
    assert_eq!(s.puts.values().next().unwrap(), &reversed(b"night result"));
    assert!(s.fails.is_empty());
    drop(s);
    assert_eq!(kit.runner.runs.load(Ordering::SeqCst), 1, "not re-run");
    assert_eq!(parked_count(&kit), 0);
    assert!(kit.journal().pending().unwrap().is_empty());
}

#[test]
fn lease_lost_while_parked_keeps_the_result_for_a_new_lease_of_the_same_task() {
    let kit = Kit::new(Mode::Sleep(300));
    let clock = Arc::new(SetClock(Mutex::new(at(12, 0, 20))));
    let (agent, lease) = park_one(&kit, &clock);
    // The coordinator drops the lease (expired on its side).
    kit.coord.s.lock().unwrap().renew_mode = RenewMode::Error(CoordError::Status {
        status: 409,
        code: "lease_expired".into(),
    });
    until(|| agent.activity() != parked_activity());
    assert!(kit.coord.s.lock().unwrap().puts.is_empty());
    assert!(
        kit.coord.fails().is_empty(),
        "no Fail for a lease that is gone"
    );
    assert_eq!(parked_count(&kit), 1, "result kept");
    assert!(
        kit.journal().pending().unwrap().is_empty(),
        "old attempt closed"
    );

    // Next window: the coordinator hands the same task out again under a new lease.
    kit.coord.s.lock().unwrap().renew_mode = RenewMode::Extend;
    clock.set(at(12, 1, 35));
    let mut again = lease.clone();
    again.lease_id = random_uuid();
    again.attempt_id = random_uuid();
    again.expires_at = at(12, 2, 35).timestamp() as u64;
    {
        let mut s = kit.coord.s.lock().unwrap();
        s.expiry.insert(again.lease_id.clone(), again.expires_at);
        s.leases.push_back(again.clone());
    }
    until(|| agent.today().jobs_done == 1);
    agent.stop_helper_session();
    let s = kit.coord.s.lock().unwrap();
    assert_eq!(s.completes.len(), 1);
    assert_eq!(s.completes[0].attempt_id, again.attempt_id, "new lease");
    drop(s);
    assert_eq!(
        kit.runner.runs.load(Ordering::SeqCst),
        1,
        "parked result reused, not re-run"
    );
    assert_eq!(parked_count(&kit), 0);
}

#[test]
fn capped_lease_while_parked_is_released_with_a_tariff_window_fail() {
    let kit = Kit::new(Mode::Sleep(300));
    // The server cap never extends the lease past its first expiry (12:01:20); the agent releases it 5 s
    // before, while it is still valid.
    kit.coord.s.lock().unwrap().renew_mode = RenewMode::CapAt(at(12, 1, 20).timestamp() as u64);
    let clock = Arc::new(SetClock(Mutex::new(at(12, 0, 20))));
    let (agent, lease) = park_one(&kit, &clock);
    clock.set(at(12, 1, 16));
    until(|| !kit.coord.fails().is_empty());
    until(|| agent.activity() != parked_activity());
    agent.stop_helper_session();
    let f = kit.coord.fails();
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].attempt_id, lease.attempt_id);
    assert_eq!(f[0].reason, LeaseFailReason::TariffWindow as i32);
    assert_eq!(f[0].code.as_deref(), Some("PARKED_RELEASE"));
    assert!(kit.coord.s.lock().unwrap().puts.is_empty());
    assert_eq!(parked_count(&kit), 1, "result kept for a new lease");
}

#[test]
fn no_room_to_park_fails_the_lease_as_before() {
    let mut kit = Kit::new(Mode::Sleep(300));
    kit.park_quota = 16; // smaller than the encrypted output
    let clock = Arc::new(SetClock(Mutex::new(at(12, 0, 20))));
    kit.coord.s.lock().unwrap().clock = Some(clock.clone());
    kit.queue_job_expiring(
        b"\0asm-module",
        b"night result",
        at(12, 1, 20).timestamp() as u64,
    );
    let gate = BudgetGate::new(&profile(1 << 30), clock.now()).unwrap();
    let agent = kit.agent_with_gate(gate, clock.clone());
    agent.start_helper_session(kit.policy()).unwrap();
    until(|| kit.runner.runs.load(Ordering::SeqCst) == 1);
    clock.set(at(12, 0, 40));
    until(|| !kit.coord.fails().is_empty());
    agent.stop_helper_session();
    let f = kit.coord.fails();
    assert_eq!(f[0].reason, LeaseFailReason::TariffWindow as i32);
    assert_eq!(f[0].code.as_deref(), Some("UPLOAD_WINDOW"));
    assert!(kit.coord.s.lock().unwrap().puts.is_empty());
    assert_eq!(parked_count(&kit), 0);
}

#[test]
fn parked_upload_never_exceeds_the_budget() {
    // Budget covers the downloads and the result, so the job is admitted; another app spends it while the
    // result is parked: the next window refuses the upload and the result stays parked.
    let kit = Kit::new(Mode::Sleep(300));
    let clock = Arc::new(SetClock(Mutex::new(at(12, 0, 20))));
    kit.coord.s.lock().unwrap().clock = Some(clock.clone());
    kit.queue_job_expiring(
        b"\0asm-module",
        b"night result",
        at(12, 1, 20).timestamp() as u64,
    );
    let gate = BudgetGate::new(&profile(1 << 30), clock.now()).unwrap();
    let agent = kit.agent_with_gate(gate, clock.clone());
    agent.start_helper_session(kit.policy()).unwrap();
    until(|| kit.runner.runs.load(Ordering::SeqCst) == 1);
    clock.set(at(12, 0, 40));
    until(|| agent.activity() == parked_activity());
    // The owner lowers the remaining budget to almost nothing.
    agent.update_network_budget(profile(1)).unwrap();
    clock.set(at(12, 1, 35));
    std::thread::sleep(Duration::from_millis(300));
    agent.stop_helper_session();
    assert!(
        kit.coord.s.lock().unwrap().puts.is_empty(),
        "no result bytes over budget"
    );
    assert!(kit.coord.s.lock().unwrap().completes.is_empty());
    assert_eq!(parked_count(&kit), 1);
}
