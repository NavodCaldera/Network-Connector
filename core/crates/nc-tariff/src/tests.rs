use super::*;
use chrono::{Datelike, Duration, NaiveTime, Utc, Weekday};
use proptest::prelude::*;

fn utc(text: &str) -> chrono::DateTime<Utc> {
    text.parse().unwrap()
}
fn profile() -> TariffProfile {
    sim::example_profile().unwrap()
}
fn network(p: &TariffProfile) -> NetworkObservation {
    NetworkObservation {
        interface: p.binding.interface,
        connection_id: Some(p.binding.connection_id.clone()),
        bytes_per_second: 1_000_000,
    }
}
fn request(bytes: u64) -> TransferRequest {
    TransferRequest {
        class: TrafficClass::Bulk,
        bytes,
    }
}
fn evaluate(p: &TariffProfile, time: &str, bytes: u64) -> Decision {
    let now = utc(time);
    decide(
        p,
        &BudgetLedger::new(p, now).unwrap().snapshot().unwrap(),
        now,
        &network(p),
        request(bytes),
    )
}

#[test]
fn json_yaml_round_trip_and_invalid_timezone() {
    let p = profile();
    let json = serde_json::to_string(&p).unwrap();
    assert_eq!(serde_json::from_str::<TariffProfile>(&json).unwrap(), p);
    let yaml = serde_saphyr::to_string(&p).unwrap();
    assert_eq!(serde_saphyr::from_str::<TariffProfile>(&yaml).unwrap(), p);
    assert!(serde_json::from_str::<TariffProfile>(
        &json.replace("Asia/Colombo", "Unknown/Nowhere")
    )
    .is_err());
}

#[test]
fn colombo_cross_midnight_and_start_weekday() {
    let mut p = profile();
    p.windows[0].days = vec![Weekday::Mon];
    // Monday 22:00 local through Tuesday 05:59:30 local.
    assert!(matches!(
        evaluate(&p, "2026-01-05T16:30:00Z", 10),
        Decision::Allow { .. }
    ));
    assert!(matches!(
        evaluate(&p, "2026-01-06T00:00:00Z", 10),
        Decision::Allow { .. }
    ));
    assert!(matches!(
        evaluate(&p, "2026-01-06T00:30:00Z", 10),
        Decision::Defer { .. }
    ));
    assert!(matches!(
        evaluate(&p, "2026-01-06T17:00:00Z", 10),
        Decision::Defer { .. }
    ));
}

#[test]
fn boundary_guard_is_hard_even_for_control_exception() {
    let mut p = profile();
    p.policy.strict_zero_outside_window = false;
    p.outside_window = OutsideWindowRule::AllowClasses(vec![TrafficClass::Control]);
    let now = utc("2026-01-06T00:29:30Z");
    let state = BudgetLedger::new(&p, now).unwrap().snapshot().unwrap();
    assert!(matches!(
        decide(
            &p,
            &state,
            now,
            &network(&p),
            TransferRequest {
                class: TrafficClass::Control,
                bytes: 1
            }
        ),
        Decision::Defer { .. }
    ));
    assert!(matches!(
        evaluate(&p, "2026-01-06T00:29:29Z", 1),
        Decision::Allow { .. }
    ));
}

#[test]
fn transfer_must_finish_before_guard_and_unknown_throughput_defers() {
    let p = profile();
    let now = utc("2026-01-06T00:29:29Z");
    let state = BudgetLedger::new(&p, now).unwrap().snapshot().unwrap();
    let mut net = network(&p);
    net.bytes_per_second = 10;
    assert!(matches!(
        decide(&p, &state, now, &net, request(10)),
        Decision::Allow { .. }
    ));
    assert!(matches!(
        decide(&p, &state, now, &net, request(11)),
        Decision::Defer { .. }
    ));
    net.bytes_per_second = 0;
    assert_eq!(
        decide(&p, &state, now, &net, request(1)),
        Decision::Defer { next_open: None }
    );
}

