#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Helper session against an in-process fake coordinator.

mod common;

use common::*;
use nc_agent::budget::{BudgetGate, NetworkBudget};
use nc_agent::journal::Entry;
use nc_agent::probe::DeviceObservation;
use nc_agent::transfer::{self, TransferError};
use nc_agent::{Activity, SessionState};
use nc_protocol::pb::v1::{LeaseFailReason, TransferRef};
use nc_tariff::TrafficClass;
use sha2::{Digest, Sha256};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn reason(r: LeaseFailReason) -> i32 {
    r as i32
}

#[test]
fn happy_path_runs_uploads_and_completes_once() {
    let kit = Kit::new(Mode::Reverse);
    kit.queue_job(b"\0asm-module", b"hello");
    let agent = kit.agent();
    agent.start_helper_session(kit.policy()).unwrap();
    until(|| agent.today().jobs_done == 1);
    agent.stop_helper_session();
    let s = kit.coord.s.lock().unwrap();
    assert_eq!(s.completes.len(), 1);
    let c = &s.completes[0];
    assert_eq!(c.version, 2);
    assert!(c.result.is_empty());
    assert_eq!(c.output_size, 5);
    assert_eq!(c.output_sha256, hex::encode(Sha256::digest(b"olleh")));
    assert_eq!(s.puts.get(&c.output_sha256).unwrap(), b"olleh");
    assert!(s.fails.is_empty());
    assert_eq!(kit.runner.runs.load(Ordering::SeqCst), 1);
}

#[test]
fn policy_refusal_means_no_lease_request() {
    let kit = Kit::new(Mode::Reverse);
    kit.queue_job(b"module", b"input");
    // Charging-only policy on battery power.
    kit.probe.set_device(DeviceObservation {
        on_ac_power: false,
        battery_percent: Some(90),
        ..good_device()
    });
    let mut policy = kit.policy();
    policy.charging_only = true;
    let agent = kit.agent();
    agent.start_helper_session(policy).unwrap();
    until(|| matches!(agent.activity(), Activity::Paused(_)));
    std::thread::sleep(Duration::from_millis(200));
    assert!(kit.coord.count("register") >= 1);
    assert_eq!(kit.coord.count("lease"), 0, "{:?}", kit.coord.calls());
    // Consent off: also no lease, and the facade says NotAccepting.
    let rx = agent.watch_status();
    let mut off = kit.policy();
    off.accept_jobs = false;
    agent.update_policy(off).unwrap();
    loop {
        if rx.recv_timeout(Duration::from_secs(5)).unwrap().state == SessionState::NotAccepting {
            break;
        }
    }
    assert_eq!(kit.coord.count("lease"), 0);
    agent.stop_helper_session();
    assert_eq!(kit.runner.runs.load(Ordering::SeqCst), 0);
}

#[test]
fn untrusted_publisher_is_refused_before_download() {
    let kit = Kit::new(Mode::Reverse);
    kit.queue_job(b"module", b"input");
    let agent = kit.agent();
    // Trust some other key only.
    agent
        .start_helper_session(policy_trusting([9u8; 32]))
        .unwrap();
    until(|| !kit.coord.fails().is_empty());
    agent.stop_helper_session();
    let f = &kit.coord.fails()[0];
    assert_eq!(f.reason, reason(LeaseFailReason::PolicyRefused));
    assert_eq!(f.code.as_deref(), Some("UNTRUSTED_PUBLISHER"));
    assert_eq!(kit.coord.count("get:"), 0);
    assert_eq!(kit.runner.runs.load(Ordering::SeqCst), 0);
}

