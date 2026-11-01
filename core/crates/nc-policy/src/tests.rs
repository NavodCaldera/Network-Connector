use super::*;
use chrono::{DateTime, Duration as TimeDelta, NaiveTime, Utc, Weekday};
use nc_tariff::{Decision, DenyReason};
use proptest::prelude::*;
use std::time::Duration;

fn utc(text: &str) -> DateTime<Utc> {
    text.parse().unwrap()
}
fn key(byte: u8) -> PeerKey {
    PeerKey::from_bytes([byte; 32])
}
fn fixture() -> (OwnerPolicy, DeviceState, TaskOffer) {
    let now = utc("2026-01-05T18:00:00Z");
    let policy = OwnerPolicy {
        accept_jobs: true,
        max_concurrent_tasks: 2,
        max_memory_mb: 1024,
        max_disk_mb: 2048,
        timezone: chrono_tz::Asia::Colombo,
        time_windows: vec![WorkingWindow {
            days: vec![
                Weekday::Mon,
                Weekday::Tue,
                Weekday::Wed,
                Weekday::Thu,
                Weekday::Fri,
                Weekday::Sat,
                Weekday::Sun,
            ],
            start: NaiveTime::from_hms_opt(22, 0, 0).unwrap(),
            end: NaiveTime::from_hms_opt(6, 0, 0).unwrap(),
        }],
        trusted_peers: vec![key(1)],
        allowed_execution_profiles: vec!["cpu-core-v1".into()],
        max_task_duration: Duration::from_secs(3600),
        ..OwnerPolicy::default()
    };
    let state = DeviceState {
        now,
        on_ac_power: true,
        battery_percent: Some(50),
        thermal_level: Some(ThermalLevel::Nominal),
        user_idle_seconds: Some(600),
        running_tasks: vec![RunningTask {
            peer_key: key(1),
            profile: "cpu-core-v1".into(),
            started_at: now - TimeDelta::seconds(60),
        }],
        memory_in_use_mb: 100,
        disk_in_use_mb: 200,
        tariff_decision: None,
    };
    let offer = TaskOffer {
        peer_key: key(1),
        profile: "cpu-core-v1".into(),
        est_memory_mb: 64,
        est_disk_mb: 128,
        est_duration: Duration::from_secs(60),
    };
    (policy, state, offer)
}
fn reject(policy: &OwnerPolicy, state: &DeviceState, offer: &TaskOffer, reason: Reason) {
    assert_eq!(admit(policy, state, offer), Admission::Reject { reason });
}
fn pause(policy: &OwnerPolicy, state: &DeviceState, reason: Reason) {
    assert_eq!(
        check_running(policy, state),
        RunningDecision::Pause { reason }
    );
}
fn stop(policy: &OwnerPolicy, state: &DeviceState, reason: Reason) {
    assert_eq!(
        check_running(policy, state),
        RunningDecision::Stop { reason }
    );
}

#[test]
fn safe_defaults_reject_and_empty_serde_never_opts_in() {
    let (_, state, offer) = fixture();
    let policy = OwnerPolicy::default();
    assert!(policy.validate().is_ok());
    reject(&policy, &state, &offer, Reason::PolicyWithdrawn);
    stop(&policy, &state, Reason::PolicyWithdrawn);
    assert_eq!(serde_json::from_str::<OwnerPolicy>("{}").unwrap(), policy);
    assert_eq!(next_change(&policy, &state), None);
    let mut policy = policy;
    policy.accept_jobs = true;
    assert!(matches!(
        admit(&policy, &state, &offer),
        Admission::Reject { .. }
    ));
}

#[test]
fn fully_allowed_offer_and_running_task() {
    let (policy, state, offer) = fixture();
    assert_eq!(admit(&policy, &state, &offer), Admission::Accept);
    assert_eq!(check_running(&policy, &state), RunningDecision::Continue);
}

