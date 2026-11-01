//! Night-data settings -> nc-tariff `TariffProfile`, and the "next free window / budget left" query.
//!
//! All tariff maths is nc-tariff's (`decide`, `BudgetLedger`); this module only builds the profile and
//! formats the answer. Presets are starting points the owner confirms, not ISP facts (PLAN.md §6).

use crate::api::{NightDataSettings, NightDataStatus, TariffPreset, TimeWindow};
use crate::settings::{parse_tz, time, weekday, DEFAULT_TZ};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use nc_tariff::{
    decide, Billing, BudgetLedger, ConnectionBinding, Decision, InterfaceClass, NetworkObservation,
    OutsideWindowRule, Policy, ResetRule, TariffError, TariffProfile, TrafficClass,
    TransferRequest, Window,
};

/// Id used when the owner sets their own window.
pub(crate) const CUSTOM: &str = "custom";
const BYTES_PER_MB: u64 = 1_000_000;
/// Stop this long before the window ends, so a transfer never runs into paid time.
const BOUNDARY_GUARD_SECONDS: u32 = 60;
// The platform adapter does not confirm the connection yet (no network binding UI in Phase 1 A1).
// Both the profile and the status query use this placeholder, so it authorises no real transfer.
const BINDING_PENDING: &str = "owner-confirmation-pending";

fn every_day() -> Vec<u8> {
    (0..7).collect()
}

pub(crate) fn presets() -> Vec<TariffPreset> {
    [
        ("night-00-08", 0, 8 * 60),
        ("night-00-06", 0, 6 * 60),
        ("night-22-06", 22 * 60, 6 * 60),
    ]
    .into_iter()
    .map(|(id, start, end)| TariffPreset {
        id: id.into(),
        window: TimeWindow {
            days: every_day(),
            start_minute: start,
            end_minute: end,
        },
    })
    .collect()
}

pub(crate) fn default_night_data() -> NightDataSettings {
    let first = presets().swap_remove(0);
    NightDataSettings {
        preset_id: first.id,
        window: first.window,
        nightly_budget_mb: 1000,
        never_paid_data: true,
        timezone: DEFAULT_TZ.into(),
    }
}

fn plain(e: &TariffError) -> String {
    match e {
        TariffError::InvalidProfile("window") => {
            "The night window needs at least one day and different start and end times.".into()
        }
        TariffError::InvalidProfile("overlapping windows or duplicate days") => {
            "Night windows overlap or repeat a day.".into()
        }
        _ => "These night-data settings are not valid.".into(),
    }
}

/// Builds and validates the nc-tariff profile. Errors are plain English.
pub(crate) fn to_profile(n: &NightDataSettings) -> Result<TariffProfile, String> {
    let timezone: Tz = parse_tz(&n.timezone)?;
    if n.preset_id != CUSTOM && !presets().iter().any(|p| p.id == n.preset_id) {
        return Err("Unknown night-data choice.".into());
    }
    if n.nightly_budget_mb == 0 {
        return Err("Nightly data budget must be more than 0 MB.".into());
    }
    let max_bytes = n
        .nightly_budget_mb
        .checked_mul(BYTES_PER_MB)
        .ok_or_else(|| "Nightly data budget is too large.".to_string())?;
    let profile = TariffProfile {
        profile_id: "owner-night-data".into(),
        version: 1,
        timezone,
        binding: ConnectionBinding {
            interface: InterfaceClass::Wifi,
            connection_id: BINDING_PENDING.into(),
        },
        windows: vec![Window {
            days: n
                .window
                .days
                .iter()
                .map(|d| weekday(*d))
                .collect::<Result<_, _>>()?,
            start: time(n.window.start_minute)?,
            end: time(n.window.end_minute)?,
            marginal_price_per_gb: 0.0,
            max_bytes,
        }],
        outside_window: if n.never_paid_data {
            OutsideWindowRule::Block
        } else {
            // Only small status messages may use paid data; bulk work always waits for the window.
            OutsideWindowRule::AllowClasses(vec![TrafficClass::Control])
        },
        billing: Billing {
            reset_rule: ResetRule { day_of_month: 1 },
            // No monthly cap in the UI yet; the nightly window cap is the limit.
            remaining_bytes: u64::MAX,
            safety_reserve_percent: 0,
        },
        policy: Policy {
            allow_cellular_fallback: false,
            boundary_guard_seconds: BOUNDARY_GUARD_SECONDS,
            strict_zero_outside_window: n.never_paid_data,
        },
    };
    profile.validate().map_err(|e| plain(&e))?;
    Ok(profile)
}

fn local(t: DateTime<Utc>, tz: Tz) -> String {
    t.with_timezone(&tz).format("%a %H:%M").to_string()
}