#[test]
fn manifest_verify_failure_reports_verify_failed_and_runs_nothing() {
    for case in 0..4 {
        let kit = Kit::new(Mode::Reverse);
        let mut lease = kit.queue_job(b"module", b"input");
        kit.coord.s.lock().unwrap().leases.clear();
        match case {
            0 => lease.manifest.as_mut().unwrap().signature[0] ^= 1,
            1 => lease.manifest.as_mut().unwrap().manifest[3] ^= 1,
            // Transfer path pointing elsewhere.
            2 => lease.transfers[0].path = "/v1/jobs/other".into(),
            // Transfer hash that does not match the signed manifest.
            _ => lease.transfers[1].sha256 = hex::encode([1u8; 32]),
        }
        kit.coord.s.lock().unwrap().leases.push_back(lease);
        let agent = kit.agent();
        agent.start_helper_session(kit.policy()).unwrap();
        until(|| !kit.coord.fails().is_empty());
        agent.stop_helper_session();
        let f = &kit.coord.fails()[0];
        assert_eq!(
            f.reason,
            reason(LeaseFailReason::VerifyFailed),
            "case {case}"
        );
        assert_eq!(
            kit.coord.count("get:"),
            0,
            "nothing downloaded, case {case}"
        );
        assert_eq!(kit.runner.runs.load(Ordering::SeqCst), 0, "case {case}");
    }
}

#[test]
fn module_bytes_with_wrong_hash_are_refused() {
    let kit = Kit::new(Mode::Reverse);
    let lease = kit.queue_job(b"module", b"input");
    // The server serves different bytes for the module path.
    kit.coord
        .s
        .lock()
        .unwrap()
        .override_body
        .insert(lease.transfers[0].path.clone(), b"modulX".to_vec());
    let agent = kit.agent();
    agent.start_helper_session(kit.policy()).unwrap();
    until(|| !kit.coord.fails().is_empty());
    agent.stop_helper_session();
    let f = &kit.coord.fails()[0];
    assert_eq!(f.reason, reason(LeaseFailReason::VerifyFailed));
    assert_eq!(f.code.as_deref(), Some("BLOB_HASH"));
    assert_eq!(kit.runner.runs.load(Ordering::SeqCst), 0);
    assert_eq!(staging_parts(kit.dir.path()), 0, "bad partial removed");
}

#[test]
fn interrupted_download_resumes_with_range() {
    let kit = Kit::new(Mode::Reverse);
    let module: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    let lease = kit.queue_job(&module, b"input-bytes");
    kit.coord
        .s
        .lock()
        .unwrap()
        .cut_once
        .insert(lease.transfers[0].path.clone(), 100_000);
    let agent = kit.agent();
    agent.start_helper_session(kit.policy()).unwrap();
    until(|| agent.today().jobs_done == 1);
    agent.stop_helper_session();
    let gets: Vec<String> = kit
        .coord
        .calls()
        .into_iter()
        .filter(|c| c.starts_with(&format!("get:{}", lease.transfers[0].path)))
        .collect();
    assert_eq!(gets.len(), 2, "{gets:?}");
    assert!(gets[0].ends_with("@0"));
    assert!(
        gets[1].ends_with("@100000"),
        "resumed from the cut: {gets:?}"
    );
    assert_eq!(
        kit.runner.seen_input.lock().unwrap().as_slice(),
        b"input-bytes"
    );
    assert!(kit.coord.fails().is_empty());
}

#[test]
fn budget_too_small_refuses_before_any_transfer() {
    let kit = Kit::new(Mode::Reverse);
    kit.queue_job(&vec![1u8; 200_000], b"input");
    let clock = Arc::new(FixedClock(noon()));
    // Enough for control traffic, not for the job (module + input + 1 MiB output allowance).
    let agent = kit.agent_with(NetworkBudget::Metered(metered(500_000)), clock);
    agent.start_helper_session(kit.policy()).unwrap();
    until(|| !kit.coord.fails().is_empty());
    agent.stop_helper_session();
    let f = &kit.coord.fails()[0];
    assert_eq!(f.reason, reason(LeaseFailReason::Budget));
    assert_eq!(kit.coord.count("get:"), 0);
    assert_eq!(kit.runner.runs.load(Ordering::SeqCst), 0);
}