#[test]
fn policy_round_trip_uses_fixed_length_hex_keys() {
    let (mut policy, _, _) = fixture();
    policy.blocked_peers = vec![key(2)];
    let json = serde_json::to_string(&policy).unwrap();
    assert!(json.contains(&"01".repeat(32)));
    assert!(json.contains(&"02".repeat(32)));
    assert_eq!(serde_json::from_str::<OwnerPolicy>(&json).unwrap(), policy);
    let bytes = PeerKey::from_bytes([0xab; 32]);
    assert_eq!(bytes.as_bytes(), &[0xab; 32]);
    assert_eq!(
        serde_json::to_string(&bytes).unwrap(),
        format!("\"{}\"", "ab".repeat(32))
    );
    assert_eq!(
        serde_json::from_str::<PeerKey>(&format!("\"{}\"", "AB".repeat(32))).unwrap(),
        bytes
    );
    for invalid in [
        "00".to_owned(),
        "g0".repeat(32),
        "\u{e9}".repeat(32),
        "00".repeat(33),
    ] {
        assert!(serde_json::from_str::<PeerKey>(&format!("\"{invalid}\"")).is_err());
    }
    assert!(serde_json::from_str::<PeerKey>("[0,1,2]").is_err());
    assert!(
        serde_json::from_str::<OwnerPolicy>(&json.replace("Asia/Colombo", "Unknown/Zone")).is_err()
    );
    assert!(serde_json::from_str::<OwnerPolicy>("{\"accept_jobz\":true}").is_err());
}

#[test]
fn validation_is_specific_for_each_numeric_field() {
    let (good, state, offer) = fixture();
    let mut cases = Vec::new();
    let mut p = good.clone();
    p.version = 0;
    cases.push((p, ValidationError::Version));
    for cpu in [0, 101, 255] {
        let mut p = good.clone();
        p.cpu_budget_percent = cpu;
        cases.push((p, ValidationError::CpuBudget));
    }
    let mut p = good.clone();
    p.max_concurrent_tasks = 0;
    cases.push((p, ValidationError::Concurrency));
    let mut p = good.clone();
    p.max_memory_mb = 0;
    cases.push((p, ValidationError::Memory));
    let mut p = good.clone();
    p.max_disk_mb = 0;
    cases.push((p, ValidationError::Disk));
    let mut p = good.clone();
    p.min_battery_percent = 101;
    cases.push((p, ValidationError::Battery));
    let mut p = good.clone();
    p.idle_minutes = 0;
    cases.push((p, ValidationError::Idle));
    for duration in [Duration::ZERO, Duration::MAX] {
        let mut p = good.clone();
        p.max_task_duration = duration;
        cases.push((p, ValidationError::Duration));
    }
    for (p, error) in cases {
        assert_eq!(p.validate(), Err(error.clone()));
        reject(&p, &state, &offer, Reason::InvalidPolicy(error.clone()));
        stop(&p, &state, Reason::InvalidPolicy(error.clone()));
        assert!(!error.to_string().is_empty());
    }
    for cpu in [1, 100] {
        let mut p = good.clone();
        p.cpu_budget_percent = cpu;
        assert!(p.validate().is_ok());
    }
}

#[test]
fn profile_names_and_working_windows_validate() {
    let (good, _, _) = fixture();
    for names in [
        vec!["".into()],
        vec![" cpu-core-v1".into()],
        vec!["cpu-core-v1 ".into()],
        vec!["cpu-core-v1".into(), "cpu-core-v1".into()],
    ] {
        let mut p = good.clone();
        p.allowed_execution_profiles = names;
        assert_eq!(p.validate(), Err(ValidationError::Profiles));
    }
    let mut p = good.clone();
    p.time_windows[0].days.clear();
    assert!(matches!(p.validate(), Err(ValidationError::Hours(_))));
    let mut p = good.clone();
    p.time_windows[0].end = p.time_windows[0].start;
    assert!(matches!(p.validate(), Err(ValidationError::Hours(_))));
    let mut p = good.clone();
    p.time_windows.push(p.time_windows[0].clone());
    assert!(matches!(p.validate(), Err(ValidationError::Hours(_))));
    let mut p = good;
    p.allowed_execution_profiles.clear();
    p.time_windows.clear();
    assert!(p.validate().is_ok()); // Empty allowlists are valid, closed policies.
}

