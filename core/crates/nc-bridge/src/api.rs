//! Functions and types exposed to Dart. Wraps `nc_agent::Agent` 1:1 (AGENT.md §3).
//!
//! Only owner settings and low-frequency status cross this boundary. Job payloads never do.
//! All validation happens here in Rust (nc-policy, nc-tariff); Dart only shows the messages.

use flutter_rust_bridge::frb;
use nc_agent::{Agent, SessionState};
use std::path::Path;
use std::sync::OnceLock;

use crate::frb_generated::StreamSink;
use crate::{night, settings, today};

/// One weekly time window, local time of `timezone`. Minutes count from midnight (0..1440).
/// `end_minute` before `start_minute` means the window crosses midnight.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimeWindow {
    /// Weekdays the window starts on, 0 = Monday .. 6 = Sunday.
    pub days: Vec<u8>,
    /// Start, minutes after local midnight (inclusive).
    pub start_minute: u16,
    /// End, minutes after local midnight (exclusive).
    pub end_minute: u16,
}

/// The owner limits the UI shows and edits (nc-policy `OwnerPolicy`). Other fields keep the safe default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicySummary {
    /// Explicit consent to run jobs for paired peers.
    pub accept_jobs: bool,
    /// CPU budget, percent (best effort).
    pub cpu_budget_percent: u8,
    /// Memory cap for all jobs, MiB.
    pub max_memory_mb: u64,
    /// Disk cap for all jobs, MiB.
    pub max_disk_mb: u64,
    /// Work only while charging.
    pub charging_only: bool,
    /// Minimum battery, percent.
    pub min_battery_percent: u8,
    /// Longest single job, minutes.
    pub max_task_minutes: u64,
    /// Work only when the device is idle.
    pub pause_on_user_activity: bool,
    /// Idle time needed before work starts, minutes.
    pub idle_minutes: u32,
    /// IANA time zone for `hours`, e.g. `Asia/Colombo`.
    pub timezone: String,
    /// Allowed working hours. Empty means never (nc-policy safe default).
    pub hours: Vec<TimeWindow>,
}

/// Night-data (tariff) settings. Windows are owner-configured, never ISP facts (PLAN.md §6).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NightDataSettings {
    /// Id of the chosen preset from [`tariff_presets`], or `custom`.
    pub preset_id: String,
    /// The window in use (copied from the preset, or the owner's own).
    pub window: TimeWindow,
    /// Most data to use in one night window, MB (10^6 bytes).
    pub nightly_budget_mb: u64,
    /// Never use data outside the free window, not even status messages.
    pub never_paid_data: bool,
    /// IANA time zone, e.g. `Asia/Colombo`.
    pub timezone: String,
}

/// A starting point for the night window. The owner must check it against their own package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TariffPreset {
    /// Stable id; the app shows its label from `strings.dart`.
    pub id: String,
    /// The window.
    pub window: TimeWindow,
}

/// Everything the app saves. One versioned JSON file; no secrets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppSettings {
    /// Owner limits.
    pub policy: PolicySummary,
    /// Night-data settings.
    pub night_data: NightDataSettings,
}

/// Result of [`load_settings`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedSettings {
    /// The settings to use (safe defaults if the file was missing or bad).
    pub settings: AppSettings,
    /// Set when the file could not be used and was kept as `settings.json.bad`.
    pub reset_notice: Option<String>,
}

/// Next free window and budget, computed by nc-tariff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NightDataStatus {
    /// The free window is open now.
    pub open_now: bool,
    /// Start of the next window, local time text (e.g. `Tue 00:00`). None while open or if none found.
    pub window_start: Option<String>,
    /// End of that window (after the safety margin), local time text.
    pub window_end: Option<String>,
    /// Bytes still allowed in that window.
    pub budget_left_bytes: u64,
    /// Nightly budget, bytes.
    pub budget_total_bytes: u64,
}

/// Today's helping summary (local date of the device clock in the policy time zone).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TodaySummary {
    /// Seconds a helper session ran today.
    pub helping_seconds: u64,
    /// Jobs finished today. Always 0 until nc-agent reports jobs.
    pub jobs_done: u64,
}

/// Session state, mirrored from `nc_agent::SessionState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HelperState {
    /// Not helping.
    Stopped,
    /// Starting.
    Starting,
    /// Helping, waiting for jobs.
    WaitingForJobs,
    /// Running, but "Accept jobs" is off.
    NotAccepting,
    /// Stopping.
    Stopping,
}

impl From<SessionState> for HelperState {
    fn from(s: SessionState) -> Self {
        match s {
            SessionState::Stopped => Self::Stopped,
            SessionState::Starting => Self::Starting,
            SessionState::WaitingForJobs => Self::WaitingForJobs,
            SessionState::NotAccepting => Self::NotAccepting,
            SessionState::Stopping => Self::Stopping,
        }
    }
}