#[test]
fn outside_tariff_window_no_contact_at_all() {
    let kit = Kit::new(Mode::Reverse);
    kit.queue_job(b"module", b"input");
    // 20:00 UTC is outside the 10:00-14:00 window; outside_window = Block.
    let night = noon() + chrono::Duration::hours(8);
    let agent = kit.agent_with(
        NetworkBudget::Metered(metered(1 << 30)),
        Arc::new(FixedClock(night)),
    );
    agent.start_helper_session(kit.policy()).unwrap();
    until(|| matches!(agent.activity(), Activity::Paused(_)));
    std::thread::sleep(Duration::from_millis(150));
    agent.stop_helper_session();
    assert!(kit.coord.calls().is_empty(), "{:?}", kit.coord.calls());
}

fn transfer_ref(kit: &Kit, bytes: &[u8]) -> TransferRef {
    let sha = hex::encode(Sha256::digest(bytes));
    let path = format!("/v1/blobs/{sha}");
    kit.coord
        .s
        .lock()
        .unwrap()
        .blobs
        .insert(path.clone(), bytes.to_vec());
    TransferRef {
        sha256: sha,
        size: bytes.len() as u64,
        path,
    }
}

#[test]
fn window_end_stops_transfer_and_charges_only_moved_bytes() {
    let kit = Kit::new(Mode::Reverse);
    let data = vec![7u8; 2 * 1024 * 1024];
    let r = transfer_ref(&kit, &data);
    // Window ends at 14:00; start at 13:59:55 and advance 1 s per clock read.
    let start = noon() + chrono::Duration::seconds(2 * 3600 - 5);
    let clock = StepClock {
        t: std::sync::Mutex::new(start),
        step: chrono::Duration::seconds(1),
    };
    let budget = BudgetGate::new(&NetworkBudget::Metered(metered(1 << 30)), start).unwrap();
    let before = budget.available_bytes().unwrap();
    let staging = kit.dir.path().join("staging");
    std::fs::create_dir_all(&staging).unwrap();
    let never = || false;
    let ctx = transfer::Ctx {
        coord: kit.coord.as_ref(),
        budget: &budget,
        probe: kit.probe.as_ref(),
        clock: &clock,
        stopped: &never,
        staging: &staging,
    };
    let e = transfer::download(&ctx, &r, TrafficClass::Bulk).unwrap_err();
    assert_eq!(e, TransferError::Window("WINDOW_ENDED"));
    let part = std::fs::metadata(staging.join(format!("{}.part", r.sha256)))
        .unwrap()
        .len();
    assert!(part > 0 && part < data.len() as u64, "partial kept: {part}");
    assert_eq!(
        before - budget.available_bytes().unwrap(),
        part,
        "charged exactly the bytes moved"
    );
    // Next night (window open again) the download resumes from the partial file.
    let next = noon() + chrono::Duration::days(1);
    let clock2 = FixedClock(next);
    let ctx2 = transfer::Ctx {
        clock: &clock2,
        ..ctx
    };
    let bytes = transfer::download(&ctx2, &r, TrafficClass::Bulk).unwrap();
    assert_eq!(bytes, data);
    assert!(kit
        .coord
        .calls()
        .iter()
        .any(|c| c.ends_with(&format!("@{part}"))));
    assert_eq!(
        before - budget.available_bytes().unwrap(),
        data.len() as u64
    );
}

#[test]
fn wifi_to_cellular_switch_stops_transfer() {
    let kit = Kit::new(Mode::Reverse);
    let data = vec![3u8; 1024 * 1024];
    let r = transfer_ref(&kit, &data);
    let budget = BudgetGate::new(&NetworkBudget::Metered(metered(1 << 30)), noon()).unwrap();
    let staging = kit.dir.path().join("staging");
    std::fs::create_dir_all(&staging).unwrap();
    let probe = kit.probe.clone();
    let reads = std::sync::atomic::AtomicUsize::new(0);
    // Switch networks after a few steps (the stop check runs between steps).
    let stopped = || {
        if reads.fetch_add(1, Ordering::SeqCst) == 4 {
            probe.set_network(cellular());
        }
        false
    };
    let clock = FixedClock(noon());
    let ctx = transfer::Ctx {
        coord: kit.coord.as_ref(),
        budget: &budget,
        probe: kit.probe.as_ref(),
        clock: &clock,
        stopped: &stopped,
        staging: &staging,
    };
    let e = transfer::download(&ctx, &r, TrafficClass::Bulk).unwrap_err();
    assert_eq!(e, TransferError::Window("NETWORK_CHANGED"));
    // And on cellular a new transfer is not authorised at all (no cellular fallback in the profile).
    let e = transfer::download(&ctx, &r, TrafficClass::Bulk).unwrap_err();
    assert_eq!(e, TransferError::Window("TARIFF_WINDOW"));
}

