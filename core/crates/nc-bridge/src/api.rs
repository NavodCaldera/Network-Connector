//! Functions and types exposed to Dart. Wraps `nc_agent::Agent` 1:1 (AGENT.md §3).
//!
//! Only owner settings and low-frequency status cross this boundary. Job payloads never do.

use flutter_rust_bridge::frb;
use nc_agent::{Agent, SessionState};
use nc_policy::OwnerPolicy;
use std::sync::OnceLock;
use std::time::Duration;

use crate::frb_generated::StreamSink;

/// The owner settings the UI shows and edits. Everything else keeps the nc-policy safe default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicySummary {
    /// Explicit consent to run jobs for paired peers.
    pub accept_jobs: bool,
    /// CPU budget, percent.
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
    /// Pause when the owner uses the device.
    pub pause_on_user_activity: bool,
}

impl From<&OwnerPolicy> for PolicySummary {
    fn from(p: &OwnerPolicy) -> Self {
        Self {
            accept_jobs: p.accept_jobs,
            cpu_budget_percent: p.cpu_budget_percent,
            max_memory_mb: p.max_memory_mb,
            max_disk_mb: p.max_disk_mb,
            charging_only: p.charging_only,
            min_battery_percent: p.min_battery_percent,
            max_task_minutes: p.max_task_duration.as_secs() / 60,
            pause_on_user_activity: p.pause_on_user_activity,
        }
    }
}

impl PolicySummary {
    fn to_policy(&self) -> OwnerPolicy {
        OwnerPolicy {
            accept_jobs: self.accept_jobs,
            cpu_budget_percent: self.cpu_budget_percent,
            max_memory_mb: self.max_memory_mb,
            max_disk_mb: self.max_disk_mb,
            charging_only: self.charging_only,
            min_battery_percent: self.min_battery_percent,
            max_task_duration: Duration::from_secs(self.max_task_minutes.saturating_mul(60)),
            pause_on_user_activity: self.pause_on_user_activity,
            ..OwnerPolicy::default()
        }
    }
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

/// The nc-policy safe defaults (no consent, charging only, 25 % CPU, ...).
#[frb(sync)]
pub fn default_policy() -> PolicySummary {
    PolicySummary::from(&OwnerPolicy::default())
}

/// `Agent::start_helper_session(policy)`. Errors are plain-English sentences for the UI.
pub fn start_helper_session(policy: PolicySummary) -> Result<(), String> {
    agent()
        .start_helper_session(policy.to_policy())
        .map_err(|e| e.to_string())
}

/// `Agent::stop_helper_session()`. Returns when the session is `Stopped`.
pub fn stop_helper_session() {
    agent().stop_helper_session();
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

    #[test]
    fn summary_round_trips_and_keeps_safe_defaults() {
        let s = default_policy();
        assert!(!s.accept_jobs);
        let p = s.to_policy();
        assert_eq!(p, OwnerPolicy::default());
        assert!(p.validate().is_ok());
    }

    #[test]
    fn invalid_summary_is_refused_with_plain_message() {
        let mut s = default_policy();
        s.cpu_budget_percent = 0;
        let err = Agent::new()
            .start_helper_session(s.to_policy())
            .unwrap_err();
        assert_eq!(
            err.to_string(),
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
