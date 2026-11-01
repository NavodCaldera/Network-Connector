use crate::*;
use chrono::{DateTime, Duration, Utc};

/// Identified pending transfer; order is preserved (no cost-optimality claim).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingTransfer {
    /// Caller-assigned transfer identifier.
    pub transfer_id: String,
    /// Complete bounded transfer estimate.
    pub request: TransferRequest,
}

/// A provisional serial schedule entry; live execution must decide/reserve again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduledTransfer {
    /// Original pending transfer.
    pub transfer: PendingTransfer,
    /// UTC start after deterministic device jitter.
    pub start: DateTime<Utc>,
    /// Estimated exclusive finish under the supplied throughput.
    pub finish: DateTime<Utc>,
}

/// Scheduled entries and explicit unscheduled transfers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// Serial transfers fitting within the horizon.
    pub schedule: Vec<ScheduledTransfer>,
    /// Transfers that cannot fit; no bytes are silently discarded.
    pub unscheduled: Vec<PendingTransfer>,
}

/// Stable FNV-1a device jitter in seconds, inclusive of zero and `max_seconds`.
/// This spreads starts deterministically; it is not cryptographic randomness.
pub fn device_jitter(device_id: &str, max_seconds: u32) -> u32 {
    let hash = device_id.bytes().fold(14695981039346656037u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(1099511628211)
    });
    (hash % (u64::from(max_seconds) + 1)) as u32
}

/// Per-device deterministic start spreading configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Jitter<'a> {
    /// Stable local device identity; never emitted by this library.
    pub device_id: &'a str,
    /// Maximum delay added at each planned start.
    pub max_seconds: u32,
}

/// Greedy serial planner honoring window/period caps and safety reserve.
///
/// Forecast renewals use the configured allocation, not a real provider query.
/// Outstanding snapshot reservations remain charged. Plans are advisory, not leases.
/// The horizon is limited to 366 days to bound CPU/memory work.
pub fn plan(
    profile: &TariffProfile,
    budget: &BudgetState,
    now: DateTime<Utc>,
    horizon: DateTime<Utc>,
    network: &NetworkObservation,
    pending: &[PendingTransfer],
    jitter: Jitter<'_>,
) -> Result<Plan, TariffError> {
    profile.validate()?;
    if horizon < now
        || horizon.signed_duration_since(now) > Duration::days(366)
        || profile != &budget.profile
    {
        return Err(TariffError::Range);
    }
    let ledger = BudgetLedger::from_state(budget.clone());
    let mut cursor = now;
    let jitter = Duration::seconds(i64::from(device_jitter(
        jitter.device_id,
        jitter.max_seconds,
    )));
    let mut result = Plan {
        schedule: Vec::new(),
        unscheduled: Vec::new(),
    };
    for transfer in pending {
        let mut candidate = cursor
            .checked_add_signed(jitter)
            .ok_or(TariffError::Range)?;
        let mut scheduled = false;
        // At most one retry per weekly occurrence across the bounded horizon.
        for _ in 0..=800 {
            if candidate >= horizon {
                break;
            }
            let snapshot = ledger.snapshot()?;
            match decide(profile, &snapshot, candidate, network, transfer.request) {
                Decision::Allow { until, .. } => {
                    let seconds = transfer.request.bytes.div_ceil(network.bytes_per_second);
                    let Ok(seconds) = i64::try_from(seconds) else {
                        break;
                    };
                    let finish = candidate
                        .checked_add_signed(Duration::seconds(seconds))
                        .ok_or(TariffError::Range)?;
                    if finish > horizon || finish > until {
                        break;
                    }
                    let id = ledger.reserve_transfer(candidate, network, transfer.request)?;
                    ledger.commit(id, transfer.request.bytes)?;
                    result.schedule.push(ScheduledTransfer {
                        transfer: transfer.clone(),
                        start: candidate,
                        finish,
                    });
                    cursor = finish;
                    scheduled = true;
                    break;
                }
                Decision::Defer {
                    next_open: Some(next),
                } if next > candidate => {
                    candidate = next.checked_add_signed(jitter).ok_or(TariffError::Range)?;
                }
                _ => break,
            }
        }
        if !scheduled {
            result.unscheduled.push(transfer.clone());
        }
    }
    Ok(result)
}