#[test]
fn consent_withdrawal_stops_before_temporary_pause() {
    let (mut policy, mut state, offer) = fixture();
    policy.accept_jobs = false;
    state.on_ac_power = false;
    reject(&policy, &state, &offer, Reason::PolicyWithdrawn);
    stop(&policy, &state, Reason::PolicyWithdrawn);
}

#[test]
fn blocked_beats_trusted_and_stops_before_outside_hours() {
    let (mut policy, mut state, offer) = fixture();
    policy.blocked_peers.push(offer.peer_key);
    state.now = utc("2026-01-05T10:00:00Z");
    state.running_tasks[0].started_at = state.now;
    reject(&policy, &state, &offer, Reason::BlockedPeer);
    stop(&policy, &state, Reason::BlockedPeer);
    state.running_tasks.clear(); // Candidate itself must be blocked even without an existing task.
    reject(&policy, &state, &offer, Reason::BlockedPeer);
}

#[test]
fn untrusted_peer_and_removed_profile_revoke_running_tasks() {
    let (mut policy, mut state, mut offer) = fixture();
    offer.peer_key = key(3);
    reject(&policy, &state, &offer, Reason::UntrustedPeer);
    offer.peer_key = key(1);
    policy.trusted_peers.clear();
    stop(&policy, &state, Reason::UntrustedPeer);
    policy.trusted_peers.push(key(1));
    offer.profile = "other-profile".into();
    reject(&policy, &state, &offer, Reason::ExecutionProfileNotAllowed);
    state.running_tasks[0].profile = "other-profile".into();
    stop(&policy, &state, Reason::ExecutionProfileNotAllowed);
}

#[test]
fn concurrency_admit_is_strict_and_running_cap_is_inclusive() {
    let (policy, mut state, offer) = fixture();
    state.running_tasks.push(state.running_tasks[0].clone());
    reject(&policy, &state, &offer, Reason::ConcurrencyLimit);
    assert_eq!(check_running(&policy, &state), RunningDecision::Continue);
    state.running_tasks.push(state.running_tasks[0].clone());
    pause(&policy, &state, Reason::ConcurrencyLimit);
}

#[test]
fn memory_exact_cap_overflow_and_revised_running_cap() {
    let (policy, mut state, mut offer) = fixture();
    offer.est_memory_mb = policy.max_memory_mb - state.memory_in_use_mb;
    assert_eq!(admit(&policy, &state, &offer), Admission::Accept);
    offer.est_memory_mb += 1;
    reject(&policy, &state, &offer, Reason::MemoryLimit);
    state.memory_in_use_mb = u64::MAX;
    offer.est_memory_mb = 1;
    reject(&policy, &state, &offer, Reason::MemoryLimit);
    pause(&policy, &state, Reason::MemoryLimit);
}

#[test]
fn disk_exact_cap_overflow_and_revised_running_cap() {
    let (policy, mut state, mut offer) = fixture();
    offer.est_disk_mb = policy.max_disk_mb - state.disk_in_use_mb;
    assert_eq!(admit(&policy, &state, &offer), Admission::Accept);
    offer.est_disk_mb += 1;
    reject(&policy, &state, &offer, Reason::DiskLimit);
    state.disk_in_use_mb = u64::MAX;
    offer.est_disk_mb = 1;
    reject(&policy, &state, &offer, Reason::DiskLimit);
    pause(&policy, &state, Reason::DiskLimit);
}

#[test]
fn estimated_and_elapsed_duration_are_checked_at_exact_boundary() {
    let (policy, mut state, mut offer) = fixture();
    offer.est_duration = Duration::ZERO;
    reject(&policy, &state, &offer, Reason::InvalidTaskDuration);
    offer.est_duration = policy.max_task_duration;
    assert_eq!(admit(&policy, &state, &offer), Admission::Accept);
    offer.est_duration += Duration::from_nanos(1);
    reject(&policy, &state, &offer, Reason::TaskDurationLimit);
    state.running_tasks[0].started_at = state.now - TimeDelta::seconds(3600);
    stop(&policy, &state, Reason::TaskDurationLimit);
    state.running_tasks[0].started_at += TimeDelta::nanoseconds(1);
    assert_eq!(check_running(&policy, &state), RunningDecision::Continue);
}