#[test]
fn budget_exhaustion_stops_a_transfer() {
    let kit = Kit::new(Mode::Reverse);
    let data = vec![5u8; 300_000];
    let r = transfer_ref(&kit, &data);
    let budget = BudgetGate::new(&NetworkBudget::Metered(metered(200_000)), noon()).unwrap();
    let staging = kit.dir.path().join("staging");
    std::fs::create_dir_all(&staging).unwrap();
    let never = || false;
    let clock = FixedClock(noon());
    let ctx = transfer::Ctx {
        coord: kit.coord.as_ref(),
        budget: &budget,
        probe: kit.probe.as_ref(),
        clock: &clock,
        stopped: &never,
        staging: &staging,
    };
    assert_eq!(
        transfer::download(&ctx, &r, TrafficClass::Bulk).unwrap_err(),
        TransferError::Budget
    );
    assert_eq!(
        kit.coord.count("get:"),
        0,
        "no byte requested past the budget"
    );
    assert_eq!(budget.available_bytes(), Some(200_000), "nothing charged");
}

#[test]
fn server_sending_too_much_is_cut_at_the_reservation() {
    let kit = Kit::new(Mode::Reverse);
    let data = vec![5u8; 100_000];
    let r = transfer_ref(&kit, &data);
    let mut longer = data.clone();
    longer.extend(vec![6u8; 500_000]);
    kit.coord
        .s
        .lock()
        .unwrap()
        .override_body
        .insert(r.path.clone(), longer);
    let budget = BudgetGate::new(&NetworkBudget::Metered(metered(1 << 30)), noon()).unwrap();
    let before = budget.available_bytes().unwrap();
    let staging = kit.dir.path().join("staging");
    std::fs::create_dir_all(&staging).unwrap();
    let never = || false;
    let clock = FixedClock(noon());
    let ctx = transfer::Ctx {
        coord: kit.coord.as_ref(),
        budget: &budget,
        probe: kit.probe.as_ref(),
        clock: &clock,
        stopped: &never,
        staging: &staging,
    };
    let bytes = transfer::download(&ctx, &r, TrafficClass::Bulk).unwrap();
    assert_eq!(bytes, data);
    assert_eq!(before - budget.available_bytes().unwrap(), 100_000);
}

#[test]
fn stop_during_run_kills_within_one_second_and_reports_stopped_by_owner() {
    let kit = Kit::new(Mode::UntilCancel);
    kit.queue_job(b"module", b"input");
    let agent = kit.agent();
    agent.start_helper_session(kit.policy()).unwrap();
    until(|| agent.activity() == Activity::Running);
    let t = Instant::now();
    agent.stop_helper_session();
    let stop_returned = t.elapsed();
    let killed = kit.runner.cancelled_at.lock().unwrap().unwrap() - t;
    eprintln!("stop returned after {stop_returned:?}; job saw cancel after {killed:?}");
    assert!(stop_returned < Duration::from_secs(1));
    assert!(killed < Duration::from_secs(1));
    until(|| !kit.coord.fails().is_empty());
    let f = &kit.coord.fails()[0];
    assert_eq!(f.reason, reason(LeaseFailReason::StoppedByOwner));
    assert_eq!(f.code.as_deref(), Some("STOP"));
    assert_eq!(kit.coord.count("complete"), 0);
}

