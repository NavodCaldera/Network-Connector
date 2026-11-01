use crate::PeerKey;
use chrono::{DateTime, NaiveTime, Utc, Weekday};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Ordered thermal severity. The configured threshold is inclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThermalLevel {
    /// Normal operating temperature.
    Nominal,
    /// Mild thermal pressure.
    Fair,
    /// Significant pressure; typically the default pause threshold.
    Serious,
    /// Emergency level, always a Stop regardless of the configured threshold.
    Critical,
}

/// Half-open weekly local working interval, with weekday attached to its start.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkingWindow {
    /// Weekdays on which the window starts.
    pub days: Vec<Weekday>,
    /// Inclusive local time, at whole-second precision.
    pub start: NaiveTime,
    /// Exclusive local end; earlier than start means crossing midnight.
    pub end: NaiveTime,
}

/// Versioned owner configuration. Empty trust/profile/window lists allow no work.
/// Missing serde fields receive safe defaults, so `{}` never opts the owner in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OwnerPolicy {
    /// Owner configuration revision; zero is invalid.
    pub version: u64,
    /// Explicit consent; false withdraws permission even for running jobs.
    pub accept_jobs: bool,
    /// Requested 1..=100 percent CPU budget, best effort (not a hard OS quota).
    pub cpu_budget_percent: u8,
    /// Maximum number of simultaneous active tasks, including paused tasks.
    pub max_concurrent_tasks: u32,
    /// Aggregate task memory cap in MiB; callers account estimates/reservations too.
    pub max_memory_mb: u64,
    /// Aggregate task disk cap in MiB, including staged data and output reservations.
    pub max_disk_mb: u64,
    /// Require external power; on_ac_power is the adapter's charging proxy.
    pub charging_only: bool,
    /// Minimum battery, inclusive; zero disables battery checks for batteryless devices.
    pub min_battery_percent: u8,
    /// IANA time zone for all working windows.
    pub timezone: Tz,
    /// Allowed working hours; empty means never. Overlaps/equal endpoints are invalid.
    pub time_windows: Vec<WorkingWindow>,
    /// Require known idle time at least idle_minutes before starting/continuing.
    pub pause_on_user_activity: bool,
    /// Idle threshold in minutes; must be positive when activity pausing is enabled.
    pub idle_minutes: u32,
    /// Pause at or above this level; Critical always stops. Unknown always blocks.
    pub thermal_stop: ThermalLevel,
    /// Only these authenticated peer keys may submit jobs; empty means none.
    pub trusted_peers: Vec<PeerKey>,
    /// Always refuses these keys, even if also trusted.
    pub blocked_peers: Vec<PeerKey>,
    /// Explicit permitted runtime profiles; empty means none, no wildcard semantics.
    pub allowed_execution_profiles: Vec<String>,
    /// Per-task wall-clock limit; serde uses Duration's seconds/nanoseconds fields.
    pub max_task_duration: Duration,
}

impl Default for OwnerPolicy {
    fn default() -> Self {
        Self {
            version: 1,
            accept_jobs: false,
            cpu_budget_percent: 25,
            max_concurrent_tasks: 1,
            max_memory_mb: 256,
            max_disk_mb: 256,
            charging_only: true,
            min_battery_percent: 20,
            timezone: chrono_tz::UTC,
            time_windows: Vec::new(),
            pause_on_user_activity: true,
            idle_minutes: 5,
            thermal_stop: ThermalLevel::Serious,
            trusted_peers: Vec::new(),
            blocked_peers: Vec::new(),
            allowed_execution_profiles: Vec::new(),
            max_task_duration: Duration::from_secs(300),
        }
    }
}