#[test]
fn backward_clock_and_unrepresentable_task_deadline_stop() {
    let (policy, mut state, offer) = fixture();
    state.running_tasks[0].started_at = state.now + TimeDelta::seconds(1);
    stop(&policy, &state, Reason::TaskClockInvalid);
    reject(&policy, &state, &offer, Reason::TaskClockInvalid);
    state.now = DateTime::<Utc>::MAX_UTC;
    state.running_tasks[0].started_at = state.now;
    stop(&policy, &state, Reason::TaskClockInvalid);
}

#[test]
fn unplugging_pauses_charging_only_work() {
    let (mut policy, mut state, offer) = fixture();
    state.on_ac_power = false;
    reject(&policy, &state, &offer, Reason::ChargingRequired);
    pause(&policy, &state, Reason::ChargingRequired);
    policy.charging_only = false;
    assert_eq!(admit(&policy, &state, &offer), Admission::Accept);
    assert_eq!(check_running(&policy, &state), RunningDecision::Continue);
}

#[test]
fn battery_exact_minimum_unknown_invalid_and_disabled() {
    let (mut policy, mut state, offer) = fixture();
    state.battery_percent = Some(policy.min_battery_percent);
    assert_eq!(admit(&policy, &state, &offer), Admission::Accept);
    assert_eq!(check_running(&policy, &state), RunningDecision::Continue);
    for (battery, reason) in [
        (Some(19), Reason::BatteryTooLow),
        (None, Reason::BatteryUnknown),
        (Some(101), Reason::InvalidBatteryReading),
    ] {
        state.battery_percent = battery;
        reject(&policy, &state, &offer, reason.clone());
        pause(&policy, &state, reason);
    }
    state.battery_percent = None;
    policy.min_battery_percent = 0;
    assert_eq!(admit(&policy, &state, &offer), Admission::Accept);
    assert_eq!(check_running(&policy, &state), RunningDecision::Continue);
}

#[test]
fn unknown_thermal_and_inclusive_threshold_pause() {
    let (mut policy, mut state, offer) = fixture();
    for (thermal, reason) in [
        (None, Reason::ThermalUnknown),
        (Some(ThermalLevel::Serious), Reason::ThermalThreshold),
    ] {
        state.thermal_level = thermal;
        reject(&policy, &state, &offer, reason.clone());
        pause(&policy, &state, reason);
    }
    for threshold in [
        ThermalLevel::Nominal,
        ThermalLevel::Fair,
        ThermalLevel::Serious,
    ] {
        policy.thermal_stop = threshold;
        state.thermal_level = Some(threshold);
        pause(&policy, &state, Reason::ThermalThreshold);
    }
    policy.thermal_stop = ThermalLevel::Critical;
    state.thermal_level = Some(ThermalLevel::Serious);
    assert_eq!(check_running(&policy, &state), RunningDecision::Continue);
}

#[test]
fn critical_thermal_always_stops_before_battery_or_activity_pause() {
    let (mut policy, mut state, offer) = fixture();
    for threshold in [
        ThermalLevel::Nominal,
        ThermalLevel::Fair,
        ThermalLevel::Serious,
        ThermalLevel::Critical,
    ] {
        policy.thermal_stop = threshold;
        state.thermal_level = Some(ThermalLevel::Critical);
        state.battery_percent = None;
        reject(&policy, &state, &offer, Reason::CriticalThermal);
        stop(&policy, &state, Reason::CriticalThermal);
    }
}

#[test]
fn user_activity_unknown_and_exact_idle_boundary() {
    let (mut policy, mut state, offer) = fixture();
    for (idle, reason) in [
        (None, Reason::UserActivityUnknown),
        (Some(299), Reason::UserActive),
    ] {
        state.user_idle_seconds = idle;
        reject(&policy, &state, &offer, reason.clone());
        pause(&policy, &state, reason);
    }
    state.user_idle_seconds = Some(300);
    assert_eq!(admit(&policy, &state, &offer), Admission::Accept);
    state.user_idle_seconds = None;
    policy.pause_on_user_activity = false;
    policy.idle_minutes = 0;
    assert!(policy.validate().is_ok());
    assert_eq!(check_running(&policy, &state), RunningDecision::Continue);
}