#[test]
fn exact_safety_reserve_and_rounding() {
    let mut p = profile();
    p.billing.remaining_bytes = 101;
    p.billing.safety_reserve_percent = 10;
    assert_eq!(p.spendable_bytes(), 90);
    let now = utc("2026-01-05T18:00:00Z");
    let ledger = BudgetLedger::new(&p, now).unwrap();
    let id = ledger.reserve(90).unwrap();
    assert_eq!(ledger.reserve(1), Err(TariffError::InsufficientBudget));
    ledger.commit(id, 90).unwrap();
    assert_eq!(ledger.snapshot().unwrap().available_bytes(), 0);
    p.billing.safety_reserve_percent = 100;
    assert_eq!(p.spendable_bytes(), 0);
    p.billing.remaining_bytes = u64::MAX;
    p.billing.safety_reserve_percent = 1;
    assert_eq!(p.spendable_bytes(), u64::MAX - u64::MAX.div_ceil(100));
}

#[test]
fn strict_mode_blocks_all_five_classes() {
    let mut p = profile();
    let classes = vec![
        TrafficClass::Control,
        TrafficClass::Bulk,
        TrafficClass::ModuleDownload,
        TrafficClass::ResultUpload,
        TrafficClass::Recovery,
    ];
    p.outside_window = OutsideWindowRule::AllowClasses(classes.clone());
    let now = utc("2026-01-05T10:00:00Z");
    let state = BudgetLedger::new(&p, now).unwrap().snapshot().unwrap();
    for class in classes {
        assert!(matches!(
            decide(
                &p,
                &state,
                now,
                &network(&p),
                TransferRequest { class, bytes: 1 }
            ),
            Decision::Defer { .. }
        ));
    }
    p.policy.strict_zero_outside_window = false;
    p.outside_window = OutsideWindowRule::AllowClasses(vec![TrafficClass::Control]);
    let state = BudgetLedger::new(&p, now).unwrap().snapshot().unwrap();
    assert!(matches!(
        decide(
            &p,
            &state,
            now,
            &network(&p),
            TransferRequest {
                class: TrafficClass::Control,
                bytes: 1
            }
        ),
        Decision::Allow { .. }
    ));
    assert!(matches!(
        decide(&p, &state, now, &network(&p), request(1)),
        Decision::Defer { .. }
    ));
}

#[test]
fn wifi_to_cellular_switch_requires_explicit_fallback() {
    let mut p = profile();
    let now = utc("2026-01-05T18:00:00Z");
    let ledger = BudgetLedger::new(&p, now).unwrap();
    let id = ledger
        .reserve_transfer(now, &network(&p), request(100))
        .unwrap();
    let mut cellular = network(&p);
    cellular.interface = InterfaceClass::Cellular;
    cellular.connection_id = Some("confirmed-cellular".into());
    assert_eq!(
        decide(&p, &ledger.snapshot().unwrap(), now, &cellular, request(50)),
        Decision::Deny {
            reason: DenyReason::NetworkMismatch
        }
    );
    ledger.commit(id, 40).unwrap();
    p.policy.allow_cellular_fallback = true;
    let state = BudgetLedger::new(&p, now).unwrap().snapshot().unwrap();
    assert!(matches!(
        decide(&p, &state, now, &cellular, request(50)),
        Decision::Allow { .. }
    ));
    cellular.connection_id = None;
    assert_eq!(
        decide(&p, &state, now, &cellular, request(50)),
        Decision::Deny {
            reason: DenyReason::NetworkMismatch
        }
    );
}

#[test]
fn wrong_binding_and_profile_revision_fail_closed() {
    let mut p = profile();
    let now = utc("2026-01-05T18:00:00Z");
    let state = BudgetLedger::new(&p, now).unwrap().snapshot().unwrap();
    let mut net = network(&p);
    net.connection_id = Some("same-ssid-but-unconfirmed-other-connection".into());
    assert_eq!(
        decide(&p, &state, now, &net, request(1)),
        Decision::Deny {
            reason: DenyReason::NetworkMismatch
        }
    );
    p.version += 1;
    assert_eq!(
        decide(&p, &state, now, &network(&p), request(1)),
        Decision::Deny {
            reason: DenyReason::ProfileMismatch
        }
    );
}