/// Invalid configuration with a field-specific owner-readable explanation.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    /// Revision must be nonzero.
    #[error("Policy version must be greater than zero.")]
    Version,
    /// CPU percentage is outside 1..=100.
    #[error("CPU budget must be between 1 and 100 percent.")]
    CpuBudget,
    /// Zero simultaneous tasks is invalid; disable consent instead.
    #[error("Maximum concurrent tasks must be greater than zero.")]
    Concurrency,
    /// Memory cap must be positive.
    #[error("Memory limit must be greater than zero.")]
    Memory,
    /// Disk cap must be positive.
    #[error("Disk limit must be greater than zero.")]
    Disk,
    /// Battery threshold is outside 0..=100.
    #[error("Minimum battery must be between 0 and 100 percent.")]
    Battery,
    /// Activity pausing needs a meaningful idle interval.
    #[error("Idle minutes must be greater than zero when pausing for user activity.")]
    Idle,
    /// Duration is zero or outside the calendar's representable range.
    #[error("Maximum task duration must be positive and fit the supported clock range.")]
    Duration,
    /// Runtime profile identifiers must be nonempty, trimmed and unique.
    #[error(
        "Execution profile names must be nonempty, have no surrounding whitespace, and be unique."
    )]
    Profiles,
    /// Shared tariff calendar rejected the working window configuration.
    #[error("Working hours are invalid: {0}")]
    Hours(nc_tariff::TariffError),
}

impl OwnerPolicy {
    /// Validates configuration even while disabled, without I/O or clock reads.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.version == 0 {
            return Err(ValidationError::Version);
        }
        if !(1..=100).contains(&self.cpu_budget_percent) {
            return Err(ValidationError::CpuBudget);
        }
        if self.max_concurrent_tasks == 0 {
            return Err(ValidationError::Concurrency);
        }
        if self.max_memory_mb == 0 {
            return Err(ValidationError::Memory);
        }
        if self.max_disk_mb == 0 {
            return Err(ValidationError::Disk);
        }
        if self.min_battery_percent > 100 {
            return Err(ValidationError::Battery);
        }
        if self.pause_on_user_activity && self.idle_minutes == 0 {
            return Err(ValidationError::Idle);
        }
        if self.max_task_duration.is_zero()
            || chrono::Duration::from_std(self.max_task_duration).is_err()
        {
            return Err(ValidationError::Duration);
        }
        let mut seen = std::collections::BTreeSet::new();
        for name in &self.allowed_execution_profiles {
            if name.is_empty() || name.trim() != name || !seen.insert(name) {
                return Err(ValidationError::Profiles);
            }
        }
        crate::hours::profile(self)
            .validate()
            .map_err(ValidationError::Hours)
    }
}

/// Authenticated task offer, not a wire-format definition or a signature check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskOffer {
    /// Authenticated submitter's device key.
    pub peer_key: PeerKey,
    /// Exact execution profile requested.
    pub profile: String,
    /// Additional reserved memory estimate in MiB.
    pub est_memory_mb: u64,
    /// Additional reserved disk estimate in MiB.
    pub est_disk_mb: u64,
    /// Estimated nonzero elapsed duration; execution still needs an external watchdog.
    pub est_duration: Duration,
}

/// Context required to revoke or time-limit each existing task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunningTask {
    /// Authenticated submitter key, retained from admission.
    pub peer_key: PeerKey,
    /// Runtime profile used by this task.
    pub profile: String,
    /// UTC task start; paused time counts against the wall-clock limit.
    pub started_at: DateTime<Utc>,
}

/// Caller-supplied snapshot; observations must be fresh and trusted by the supervisor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceState {
    /// Current UTC wall-clock instant; this library never reads a system clock.
    pub now: DateTime<Utc>,
    /// Whether the adapter reports external power.
    pub on_ac_power: bool,
    /// Known 0..=100 battery percent, or unavailable/batteryless.
    pub battery_percent: Option<u8>,
    /// Thermal observation; unknown conservatively rejects/pauses all work.
    pub thermal_level: Option<ThermalLevel>,
    /// Known seconds since last user input; None blocks when activity checks are enabled.
    pub user_idle_seconds: Option<u64>,
    /// All active tasks, including paused tasks, for revocation and concurrency checks.
    pub running_tasks: Vec<RunningTask>,
    /// Aggregate accounted memory, including existing task reservations, in MiB.
    pub memory_in_use_mb: u64,
    /// Aggregate accounted disk, including existing reservations, in MiB.
    pub disk_in_use_mb: u64,
    /// Latest nc-tariff decision for a transfer; None means local compute with no transfer.
    /// Supply and reserve a fresh real tariff decision separately for every transfer.
    pub tariff_decision: Option<nc_tariff::Decision>,
}