/// Asks nc-tariff whether a bulk transfer could run at `now`, and if not, when the next window opens.
pub(crate) fn status_at(
    n: &NightDataSettings,
    now: DateTime<Utc>,
) -> Result<NightDataStatus, String> {
    let profile = to_profile(n)?;
    let total = n.nightly_budget_mb.saturating_mul(BYTES_PER_MB);
    let ledger = BudgetLedger::new(&profile, now).map_err(|e| plain(&e))?;
    let budget = ledger.snapshot().map_err(|e| plain(&e))?;
    let network = NetworkObservation {
        interface: profile.binding.interface,
        connection_id: Some(profile.binding.connection_id.clone()),
        // Throughput is not the question here; only the window and budget limits are.
        bytes_per_second: u64::MAX,
    };
    let probe = TransferRequest {
        class: TrafficClass::Bulk,
        bytes: 1,
    };
    let none = NightDataStatus {
        open_now: false,
        window_start: None,
        window_end: None,
        budget_left_bytes: 0,
        budget_total_bytes: total,
    };
    match decide(&profile, &budget, now, &network, probe) {
        Decision::Allow { until, max_bytes } => Ok(NightDataStatus {
            open_now: true,
            window_start: None,
            window_end: Some(local(until, profile.timezone)),
            budget_left_bytes: max_bytes,
            budget_total_bytes: total,
        }),
        Decision::Defer {
            next_open: Some(start),
        } => match decide(&profile, &budget, start, &network, probe) {
            Decision::Allow { until, max_bytes } => Ok(NightDataStatus {
                open_now: false,
                window_start: Some(local(start, profile.timezone)),
                window_end: Some(local(until, profile.timezone)),
                budget_left_bytes: max_bytes,
                budget_total_bytes: total,
            }),
            _ => Ok(none),
        },
        Decision::Defer { next_open: None } => Ok(none),
        Decision::Deny { .. } => Err("These night-data settings are not valid.".into()),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    #[test]
    fn default_is_valid_and_never_uses_paid_data() {
        let n = default_night_data();
        let p = to_profile(&n).unwrap();
        assert!(p.policy.strict_zero_outside_window);
        assert!(!p.policy.allow_cellular_fallback);
        assert_eq!(p.outside_window, OutsideWindowRule::Block);
        assert_eq!(p.timezone, chrono_tz::Asia::Colombo);
    }

    #[test]
    fn every_preset_is_a_valid_profile() {
        for preset in presets() {
            let n = NightDataSettings {
                preset_id: preset.id,
                window: preset.window,
                ..default_night_data()
            };
            to_profile(&n).unwrap();
        }
    }

    #[test]
    fn next_window_in_the_afternoon_is_tonight() {
        // 2026-01-05 is a Monday. 12:00 UTC = 17:30 in Colombo (UTC+05:30).
        let s = status_at(&default_night_data(), at("2026-01-05T12:00:00Z")).unwrap();
        assert!(!s.open_now);
        assert_eq!(s.window_start.as_deref(), Some("Tue 00:00"));
        // 08:00 minus the 60 s boundary guard.
        assert_eq!(s.window_end.as_deref(), Some("Tue 07:59"));
        assert_eq!(s.budget_left_bytes, 1000 * BYTES_PER_MB);
        assert_eq!(s.budget_total_bytes, 1000 * BYTES_PER_MB);
    }

    #[test]
    fn open_window_reports_now_and_end() {
        // 20:00 UTC Monday = 01:30 Tuesday in Colombo.
        let s = status_at(&default_night_data(), at("2026-01-05T20:00:00Z")).unwrap();
        assert!(s.open_now);
        assert_eq!(s.window_start, None);
        assert_eq!(s.window_end.as_deref(), Some("Tue 07:59"));
        assert_eq!(s.budget_left_bytes, 1000 * BYTES_PER_MB);
    }

    #[test]
    fn custom_weekend_window_cross_midnight() {
        let n = NightDataSettings {
            preset_id: CUSTOM.into(),
            window: TimeWindow {
                days: vec![5], // Saturday
                start_minute: 23 * 60,
                end_minute: 5 * 60,
            },
            nightly_budget_mb: 250,
            ..default_night_data()
        };
        // Monday afternoon: next is Saturday 23:00 local, ending Sunday 04:59.
        let s = status_at(&n, at("2026-01-05T12:00:00Z")).unwrap();
        assert_eq!(s.window_start.as_deref(), Some("Sat 23:00"));
        assert_eq!(s.window_end.as_deref(), Some("Sun 04:59"));
        assert_eq!(s.budget_left_bytes, 250 * BYTES_PER_MB);
    }

    #[test]
    fn invalid_settings_get_plain_errors() {
        let mut n = default_night_data();
        n.nightly_budget_mb = 0;
        assert_eq!(
            to_profile(&n).unwrap_err(),
            "Nightly data budget must be more than 0 MB."
        );
        let mut n = default_night_data();
        n.preset_id = CUSTOM.into();
        n.window.days.clear();
        assert_eq!(
            to_profile(&n).unwrap_err(),
            "The night window needs at least one day and different start and end times."
        );
        let mut n = default_night_data();
        n.window.end_minute = n.window.start_minute;
        assert!(to_profile(&n).is_err());
        let mut n = default_night_data();
        n.window.start_minute = 24 * 60;
        assert_eq!(
            to_profile(&n).unwrap_err(),
            "Times must be between 00:00 and 23:59."
        );
        let mut n = default_night_data();
        n.preset_id = "isp-x".into();
        assert_eq!(to_profile(&n).unwrap_err(), "Unknown night-data choice.");
        let mut n = default_night_data();
        n.nightly_budget_mb = u64::MAX;
        assert_eq!(
            to_profile(&n).unwrap_err(),
            "Nightly data budget is too large."
        );
    }

    #[test]
    fn paid_data_switch_off_allows_only_control() {
        let mut n = default_night_data();
        n.never_paid_data = false;
        let p = to_profile(&n).unwrap();
        assert!(!p.policy.strict_zero_outside_window);
        assert_eq!(
            p.outside_window,
            OutsideWindowRule::AllowClasses(vec![TrafficClass::Control])
        );
        // Bulk still waits for the window.
        let s = status_at(&n, at("2026-01-05T12:00:00Z")).unwrap();
        assert!(!s.open_now);
    }
}