#[test]
fn backward_clock_does_not_restore_budget_and_forward_reset_is_explicit() {
    let p = profile();
    let now = utc("2026-01-05T18:00:00Z");
    let ledger = BudgetLedger::new(&p, now).unwrap();
    let id = ledger.reserve(100).unwrap();
    ledger.commit(id, 100).unwrap();
    assert!(ledger.advance(now - Duration::days(40)).is_err());
    assert_eq!(
        decide(
            &p,
            &ledger.snapshot().unwrap(),
            now - Duration::seconds(1),
            &network(&p),
            request(1)
        ),
        Decision::Deny {
            reason: DenyReason::ClockWentBackwards
        }
    );
    ledger.advance(now + Duration::days(70)).unwrap();
    assert_eq!(ledger.snapshot().unwrap().committed_bytes(), 0);
    assert!(ledger.advance(now).is_err());
}

#[test]
fn month_reset_is_local_and_blocks_unsettled_reservations() {
    let p = profile();
    let now = utc("2026-01-31T18:29:59Z");
    let ledger = BudgetLedger::new(&p, now).unwrap();
    let id = ledger.reserve(123).unwrap();
    assert!(ledger.advance(now + Duration::seconds(1)).is_err());
    ledger.commit(id, 125).unwrap();
    ledger.advance(now + Duration::seconds(1)).unwrap();
    assert_eq!(
        ledger.snapshot().unwrap().available_bytes(),
        p.spendable_bytes()
    );
    assert_eq!(ledger.commit(id, 125), Err(TariffError::UnknownReservation));
}

#[test]
fn reset_day_clamps_in_february_and_leap_year() {
    let mut p = profile();
    p.billing.reset_rule.day_of_month = 31;
    for (start, before, after) in [
        (
            "2026-02-01T00:00:00Z",
            "2026-02-28T18:29:59Z",
            "2026-02-28T18:30:00Z",
        ),
        (
            "2024-02-01T00:00:00Z",
            "2024-02-29T18:29:59Z",
            "2024-02-29T18:30:00Z",
        ),
    ] {
        let ledger = BudgetLedger::new(&p, utc(start)).unwrap();
        let id = ledger.reserve(100).unwrap();
        ledger.commit(id, 100).unwrap();
        // Colombo midnight on the final day is 18:30 UTC on the preceding day.
        let before = utc(before) - Duration::days(1);
        let after = utc(after) - Duration::days(1);
        ledger.advance(before).unwrap();
        assert_eq!(ledger.snapshot().unwrap().committed_bytes(), 100);
        ledger.advance(after).unwrap();
        assert_eq!(ledger.snapshot().unwrap().committed_bytes(), 0);
    }
}

#[test]
fn london_dst_overnight_has_real_elapsed_duration() {
    let mut p = profile();
    p.timezone = chrono_tz::Europe::London;
    p.policy.boundary_guard_seconds = 0;
    for (start, expected_hours) in [("2026-03-28T22:00:00Z", 7), ("2026-10-24T21:00:00Z", 9)] {
        let now = utc(start);
        let slot = crate::decision::active_slot(&p, now).unwrap().unwrap();
        assert_eq!(
            slot.end.signed_duration_since(slot.start).num_hours(),
            expected_hours
        );
    }
}

#[test]
fn london_nonexistent_start_skips_and_ambiguous_start_is_late() {
    let mut p = profile();
    p.timezone = chrono_tz::Europe::London;
    p.policy.boundary_guard_seconds = 0;
    p.windows[0].start = NaiveTime::from_hms_opt(1, 30, 0).unwrap();
    p.windows[0].end = NaiveTime::from_hms_opt(3, 0, 0).unwrap();
    assert!(matches!(
        evaluate(&p, "2026-03-29T01:00:00Z", 1),
        Decision::Defer { .. }
    ));
    assert!(matches!(
        evaluate(&p, "2026-10-25T00:45:00Z", 1),
        Decision::Defer { .. }
    ));
    assert!(matches!(
        evaluate(&p, "2026-10-25T01:30:00Z", 1),
        Decision::Allow { .. }
    ));
}

#[test]
fn london_ambiguous_end_is_early() {
    let mut p = profile();
    p.timezone = chrono_tz::Europe::London;
    p.policy.boundary_guard_seconds = 0;
    p.windows[0].start = NaiveTime::from_hms_opt(0, 0, 0).unwrap();
    p.windows[0].end = NaiveTime::from_hms_opt(1, 30, 0).unwrap();
    assert!(matches!(
        evaluate(&p, "2026-10-25T00:29:59Z", 1),
        Decision::Allow { .. }
    ));
    assert!(matches!(
        evaluate(&p, "2026-10-25T00:30:00Z", 1),
        Decision::Defer { .. }
    ));
}

