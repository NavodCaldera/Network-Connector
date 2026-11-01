use crate::{
    time::{slots, Slot},
    *,
};
use chrono::{DateTime, Duration, Utc};

/// Fail-closed denial reason suitable for UI and telemetry without sensitive IDs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DenyReason {
    /// Invalid configuration.
    InvalidProfile,
    /// Ledger belongs to a different profile or revision/configuration.
    ProfileMismatch,
    /// Wrong or unconfirmed connection, including forbidden cellular fallback.
    NetworkMismatch,
    /// Wall clock moved behind the ledger's most recently observed instant.
    ClockWentBackwards,
    /// Outstanding reservations prevent safely renewing the allocation.
    UnsettledPeriod,
    /// Calendar/arithmetic limits prevent a trustworthy decision.
    OutOfRange,
}

/// Permission for one bounded transfer; decisions never mutate the ledger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Entire request fits; caller must reserve and stop/recheck at `until`.
    Allow {
        /// Exclusive deadline, already reduced by the boundary guard and billing boundary.
        until: DateTime<Utc>,
        /// Maximum total wire bytes available at this instant, including reserved accounting.
        max_bytes: u64,
    },
    /// Not currently feasible; no opening means none found within the 370-day search.
    Defer {
        /// Earliest evaluated feasible start under the same throughput estimate.
        next_open: Option<DateTime<Utc>>,
    },
    /// Configuration, binding or clock forbids sending.
    Deny {
        /// Stable refusal category.
        reason: DenyReason,
    },
}

pub(crate) fn active_slot(
    profile: &TariffProfile,
    now: DateTime<Utc>,
) -> Result<Option<Slot>, TariffError> {
    Ok(slots(profile, now, now)?
        .into_iter()
        .find(|s| s.start <= now && now < s.end))
}

fn allowance(
    profile: &TariffProfile,
    state: &BudgetState,
    now: DateTime<Utc>,
    network: &NetworkObservation,
    request: TransferRequest,
) -> Result<Option<(DateTime<Utc>, u64)>, TariffError> {
    let active = active_slot(profile, now)?;
    let (until, cap) = if let Some(slot) = active {
        (slot.end.min(state.end), state.window_available(&slot))
    } else {
        // A guard is a hard exclusion, even when outside-window control is enabled.
        let mut unguarded = profile.clone();
        unguarded.policy.boundary_guard_seconds = 0;
        if active_slot(&unguarded, now)?.is_some() {
            return Ok(None);
        }
        let allowed = !profile.policy.strict_zero_outside_window
            && matches!(&profile.outside_window, OutsideWindowRule::AllowClasses(classes) if classes.contains(&request.class));
        if !allowed {
            return Ok(None);
        }
        let until = slots(profile, now, state.end)?
            .first()
            .map_or(state.end, |s| s.start.min(state.end));
        (until, u64::MAX)
    };
    let seconds = until.signed_duration_since(now).num_seconds();
    let wire_capacity = u64::try_from(seconds)
        .unwrap_or(0)
        .saturating_mul(network.bytes_per_second);
    let max_bytes = state.available_bytes().min(cap).min(wire_capacity);
    if now < until && network.bytes_per_second > 0 && request.bytes <= max_bytes {
        Ok(Some((until, max_bytes)))
    } else {
        Ok(None)
    }
}

/// Decides using owner configuration and a snapshot, without I/O or state mutation.
///
/// Cellular fallback accepts only a nonempty adapter-confirmed cellular identity;
/// it still uses this profile's windows/budget. Use a separate profile for a separate
/// mobile tariff. An existing transfer must be stopped and reauthorized on a switch.
/// Forecasts assume constant conservative throughput; recheck every chunk in use.
pub fn decide(
    profile: &TariffProfile,
    budget: &BudgetState,
    now: DateTime<Utc>,
    network: &NetworkObservation,
    request: TransferRequest,
) -> Decision {
    let deny = |reason| Decision::Deny { reason };
    if profile.validate().is_err() {
        return deny(DenyReason::InvalidProfile);
    }
    if profile != &budget.profile {
        return deny(DenyReason::ProfileMismatch);
    }
    let confirmed = network
        .connection_id
        .as_deref()
        .is_some_and(|id| !id.is_empty());
    let bound = confirmed
        && network.interface == profile.binding.interface
        && network.connection_id.as_deref() == Some(profile.binding.connection_id.as_str());
    let fallback = confirmed
        && network.interface == InterfaceClass::Cellular
        && profile.policy.allow_cellular_fallback;
    if !bound && !fallback {
        return deny(DenyReason::NetworkMismatch);
    }
    if now < budget.observed {
        return deny(DenyReason::ClockWentBackwards);
    }
    let mut state = budget.clone();
    if state.advance(now).is_err() {
        return deny(DenyReason::UnsettledPeriod);
    }
    match allowance(profile, &state, now, network, request) {
        Ok(Some((until, max_bytes))) => return Decision::Allow { until, max_bytes },
        Err(_) => return deny(DenyReason::OutOfRange),
        _ => {}
    }
    let Some(horizon) = now.checked_add_signed(Duration::days(370)) else {
        return deny(DenyReason::OutOfRange);
    };
    let Ok(future_slots) = slots(profile, now, horizon) else {
        return deny(DenyReason::OutOfRange);
    };
    let mut candidates: Vec<_> = future_slots
        .into_iter()
        .map(|s| s.start)
        .filter(|t| *t > now)
        .collect();
    // Include renewal for outside-window exceptions and windows straddling reset.
    let mut boundary = state.end;
    while boundary <= horizon {
        candidates.push(boundary);
        let Ok((_, next)) = crate::time::period(profile, boundary) else {
            break;
        };
        if next <= boundary {
            break;
        }
        boundary = next;
    }
    candidates.sort_unstable();
    candidates.dedup();
    for candidate in candidates {
        let mut future = state.clone();
        if future.advance(candidate).is_ok()
            && matches!(
                allowance(profile, &future, candidate, network, request),
                Ok(Some(_))
            )
        {
            return Decision::Defer {
                next_open: Some(candidate),
            };
        }
    }
    Decision::Defer { next_open: None }
}