fn at(state: &DeviceState, now: &str) -> DeviceState {
    let mut state = state.clone();
    state.now = utc(now);
    for task in &mut state.running_tasks {
        task.started_at = state.now;
    }
    state
}

#[test]
fn colombo_cross_midnight_uses_start_weekday_and_exclusive_end() {
    let (mut policy, state, offer) = fixture();
    policy.time_windows[0].days = vec![Weekday::Mon];
    for now in ["2026-01-05T16:30:00Z", "2026-01-06T00:29:59Z"] {
        let state = at(&state, now);
        assert_eq!(admit(&policy, &state, &offer), Admission::Accept);
        assert_eq!(check_running(&policy, &state), RunningDecision::Continue);
    }
    for now in [
        "2026-01-05T16:29:59Z",
        "2026-01-06T00:30:00Z",
        "2026-01-06T17:00:00Z",
    ] {
        let state = at(&state, now);
        reject(&policy, &state, &offer, Reason::OutsideHours);
        pause(&policy, &state, Reason::OutsideHours);
    }
}

#[test]
fn london_dst_gaps_and_folds_are_conservative() {
    let (mut policy, state, offer) = fixture();
    policy.timezone = chrono_tz::Europe::London;
    policy.time_windows[0].start = NaiveTime::from_hms_opt(1, 30, 0).unwrap();
    policy.time_windows[0].end = NaiveTime::from_hms_opt(3, 0, 0).unwrap();
    for now in ["2026-03-29T01:00:00Z", "2026-10-25T00:45:00Z"] {
        let state = at(&state, now);
        reject(&policy, &state, &offer, Reason::OutsideHours);
    }
    let state = at(&state, "2026-10-25T01:30:00Z");
    assert_eq!(admit(&policy, &state, &offer), Admission::Accept);
    policy.time_windows[0].start = NaiveTime::from_hms_opt(0, 0, 0).unwrap();
    policy.time_windows[0].end = NaiveTime::from_hms_opt(1, 30, 0).unwrap();
    pause(&policy, &state, Reason::OutsideHours); // Earlier ambiguous end is used.
}

#[test]
fn empty_hours_and_calendar_extremes_fail_closed() {
    let (mut policy, mut state, offer) = fixture();
    policy.time_windows.clear();
    reject(&policy, &state, &offer, Reason::OutsideHours);
    pause(&policy, &state, Reason::OutsideHours);
    let (policy, _, _) = fixture();
    state.running_tasks.clear();
    for now in [DateTime::<Utc>::MIN_UTC, DateTime::<Utc>::MAX_UTC] {
        state.now = now;
        reject(&policy, &state, &offer, Reason::HoursUnavailable);
        pause(&policy, &state, Reason::HoursUnavailable);
        assert_eq!(next_change(&policy, &state), None);
    }
}

#[test]
fn every_real_tariff_outcome_is_respected() {
    let (policy, mut state, offer) = fixture();
    let cases = [
        (
            Decision::Deny {
                reason: DenyReason::NetworkMismatch,
            },
            Reason::TariffDenied,
        ),
        (
            Decision::Defer {
                next_open: Some(state.now + TimeDelta::hours(1)),
            },
            Reason::TariffDeferred,
        ),
        (
            Decision::Allow {
                until: state.now,
                max_bytes: 10,
            },
            Reason::TariffExpired,
        ),
        (
            Decision::Allow {
                until: state.now + TimeDelta::seconds(30),
                max_bytes: 0,
            },
            Reason::TariffCapacityUnavailable,
        ),
    ];
    for (tariff, reason) in cases {
        state.tariff_decision = Some(tariff);
        reject(&policy, &state, &offer, reason.clone());
        pause(&policy, &state, reason);
    }
    state.tariff_decision = Some(Decision::Allow {
        until: state.now + TimeDelta::seconds(30),
        max_bytes: 1,
    });
    assert_eq!(admit(&policy, &state, &offer), Admission::Accept);
    assert_eq!(check_running(&policy, &state), RunningDecision::Continue);
}