#[test]
fn release_and_duplicate_settlement() {
    let p = profile();
    let ledger = BudgetLedger::new(&p, utc("2026-01-05T18:00:00Z")).unwrap();
    let id = ledger.reserve(100).unwrap();
    ledger.release(id).unwrap();
    assert_eq!(ledger.release(id), Err(TariffError::UnknownReservation));
    assert_eq!(ledger.commit(id, 10), Err(TariffError::UnknownReservation));
    assert_eq!(
        ledger.snapshot().unwrap().available_bytes(),
        p.spendable_bytes()
    );
}

#[test]
fn overhead_overage_is_reported_and_future_spend_stops() {
    let mut p = profile();
    p.billing.remaining_bytes = 100;
    p.billing.safety_reserve_percent = 10;
    p.windows[0].max_bytes = 80;
    let now = utc("2026-01-05T18:00:00Z");
    let ledger = BudgetLedger::new(&p, now).unwrap();
    let id = ledger
        .reserve_transfer(now, &network(&p), request(80))
        .unwrap();
    let report = ledger.commit(id, 101).unwrap();
    assert_eq!(report.estimate_overage_bytes, 21);
    assert_eq!(report.window_overage_bytes, 21);
    assert_eq!(report.budget_overage_bytes, 11);
    assert_eq!(ledger.snapshot().unwrap().available_bytes(), 0);
    assert!(ledger.reserve(1).is_err());
}

#[test]
fn overflow_saturates_and_reservation_ids_survive_renewal() {
    let mut p = profile();
    p.billing.remaining_bytes = u64::MAX;
    p.billing.safety_reserve_percent = 0;
    let now = utc("2026-01-05T18:00:00Z");
    let ledger = BudgetLedger::new(&p, now).unwrap();
    let a = ledger.reserve(1).unwrap();
    let b = ledger.reserve(1).unwrap();
    ledger.commit(a, u64::MAX).unwrap();
    assert!(ledger.commit(b, 1).unwrap().saturated);
    assert_eq!(ledger.snapshot().unwrap().available_bytes(), 0);
    ledger.advance(now + Duration::days(40)).unwrap();
    let c = ledger.reserve(1).unwrap();
    assert_ne!(a, c);
    assert_eq!(ledger.commit(a, 1), Err(TariffError::UnknownReservation));
}

#[test]
fn atomic_concurrent_reservations_never_oversubscribe() {
    let mut p = profile();
    p.billing.remaining_bytes = 100;
    p.billing.safety_reserve_percent = 0;
    let ledger = std::sync::Arc::new(BudgetLedger::new(&p, utc("2026-01-05T18:00:00Z")).unwrap());
    let handles: Vec<_> = (0..32)
        .map(|_| {
            let ledger = ledger.clone();
            std::thread::spawn(move || ledger.reserve(10).is_ok())
        })
        .collect();
    let successes = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .filter(|success| *success)
        .count();
    assert_eq!(successes, 10);
    assert_eq!(ledger.snapshot().unwrap().available_bytes(), 0);
}

#[test]
fn occurrence_cap_remains_across_billing_reset() {
    let mut p = profile();
    p.windows[0].max_bytes = 100;
    let now = utc("2026-01-31T17:00:00Z");
    let ledger = BudgetLedger::new(&p, now).unwrap();
    let id = ledger
        .reserve_transfer(now, &network(&p), request(100))
        .unwrap();
    ledger.commit(id, 100).unwrap();
    ledger.advance(utc("2026-01-31T18:30:00Z")).unwrap();
    assert!(matches!(
        decide(
            &p,
            &ledger.snapshot().unwrap(),
            utc("2026-01-31T18:30:00Z"),
            &network(&p),
            request(1)
        ),
        Decision::Defer { .. }
    ));
}

#[test]
fn invalid_profiles_fail_closed() {
    let mut p = profile();
    p.windows.push(p.windows[0].clone());
    assert!(p.validate().is_err());
    let mut p = profile();
    p.windows[0].marginal_price_per_gb = f64::NAN;
    assert!(p.validate().is_err());
    let mut p = profile();
    p.billing.reset_rule.day_of_month = 0;
    assert!(p.validate().is_err());
    let mut p = profile();
    p.billing.safety_reserve_percent = 101;
    assert!(p.validate().is_err());
    let mut p = profile();
    p.windows[0].end = p.windows[0].start;
    assert!(p.validate().is_err());
}

