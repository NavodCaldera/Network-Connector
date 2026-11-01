use crate::OwnerPolicy;
use chrono::{DateTime, Utc};
use nc_tariff::*;

// This synthetic, unlimited tariff projects compute hours onto the shared calendar.
// It authorizes no real transport; DeviceState carries the independent real tariff decision.
pub(crate) fn profile(policy: &OwnerPolicy) -> TariffProfile {
    TariffProfile {
        profile_id: "internal-policy-hours-only".into(),
        version: policy.version,
        timezone: policy.timezone,
        binding: ConnectionBinding {
            interface: InterfaceClass::Ethernet,
            connection_id: "internal-no-transport".into(),
        },
        windows: policy
            .time_windows
            .iter()
            .map(|w| Window {
                days: w.days.clone(),
                start: w.start,
                end: w.end,
                marginal_price_per_gb: 0.0,
                max_bytes: u64::MAX,
            })
            .collect(),
        outside_window: OutsideWindowRule::Block,
        billing: Billing {
            reset_rule: ResetRule { day_of_month: 1 },
            remaining_bytes: u64::MAX,
            safety_reserve_percent: 0,
        },
        policy: Policy {
            allow_cellular_fallback: false,
            boundary_guard_seconds: 0,
            strict_zero_outside_window: true,
        },
    }
}

pub(crate) fn decision(policy: &OwnerPolicy, now: DateTime<Utc>) -> Result<Decision, TariffError> {
    // Keep conversions and the shared 370-day lookahead within chrono's calendar.
    if now
        .checked_add_signed(chrono::Duration::days(400))
        .is_none()
        || now
            .checked_sub_signed(chrono::Duration::days(400))
            .is_none()
    {
        return Err(TariffError::Range);
    }
    let profile = profile(policy);
    let ledger = BudgetLedger::new(&profile, now)?;
    let network = NetworkObservation {
        interface: profile.binding.interface,
        connection_id: Some(profile.binding.connection_id.clone()),
        bytes_per_second: 1,
    };
    Ok(decide(
        &profile,
        &ledger.snapshot()?,
        now,
        &network,
        TransferRequest {
            class: TrafficClass::Control,
            bytes: 0,
        },
    ))
}