#[test]
fn actual_tariff_decide_integrates_without_bypassing_network_binding() {
    let (policy, mut state, offer) = fixture();
    let tariff = nc_tariff::sim::example_profile().unwrap();
    let ledger = nc_tariff::BudgetLedger::new(&tariff, state.now).unwrap();
    let mut network = nc_tariff::NetworkObservation {
        interface: nc_tariff::InterfaceClass::Wifi,
        connection_id: Some(tariff.binding.connection_id.clone()),
        bytes_per_second: 1000,
    };
    let transfer = nc_tariff::TransferRequest {
        class: nc_tariff::TrafficClass::ModuleDownload,
        bytes: 100,
    };
    state.tariff_decision = Some(nc_tariff::decide(
        &tariff,
        &ledger.snapshot().unwrap(),
        state.now,
        &network,
        transfer,
    ));
    assert_eq!(admit(&policy, &state, &offer), Admission::Accept);
    network.interface = nc_tariff::InterfaceClass::Cellular;
    state.tariff_decision = Some(nc_tariff::decide(
        &tariff,
        &ledger.snapshot().unwrap(),
        state.now,
        &network,
        transfer,
    ));
    reject(&policy, &state, &offer, Reason::TariffDenied);
    pause(&policy, &state, Reason::TariffDenied);
}

#[test]
fn next_change_selects_idle_deadline_hours_and_tariff_boundaries() {
    let (policy, mut state, _) = fixture();
    state.user_idle_seconds = Some(299);
    assert_eq!(
        next_change(&policy, &state),
        Some(state.now + TimeDelta::seconds(1))
    );
    state.user_idle_seconds = Some(300);
    state.tariff_decision = Some(Decision::Allow {
        until: state.now + TimeDelta::seconds(10),
        max_bytes: 1,
    });
    assert_eq!(
        next_change(&policy, &state),
        Some(state.now + TimeDelta::seconds(10))
    );
    state.tariff_decision = Some(Decision::Defer {
        next_open: Some(state.now + TimeDelta::seconds(20)),
    });
    assert_eq!(
        next_change(&policy, &state),
        Some(state.now + TimeDelta::seconds(20))
    );
    state.tariff_decision = None;
    assert_eq!(
        next_change(&policy, &state),
        Some(state.running_tasks[0].started_at + TimeDelta::seconds(3600))
    );
    state.running_tasks.clear();
    assert_eq!(
        next_change(&policy, &state),
        Some(utc("2026-01-06T00:30:00Z"))
    );
    let state = at(&state, "2026-01-06T10:00:00Z");
    assert_eq!(
        next_change(&policy, &state),
        Some(utc("2026-01-06T16:30:00Z"))
    );
}

#[test]
fn next_change_none_for_no_known_opening_and_ignores_past_tariff_time() {
    let (mut policy, mut state, _) = fixture();
    state.running_tasks.clear();
    policy.time_windows.clear();
    state.tariff_decision = Some(Decision::Defer { next_open: None });
    assert_eq!(next_change(&policy, &state), None);
    policy.blocked_peers.push(key(1));
    state.running_tasks.push(RunningTask {
        peer_key: key(1),
        profile: "cpu-core-v1".into(),
        started_at: state.now,
    });
    assert_eq!(next_change(&policy, &state), None);
    let (policy, mut state, _) = fixture();
    state.tariff_decision = Some(Decision::Defer {
        next_open: Some(state.now - TimeDelta::seconds(1)),
    });
    assert!(next_change(&policy, &state).is_some_and(|time| time > state.now));
}