#[test]
fn deterministic_jitter_and_horizon() {
    let p = profile();
    let now = utc("2026-01-05T10:00:00Z");
    let state = BudgetLedger::new(&p, now).unwrap().snapshot().unwrap();
    let pending = vec![PendingTransfer {
        transfer_id: "example".into(),
        request: request(100),
    }];
    let a = plan(
        &p,
        &state,
        now,
        now + Duration::days(1),
        &network(&p),
        &pending,
        Jitter {
            device_id: "device-a",
            max_seconds: 60,
        },
    )
    .unwrap();
    let b = plan(
        &p,
        &state,
        now,
        now + Duration::days(1),
        &network(&p),
        &pending,
        Jitter {
            device_id: "device-a",
            max_seconds: 60,
        },
    )
    .unwrap();
    assert_eq!(a, b);
    assert_eq!(a.schedule.len(), 1);
    assert_eq!(
        a.schedule[0].start,
        utc("2026-01-05T16:30:00Z") + Duration::seconds(i64::from(device_jitter("device-a", 60)))
    );
    assert_ne!(device_jitter("device-a", 60), device_jitter("device-b", 60));
    assert!(plan(
        &p,
        &state,
        now,
        now - Duration::seconds(1),
        &network(&p),
        &pending,
        Jitter {
            device_id: "d",
            max_seconds: 0
        }
    )
    .is_err());
}

#[test]
fn simulator_many_nights_and_seeds() {
    for seed in 0..4 {
        let report = sim::run(65, utc("2026-01-01T12:00:00Z"), seed).unwrap();
        assert_eq!(report.completed + report.deferred, 65 * 32);
        assert!(report.completed > 0);
    }
}

// Independent wall-clock oracle: does not call the decision/calendar implementation.
fn wall_clock_allowed(profile: &TariffProfile, instant: chrono::DateTime<Utc>) -> bool {
    let local = instant.with_timezone(&profile.timezone);
    let day = local.weekday();
    let time = local.time();
    profile.windows.iter().any(|w| {
        if w.start < w.end {
            w.days.contains(&day) && w.start <= time && time < w.end
        } else {
            (w.days.contains(&day) && time >= w.start)
                || (w.days.contains(&day.pred()) && time < w.end)
        }
    })
}
proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]
    #[test]
    fn no_schedule_ever_places_bytes_outside_allowed_window(
        sizes in prop::collection::vec(1u64..3_000_000, 0..30),
        speed in 1u64..200_000,
        offset in 0i64..(365 * 86400),
        london in any::<bool>(), jitter in 0u32..120,
    ) {
        let mut p = profile();
        if london { p.timezone = chrono_tz::Europe::London; }
        let now = utc("2026-01-01T00:00:00Z") + Duration::seconds(offset);
        let ledger = BudgetLedger::new(&p, now).unwrap();
        let mut net = network(&p); net.bytes_per_second = speed;
        let pending: Vec<_> = sizes.into_iter().enumerate().map(|(i, bytes)| PendingTransfer { transfer_id: i.to_string(), request: request(bytes) }).collect();
        let result = plan(&p, &ledger.snapshot().unwrap(), now, now + Duration::days(3), &net, &pending, Jitter { device_id: "property-device", max_seconds: jitter }).unwrap();
        let mut previous_finish = now;
        for entry in result.schedule {
            let slot = crate::decision::active_slot(&p, entry.start).unwrap().unwrap();
            prop_assert!(entry.start >= previous_finish);
            prop_assert!(entry.finish <= slot.end);
            prop_assert!(entry.finish <= now + Duration::days(3));
            // Check the last occupied instant as well as the start and full deadline.
            let last_byte = entry.finish - Duration::nanoseconds(1);
            prop_assert!(crate::decision::active_slot(&p, last_byte).unwrap().is_some());
            prop_assert!(wall_clock_allowed(&p, entry.start));
            prop_assert!(wall_clock_allowed(&p, last_byte));
            // Probe the complete occupied interval independently in one-minute steps.
            let mut instant = entry.start;
            while instant < entry.finish {
                prop_assert!(wall_clock_allowed(&p, instant));
                prop_assert!(wall_clock_allowed(&p, instant + Duration::seconds(i64::from(p.policy.boundary_guard_seconds))));
                instant += Duration::minutes(1);
            }
            let id = ledger.reserve_transfer(entry.start, &net, entry.transfer.request).unwrap();
            let report = ledger.commit(id, entry.transfer.request.bytes).unwrap();
            prop_assert_eq!(report.budget_overage_bytes, 0);
            prop_assert_eq!(report.window_overage_bytes, 0);
            previous_finish = entry.finish;
        }
    }

    #[test]
    fn committed_sum_never_exceeds_budget_minus_reserve_unless_reported(
        budget in 1u64..1_000_000,
        percent in 0u8..=100,
        operations in prop::collection::vec((0u64..100_000, 0u64..100_000, any::<bool>()), 0..100),
    ) {
        let mut p = profile(); p.billing.remaining_bytes = budget; p.billing.safety_reserve_percent = percent;
        let ledger = BudgetLedger::new(&p, utc("2026-01-05T18:00:00Z")).unwrap();
        let mut sum = 0u64; let mut reported = false;
        for (estimate, actual, release) in operations {
            if let Ok(id) = ledger.reserve(estimate) {
                if release { ledger.release(id).unwrap(); } else {
                    let outcome = ledger.commit(id, actual).unwrap(); sum += actual;
                    reported |= outcome.budget_overage_bytes > 0;
                    prop_assert_eq!(outcome.budget_overage_bytes, sum.saturating_sub(p.spendable_bytes()));
                }
            }
            prop_assert_eq!(ledger.snapshot().unwrap().committed_bytes(), sum);
            prop_assert!(sum <= p.spendable_bytes() || reported);
        }
    }
}