/// Status event sent to the UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelperStatus {
    /// State.
    pub state: HelperState,
    /// Change counter.
    pub seq: u64,
}

fn agent() -> &'static Agent {
    static AGENT: OnceLock<Agent> = OnceLock::new();
    AGENT.get_or_init(Agent::new)
}

/// Called once by the generated Dart `RustLib.init()`.
#[frb(init)]
pub fn init_app() {
    flutter_rust_bridge::setup_default_user_utils();
}

/// The nc-policy safe defaults (no consent, charging only, 25 % CPU, no hours, ...).
#[frb(sync)]
pub fn default_policy() -> PolicySummary {
    settings::default_settings().policy
}

/// Safe defaults for everything the app saves.
#[frb(sync)]
pub fn default_settings() -> AppSettings {
    settings::default_settings()
}

/// Checks owner limits with nc-policy. `None` means valid; otherwise a plain-English sentence.
#[frb(sync)]
pub fn validate_policy(policy: PolicySummary) -> Option<String> {
    settings::to_policy(&policy).err()
}

/// Checks night-data settings with nc-tariff. `None` means valid.
#[frb(sync)]
pub fn validate_night_data(night_data: NightDataSettings) -> Option<String> {
    night::to_profile(&night_data).err()
}

/// Built-in night windows to start from.
#[frb(sync)]
pub fn tariff_presets() -> Vec<TariffPreset> {
    night::presets()
}

/// Current or next free window and the budget left, from nc-tariff at the device clock.
#[frb(sync)]
pub fn night_data_status(night_data: NightDataSettings) -> Result<NightDataStatus, String> {
    night::status_at(&night_data, chrono::Utc::now())
}

/// The platform app-support folder for `settings.json` (desktop only for now; mobile needs a platform
/// adapter). Errors are plain English.
#[frb(sync)]
pub fn settings_dir() -> Result<String, String> {
    settings::app_support_dir().map(|p| p.to_string_lossy().into_owned())
}

/// Loads `settings.json` from `dir`. Never fails: a missing file gives defaults, a bad one is kept as
/// `settings.json.bad` and defaults are used.
pub fn load_settings(dir: String) -> LoadedSettings {
    settings::load(Path::new(&dir))
}

/// Validates, then writes `settings.json` in `dir` atomically (temp file + rename).
pub fn save_settings(dir: String, settings: AppSettings) -> Result<(), String> {
    settings::save(Path::new(&dir), &settings)
}

/// `Agent::start_helper_session(policy)`. Errors are plain-English sentences for the UI.
pub fn start_helper_session(policy: PolicySummary) -> Result<(), String> {
    let p = settings::to_policy(&policy)?;
    agent().start_helper_session(p).map_err(|e| e.to_string())?;
    today::session_started(&policy.timezone);
    Ok(())
}

/// `Agent::stop_helper_session()`. Returns when the session is `Stopped`.
pub fn stop_helper_session() {
    agent().stop_helper_session();
    today::session_stopped();
}

/// Version of the Rust core (nc-bridge), for the About page.
#[frb(sync)]
pub fn core_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Today's helping summary.
#[frb(sync)]
pub fn today_summary() -> TodaySummary {
    today::summary()
}

/// `Agent::watch_status()` as a Dart `Stream`. Ends when Dart cancels the stream.
pub fn watch_status(sink: StreamSink<HelperStatus>) {
    let rx = agent().watch_status();
    // Forward on a dedicated thread so this call returns at once and never pins a bridge worker.
    // The thread ends at the first status change after Dart cancels (sink.add fails) or when the agent drops.
    let spawned = std::thread::Builder::new()
        .name("nc-bridge-status".into())
        .spawn(move || {
            while let Ok(s) = rx.recv() {
                let ev = HelperStatus {
                    state: s.state.into(),
                    seq: s.seq,
                };
                if sink.add(ev).is_err() {
                    break;
                }
            }
        });
    // If the thread cannot start, the Dart stream simply gets no events; the UI still shows Stop.
    drop(spawned);
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use nc_policy::OwnerPolicy;

    #[test]
    fn summary_round_trips_and_keeps_safe_defaults() {
        let s = default_policy();
        assert!(!s.accept_jobs);
        let p = settings::to_policy(&s).unwrap();
        assert_eq!(
            p,
            OwnerPolicy {
                timezone: chrono_tz::Asia::Colombo,
                ..OwnerPolicy::default()
            }
        );
    }

    #[test]
    fn invalid_summary_is_refused_with_plain_message() {
        let mut s = default_policy();
        s.cpu_budget_percent = 0;
        assert_eq!(
            start_helper_session(s).unwrap_err(),
            "CPU budget must be between 1 and 100 percent."
        );
    }

    #[test]
    fn state_mirror_is_complete() {
        assert_eq!(
            HelperState::from(SessionState::NotAccepting),
            HelperState::NotAccepting
        );
    }
}
