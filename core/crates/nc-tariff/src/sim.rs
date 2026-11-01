use crate::*;
use chrono::{DateTime, Duration, NaiveTime, Utc, Weekday};

/// A synthetic profile for examples/tests only, never a real ISP package.
pub fn example_profile() -> Result<TariffProfile, TariffError> {
    Ok(TariffProfile {
        profile_id: "example-only-night-plan".into(),
        version: 1,
        timezone: chrono_tz::Asia::Colombo,
        binding: ConnectionBinding {
            interface: InterfaceClass::Wifi,
            connection_id: "owner-confirmed-example-connection".into(),
        },
        windows: vec![Window {
            days: vec![
                Weekday::Mon,
                Weekday::Tue,
                Weekday::Wed,
                Weekday::Thu,
                Weekday::Fri,
                Weekday::Sat,
                Weekday::Sun,
            ],
            start: NaiveTime::from_hms_opt(22, 0, 0).ok_or(TariffError::Range)?,
            end: NaiveTime::from_hms_opt(6, 0, 0).ok_or(TariffError::Range)?,
            marginal_price_per_gb: 0.0,
            max_bytes: 50_000_000,
        }],
        outside_window: OutsideWindowRule::Block,
        billing: Billing {
            reset_rule: ResetRule { day_of_month: 1 },
            remaining_bytes: 500_000_000,
            safety_reserve_percent: 10,
        },
        policy: Policy {
            allow_cellular_fallback: false,
            boundary_guard_seconds: 30,
            strict_zero_outside_window: true,
        },
    })
}

/// Deterministic simulation summary; bytes describe synthetic wire traffic only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimulationReport {
    /// Requested number of synthetic nights.
    pub nights: u32,
    /// Successfully committed transfers.
    pub completed: u64,
    /// Transfers not feasible before their nightly horizon.
    pub deferred: u64,
    /// Total simulated bytes across billing periods.
    pub committed_bytes: u64,
}

fn random(seed: &mut u64) -> u64 {
    *seed = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    *seed
}

/// Simulates random-sized transfers and throughput with a deterministic seed.
///
/// Every scheduled start/end is checked against the actual decision API and ledger;
/// an invariant failure returns an error. The caller supplies UTC time, no clock I/O.
/// At most 3660 nights are accepted to bound simulation work.
pub fn run(nights: u32, start: DateTime<Utc>, seed: u64) -> Result<SimulationReport, TariffError> {
    if nights > 3660 {
        return Err(TariffError::Range);
    }
    let profile = example_profile()?;
    let ledger = BudgetLedger::new(&profile, start)?;
    let mut seed = seed;
    let mut report = SimulationReport {
        nights,
        completed: 0,
        deferred: 0,
        committed_bytes: 0,
    };
    for night in 0..nights {
        let beginning = start
            .checked_add_signed(Duration::days(i64::from(night)))
            .ok_or(TariffError::Range)?;
        let horizon = beginning
            .checked_add_signed(Duration::days(1))
            .ok_or(TariffError::Range)?;
        ledger.advance(beginning)?;
        let mut cursor = beginning;
        for transfer in 0..32 {
            let network = NetworkObservation {
                interface: InterfaceClass::Wifi,
                connection_id: Some(profile.binding.connection_id.clone()),
                bytes_per_second: 100 + random(&mut seed) % 100_000,
            };
            let bytes = 1 + random(&mut seed) % 10_000_000;
            let pending = PendingTransfer {
                transfer_id: format!("example-{night}-{transfer}"),
                request: TransferRequest {
                    class: TrafficClass::Bulk,
                    bytes,
                },
            };
            let proposed = plan(
                &profile,
                &ledger.snapshot()?,
                cursor,
                horizon,
                &network,
                &[pending],
                Jitter {
                    device_id: "example-device",
                    max_seconds: 60,
                },
            )?;
            if let Some(entry) = proposed.schedule.first() {
                let snapshot = ledger.snapshot()?;
                if !matches!(decide(&profile, &snapshot, entry.start, &network, entry.transfer.request), Decision::Allow { until, .. } if entry.finish <= until)
                {
                    return Err(TariffError::NotAllowed);
                }
                // Window containment is checked independently of the planner's deadline.
                let slot = crate::decision::active_slot(&profile, entry.start)?
                    .ok_or(TariffError::NotAllowed)?;
                if entry.finish > slot.end {
                    return Err(TariffError::NotAllowed);
                }
                let id = ledger.reserve_transfer(entry.start, &network, entry.transfer.request)?;
                let committed = ledger.commit(id, bytes)?;
                if committed.budget_overage_bytes != 0
                    || committed.window_overage_bytes != 0
                    || committed.saturated
                {
                    return Err(TariffError::InsufficientBudget);
                }
                if ledger.snapshot()?.committed_bytes() > profile.spendable_bytes() {
                    return Err(TariffError::InsufficientBudget);
                }
                report.completed = report.completed.saturating_add(1);
                report.committed_bytes = report.committed_bytes.saturating_add(bytes);
                cursor = entry.finish;
            } else {
                report.deferred = report.deferred.saturating_add(1);
            }
        }
    }
    Ok(report)
}