#[test]
fn planner_honors_window_caps_and_preserves_unscheduled_work() {
    let mut p = profile();
    p.billing.remaining_bytes = 150;
    p.billing.safety_reserve_percent = 0;
    p.windows[0].max_bytes = 100;
    let now = utc("2026-01-05T18:00:00Z");
    let ledger = BudgetLedger::new(&p, now).unwrap();
    let pending: Vec<_> = (0..3)
        .map(|i| PendingTransfer {
            transfer_id: i.to_string(),
            request: request(70),
        })
        .collect();
    let result = plan(
        &p,
        &ledger.snapshot().unwrap(),
        now,
        now + Duration::days(3),
        &network(&p),
        &pending,
        Jitter {
            device_id: "cap-test",
            max_seconds: 0,
        },
    )
    .unwrap();
    assert_eq!(result.schedule.len(), 2);
    assert_eq!(result.unscheduled.len(), 1);
    assert!(result.schedule[1].start >= utc("2026-01-06T16:30:00Z"));
    for entry in result.schedule {
        let id = ledger
            .reserve_transfer(entry.start, &network(&p), entry.transfer.request)
            .unwrap();
        let outcome = ledger.commit(id, entry.transfer.request.bytes).unwrap();
        assert_eq!(outcome.window_overage_bytes, 0);
        assert_eq!(outcome.budget_overage_bytes, 0);
    }
    assert_eq!(ledger.snapshot().unwrap().committed_bytes(), 140);
}

#[test]
fn slower_observation_defers_remaining_transfer_and_empty_windows_never_open() {
    let p = profile();
    let now = utc("2026-01-06T00:29:00Z");
    let ledger = BudgetLedger::new(&p, now).unwrap();
    let mut net = network(&p);
    assert!(matches!(
        decide(&p, &ledger.snapshot().unwrap(), now, &net, request(100)),
        Decision::Allow { .. }
    ));
    net.bytes_per_second = 1;
    assert!(matches!(
        decide(&p, &ledger.snapshot().unwrap(), now, &net, request(100)),
        Decision::Defer { .. }
    ));
    let mut p = profile();
    p.windows.clear();
    let ledger = BudgetLedger::new(&p, now).unwrap();
    assert_eq!(
        decide(&p, &ledger.snapshot().unwrap(), now, &net, request(1)),
        Decision::Defer { next_open: None }
    );
}
