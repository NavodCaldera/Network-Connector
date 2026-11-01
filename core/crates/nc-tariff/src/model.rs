use chrono::{NaiveTime, Weekday};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

/// Owner-configured, versioned tariff; `remaining_bytes` is the allocation renewed each period.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TariffProfile {
    /// Stable owner-assigned identifier.
    pub profile_id: String,
    /// Configuration revision; changing it requires a new ledger.
    pub version: u64,
    /// IANA time zone, serialized by name.
    pub timezone: Tz,
    /// Owner-confirmed connection identity, never an SSID alone.
    pub binding: ConnectionBinding,
    /// Non-overlapping allowed windows; weekday belongs to the start date.
    pub windows: Vec<Window>,
    /// Explicit exceptions outside windows (ignored in strict mode).
    pub outside_window: OutsideWindowRule,
    /// Monthly accounting configuration.
    pub billing: Billing,
    /// Transfer restrictions.
    pub policy: Policy,
}

/// Physical interface category.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterfaceClass {
    /// Wireless LAN.
    Wifi,
    /// Wired LAN.
    Ethernet,
    /// Mobile-data connection.
    Cellular,
}

/// Explicitly confirmed logical connection; an observed SSID is insufficient.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionBinding {
    /// Expected physical interface category.
    pub interface: InterfaceClass,
    /// Owner-confirmed opaque identity supplied by the platform adapter.
    pub connection_id: String,
}

/// Weekly half-open local interval; end before start means crossing midnight.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Window {
    /// Weekdays on which this window starts.
    pub days: Vec<Weekday>,
    /// Inclusive local start, e.g. `00:00:00`.
    pub start: NaiveTime,
    /// Exclusive local end; equal endpoints are invalid (not a full day).
    pub end: NaiveTime,
    /// Owner-provided nonnegative price; no currency conversion is performed.
    pub marginal_price_per_gb: f64,
    /// Cap per concrete window occurrence, including reservations and overhead.
    pub max_bytes: u64,
}

/// Traffic purpose; permissions are independently selected outside windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrafficClass {
    /// Heartbeats and control messages.
    Control,
    /// Input and other bulk payloads.
    Bulk,
    /// Wasm module acquisition.
    ModuleDownload,
    /// Computation results.
    ResultUpload,
    /// Explicit recovery traffic.
    Recovery,
}

/// Optional out-of-window permissions, still subject to network and period budget.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutsideWindowRule {
    /// Defer all traffic until a configured window.
    Block,
    /// Permit only these classes outside windows.
    AllowClasses(Vec<TrafficClass>),
}

/// Monthly renewal in the profile's local time zone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResetRule {
    /// Local day of month, 1..=31; clamped to the final day of shorter months.
    pub day_of_month: u32,
}

/// Period budget; reserve rounds upwards to avoid spending fractional bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Billing {
    /// Monthly local-midnight renewal rule.
    pub reset_rule: ResetRule,
    /// Available allocation at initialization and each renewal, not an ISP query.
    pub remaining_bytes: u64,
    /// Percentage 0..=100 excluded from spending.
    pub safety_reserve_percent: u8,
}

/// Owner-selected transfer restrictions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    /// Explicitly allows a confirmed cellular identity when the primary binding differs.
    pub allow_cellular_fallback: bool,
    /// Seconds before window end at which all window traffic must stop.
    pub boundary_guard_seconds: u32,
    /// Blocks every class outside windows, including Control.
    pub strict_zero_outside_window: bool,
}

/// Adapter observation; the adapter must supply a trustworthy confirmed connection ID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkObservation {
    /// Current interface category.
    pub interface: InterfaceClass,
    /// Confirmed identity; `None` refuses traffic even if an SSID is known.
    pub connection_id: Option<String>,
    /// Conservative observed total wire bytes per second; zero means unknown.
    pub bytes_per_second: u64,
}

/// A complete bounded transfer, including estimated protocol/relay overhead.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferRequest {
    /// Traffic permission class.
    pub class: TrafficClass,
    /// Estimated total wire bytes; reconcile against actual accounting later.
    pub bytes: u64,
}

/// Fail-closed input, ledger or scheduling failure.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TariffError {
    /// Malformed or overlapping profile windows.
    #[error("invalid profile: {0}")]
    InvalidProfile(&'static str),
    /// UTC/local calendar conversion or checked arithmetic failed.
    #[error("date or arithmetic out of range")]
    Range,
    /// A ledger mutex was poisoned; no spending is authorized.
    #[error("ledger unavailable")]
    LedgerUnavailable,
    /// Reservation is unknown, already settled or expired by renewal.
    #[error("unknown reservation")]
    UnknownReservation,
    /// Available bytes cannot cover a reservation.
    #[error("insufficient budget")]
    InsufficientBudget,
    /// Decision refused this transfer.
    #[error("transfer not currently allowed")]
    NotAllowed,
}

impl TariffProfile {
    /// Checks static configuration, including overlap across weekly midnight boundaries.
    pub fn validate(&self) -> Result<(), TariffError> {
        if self.profile_id.is_empty() || self.binding.connection_id.is_empty() {
            return Err(TariffError::InvalidProfile("empty identity"));
        }
        if !(1..=31).contains(&self.billing.reset_rule.day_of_month)
            || self.billing.safety_reserve_percent > 100
        {
            return Err(TariffError::InvalidProfile("billing rule"));
        }
        let mut intervals = Vec::new();
        use chrono::Timelike;
        for w in &self.windows {
            if w.days.is_empty()
                || w.start == w.end
                || !w.marginal_price_per_gb.is_finite()
                || w.marginal_price_per_gb < 0.0
                || w.start.nanosecond() != 0
                || w.end.nanosecond() != 0
            {
                return Err(TariffError::InvalidProfile("window"));
            }
            for day in &w.days {
                let start = i64::from(day.num_days_from_monday()) * 86400
                    + i64::from(w.start.num_seconds_from_midnight());
                let mut end = i64::from(day.num_days_from_monday()) * 86400
                    + i64::from(w.end.num_seconds_from_midnight());
                if end <= start {
                    end += 86400;
                }
                for offset in [-604800, 0, 604800] {
                    intervals.push((start + offset, end + offset));
                }
            }
        }
        intervals.sort_unstable();
        if intervals.windows(2).any(|p| p[0].1 > p[1].0) {
            return Err(TariffError::InvalidProfile(
                "overlapping windows or duplicate days",
            ));
        }
        Ok(())
    }

    /// Spendable allocation after an upward-rounded safety reserve, without overflow.
    pub fn spendable_bytes(&self) -> u64 {
        let total = u128::from(self.billing.remaining_bytes);
        let reserve = (total * u128::from(self.billing.safety_reserve_percent)).div_ceil(100);
        u64::try_from(total.saturating_sub(reserve)).unwrap_or(0)
    }
}