#[test]
fn consent_off_during_run_cancels_the_job() {
    let kit = Kit::new(Mode::UntilCancel);
    kit.queue_job(b"module", b"input");
    let agent = kit.agent();
    agent.start_helper_session(kit.policy()).unwrap();
    until(|| agent.activity() == Activity::Running);
    let t = Instant::now();
    let mut off = kit.policy();
    off.accept_jobs = false;
    agent.update_policy(off).unwrap();
    until(|| !kit.coord.fails().is_empty());
    let killed = kit.runner.cancelled_at.lock().unwrap().unwrap() - t;
    assert!(killed < Duration::from_secs(1), "{killed:?}");
    let f = &kit.coord.fails()[0];
    assert_eq!(f.reason, reason(LeaseFailReason::StoppedByOwner));
    assert_eq!(f.code.as_deref(), Some("POLICY"));
    agent.stop_helper_session();
}

fn uuid(n: u8) -> String {
    uuid::Uuid::from_bytes([n; 16]).to_string()
}

#[test]
fn crash_recovery_fails_unfinished_and_resends_uploaded() {
    let kit = Kit::new(Mode::Reverse);
    {
        // A previous run: one attempt crashed mid-run, one crashed after uploading its result.
        let mut j = kit.journal();
        let mid = j
            .begin(
                Entry {
                    lease_id: uuid(1),
                    attempt_id: uuid(2),
                    task_id: uuid(3),
                    output_sha256: None,
                    output_size: None,
                },
                1,
            )
            .unwrap();
        j.start(mid, 2).unwrap();
        let up = j
            .begin(
                Entry {
                    lease_id: uuid(4),
                    attempt_id: uuid(5),
                    task_id: uuid(6),
                    output_sha256: None,
                    output_size: None,
                },
                3,
            )
            .unwrap();
        j.start(up, 4).unwrap();
        j.uploaded(up, [9u8; 32], 12, 5).unwrap();
    }
    let agent = kit.agent();
    agent.start_helper_session(kit.policy()).unwrap();
    until(|| kit.coord.count("complete") == 1 && !kit.coord.fails().is_empty());
    until(|| agent.today().jobs_done == 1);
    agent.stop_helper_session();
    let s = kit.coord.s.lock().unwrap();
    assert_eq!(s.fails.len(), 1);
    assert_eq!(s.fails[0].attempt_id, uuid(2));
    assert_eq!(s.fails[0].reason, reason(LeaseFailReason::Crashed));
    assert_eq!(s.fails[0].code.as_deref(), Some("AGENT_RESTART"));
    assert_eq!(s.completes[0].attempt_id, uuid(5));
    assert_eq!(s.completes[0].output_sha256, hex::encode([9u8; 32]));
    assert_eq!(s.completes[0].output_size, 12);
    drop(s);
    let mut j = kit.journal();
    assert!(j.pending().unwrap().is_empty(), "journal is clean");
    assert_eq!(kit.runner.runs.load(Ordering::SeqCst), 0, "nothing re-run");
}

#[test]
fn lost_complete_is_resent_and_counted_once() {
    let kit = Kit::new(Mode::Reverse);
    kit.queue_job(b"module", b"input");
    // The coordinator records the first Complete but the reply is lost.
    kit.coord.s.lock().unwrap().complete_lost = 1;
    let agent = kit.agent();
    agent.start_helper_session(kit.policy()).unwrap();
    until(|| kit.coord.count("complete") >= 2);
    until(|| agent.today().jobs_done == 1);
    std::thread::sleep(Duration::from_millis(100));
    agent.stop_helper_session();
    let s = kit.coord.s.lock().unwrap();
    assert_eq!(s.completes.len(), 2, "resent exactly once");
    assert_eq!(s.completes[0], s.completes[1], "identical payload");
    assert_eq!(agent.today().jobs_done, 1, "counted once");
    assert_eq!(kit.runner.runs.load(Ordering::SeqCst), 1, "not re-run");
}