#[test]
fn every_reason_has_plain_english_display_without_identifiers() {
    let reasons = vec![
        Reason::InvalidPolicy(ValidationError::CpuBudget),
        Reason::PolicyWithdrawn,
        Reason::BlockedPeer,
        Reason::UntrustedPeer,
        Reason::ExecutionProfileNotAllowed,
        Reason::ConcurrencyLimit,
        Reason::MemoryLimit,
        Reason::DiskLimit,
        Reason::InvalidTaskDuration,
        Reason::TaskDurationLimit,
        Reason::TaskClockInvalid,
        Reason::ChargingRequired,
        Reason::BatteryUnknown,
        Reason::InvalidBatteryReading,
        Reason::BatteryTooLow,
        Reason::ThermalUnknown,
        Reason::ThermalThreshold,
        Reason::CriticalThermal,
        Reason::UserActivityUnknown,
        Reason::UserActive,
        Reason::OutsideHours,
        Reason::HoursUnavailable,
        Reason::TariffDenied,
        Reason::TariffDeferred,
        Reason::TariffExpired,
        Reason::TariffCapacityUnavailable,
    ];
    for reason in reasons {
        let display = reason.to_string();
        assert!(display.len() > 15 && display.contains(' '));
        assert!(!display.contains(&"01".repeat(32)));
        assert!(!display.contains("cpu-core-v1"));
    }
    assert_eq!(
        Reason::PolicyWithdrawn.to_string(),
        "The owner has disabled helper work."
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]
    #[test]
    fn admit_never_accepts_when_any_hard_rule_fails(mask in prop_oneof![1u32..(1 << 20), (0u32..20).prop_map(|bit| 1u32 << bit)]) {
        let (mut p, mut s, mut offer) = fixture();
        s.running_tasks.clear();
        if mask & 1 != 0 { p.accept_jobs = false; }
        if mask & 2 != 0 { p.blocked_peers.push(offer.peer_key); }
        if mask & 4 != 0 { p.trusted_peers.clear(); }
        if mask & 8 != 0 { offer.profile = "forbidden-profile".into(); }
        if mask & 16 != 0 { s.running_tasks = (0..p.max_concurrent_tasks).map(|_| RunningTask { peer_key: key(1), profile: "cpu-core-v1".into(), started_at: s.now }).collect(); }
        if mask & 32 != 0 { offer.est_memory_mb = p.max_memory_mb + 1; }
        if mask & 64 != 0 { offer.est_disk_mb = p.max_disk_mb + 1; }
        if mask & 128 != 0 { offer.est_duration = p.max_task_duration + Duration::from_secs(1); }
        if mask & 256 != 0 { s.on_ac_power = false; }
        if mask & 512 != 0 { s.battery_percent = Some(p.min_battery_percent - 1); }
        if mask & 1024 != 0 { s.battery_percent = None; }
        if mask & 2048 != 0 { s.thermal_level = None; }
        if mask & 4096 != 0 { s.thermal_level = Some(ThermalLevel::Critical); }
        if mask & 8192 != 0 { s.user_idle_seconds = Some(0); }
        if mask & 16384 != 0 { s.user_idle_seconds = None; }
        if mask & 32768 != 0 { p.time_windows.clear(); }
        if mask & 65536 != 0 { s.tariff_decision = Some(Decision::Deny { reason: DenyReason::NetworkMismatch }); }
        if mask & 131072 != 0 { p.cpu_budget_percent = 0; }
        if mask & 262144 != 0 { offer.est_duration = Duration::ZERO; }
        if mask & 524288 != 0 { s.thermal_level = Some(ThermalLevel::Serious); }
        prop_assert_ne!(admit(&p, &s, &offer), Admission::Accept);
    }

    #[test]
    fn running_never_continues_for_blocked_peer_or_outside_hours(
        blocked in any::<bool>(), outside in any::<bool>(), second in 0u32..3600,
        peer in prop::array::uniform32(any::<u8>()),
    ) {
        let (mut p, mut s, _) = fixture();
        let peer = PeerKey::from_bytes(peer);
        p.trusted_peers = vec![peer]; s.running_tasks[0].peer_key = peer;
        if blocked { p.blocked_peers.push(peer); }
        if outside { s.now = utc("2026-01-05T10:00:00Z") + TimeDelta::seconds(i64::from(second)); }
        s.running_tasks[0].started_at = s.now;
        let outcome = check_running(&p, &s);
        if blocked || outside { prop_assert_ne!(outcome, RunningDecision::Continue); }
        else { prop_assert_eq!(outcome, RunningDecision::Continue); }
    }
}
