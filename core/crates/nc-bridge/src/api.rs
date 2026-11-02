//! Functions and types exposed to Dart. Wraps `nc_agent::Agent` 1:1 (AGENT.md §3).
//!
//! Only owner settings and low-frequency status cross this boundary. Job payloads never do.
//! All validation happens here in Rust (nc-policy, nc-tariff); Dart only shows the messages.

use flutter_rust_bridge::frb;
use nc_agent::{Agent, SessionState};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

use crate::frb_generated::StreamSink;
use crate::{enrol, night, requests, settings};

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

/// Today's counters from `nc_agent::Agent::today()` (reset at local midnight in the policy time zone).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TodaySummary {
    /// Seconds a helper session ran today.
    pub helping_seconds: u64,
    /// Jobs committed today (each attempt counted once).
    pub jobs_done: u64,
    /// Seconds spent running jobs today.
    pub job_seconds: u64,
}

impl From<nc_agent::Today> for TodaySummary {
    fn from(t: nc_agent::Today) -> Self {
        Self {
            helping_seconds: t.helping_seconds,
            jobs_done: t.jobs_done,
            job_seconds: t.job_seconds,
        }
    }
}

/// What the helper is doing now, mirrored from `nc_agent::Activity`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivityKind {
    /// No session.
    Off,
    /// Waiting for work.
    Idle,
    /// Not asking for work (see `paused_reason`).
    Paused,
    /// Cannot reach the coordinator; retrying.
    Offline,
    /// Downloading a job.
    Downloading,
    /// Running a job in the sandbox.
    Running,
    /// Uploading a result.
    Uploading,
}

/// `nc_agent::Activity` as a plain struct. Never carries job data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelperActivity {
    /// What the helper is doing.
    pub kind: ActivityKind,
    /// Owner-readable reason when `kind` is `Paused`.
    pub paused_reason: Option<String>,
}

impl From<nc_agent::Activity> for HelperActivity {
    fn from(a: nc_agent::Activity) -> Self {
        use nc_agent::Activity as A;
        let (kind, paused_reason) = match a {
            A::Off => (ActivityKind::Off, None),
            A::Idle => (ActivityKind::Idle, None),
            A::Paused(r) => (ActivityKind::Paused, Some(r)),
            A::Offline => (ActivityKind::Offline, None),
            A::Downloading => (ActivityKind::Downloading, None),
            A::Running => (ActivityKind::Running, None),
            A::Uploading => (ActivityKind::Uploading, None),
        };
        Self {
            kind,
            paused_reason,
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

/// How this device reaches the coordinator. The owner states it; the platform cannot confirm it yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkKind {
    /// Wired.
    Ethernet,
    /// Wi-Fi.
    Wifi,
}

/// What the owner enters to enrol. `enrol_secret` is sent once to the coordinator and never stored.
#[derive(Clone, PartialEq, Eq)]
pub struct EnrolRequest {
    /// Coordinator base URL, `https://...`.
    pub coordinator_url: String,
    /// Enrolment secret from the coordinator owner.
    pub enrol_secret: String,
    /// Name shown on the coordinator, 1 to 64 characters.
    pub device_name: String,
    /// Wired or Wi-Fi.
    pub network: NetworkKind,
    /// Owner's name for this connection, e.g. `home`.
    pub connection_name: String,
    /// Conservative connection speed, Mbit/s.
    pub speed_mbps: u32,
    /// The connection has no data limit (no night-data budget needed).
    pub unlimited_data: bool,
    /// The owner confirms the device stays cool enough (no thermal sensor is readable).
    pub cool_device: bool,
    /// Public keys of the people whose jobs this device may run (base64url or hex), 1 to 16.
    pub trusted_publishers: Vec<String>,
}

impl std::fmt::Debug for EnrolRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the enrolment secret.
        f.debug_struct("EnrolRequest")
            .field("coordinator_url", &self.coordinator_url)
            .field("device_name", &self.device_name)
            .finish_non_exhaustive()
    }
}

/// The saved enrolment. No secrets, no keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Enrolment {
    /// Coordinator base URL.
    pub coordinator_url: String,
    /// Name on the coordinator.
    pub device_name: String,
    /// The connection has no data limit.
    pub unlimited_data: bool,
    /// Number of trusted publisher keys.
    pub trusted_publishers: u32,
    /// This device's public key (base64url), to share with people who should trust it. Never the private key.
    pub device_key: String,
    /// The running agent uses this coordinator now (false until the app restarts after enrolling).
    pub active: bool,
}

/// Result of [`open_agent`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentStart {
    /// Saved enrolment, if any.
    pub enrolment: Option<Enrolment>,
    /// Plain-English problem with the saved enrolment (the app then runs without a coordinator).
    pub notice: Option<String>,
}

/// What the coordinator says about the saved enrolment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnrolmentState {
    /// Nothing is saved on this device.
    NotEnrolled,
    /// The coordinator knows this device.
    Registered,
    /// The coordinator no longer knows this device (removed or reset there). Enrol again.
    NotRegistered,
    /// No answer; the enrolment may still be fine. See `message`.
    Unknown,
}

/// Result of [`check_enrolment`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnrolmentCheck {
    /// What the coordinator said.
    pub state: EnrolmentState,
    /// Plain-English reason when `state` is `Unknown`.
    pub message: Option<String>,
}

impl EnrolmentCheck {
    fn of(state: EnrolmentState) -> Self {
        Self {
            state,
            message: None,
        }
    }
}

/// What the owner asks for on the Requester screen. Paths only: job bytes never cross the bridge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubmitJobRequest {
    /// The signed-by-this-device Wasm module file.
    pub module_path: String,
    /// The input file.
    pub input_path: String,
    /// Independent helpers that must agree, 1 or 2.
    pub replicas: u32,
}

/// Where a requested job is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobPhase {
    /// Recorded on this device; not yet accepted by the coordinator.
    Uploading,
    /// Waiting for a helper.
    Queued,
    /// A helper is working on it.
    Running,
    /// Result ready.
    Committed,
    /// Two helpers disagreed; no result.
    Disputed,
    /// Ended without a result.
    Failed,
    /// Not known yet (refresh to ask the coordinator).
    Unknown,
}

/// One requested job. No job data; the output hash and size are public metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestedJob {
    /// Job id (UUID).
    pub job_id: String,
    /// Where it is.
    pub phase: JobPhase,
    /// When it was recorded (unix ms).
    pub created_ms: i64,
    /// Committed output SHA-256 (hex), once known.
    pub output_sha256: Option<String>,
    /// Committed output size, bytes.
    pub output_size: Option<u64>,
    /// Plain-English problem from the last attempt (e.g. offline), if any.
    pub message: Option<String>,
}

/// A saved, hash-checked output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchedOutput {
    /// SHA-256 (hex) the bytes were checked against.
    pub sha256: String,
    /// Bytes written.
    pub size: u64,
}

static AGENT: OnceLock<Agent> = OnceLock::new();
/// The enrolment the running agent was built from.
static ACTIVE: Mutex<Option<enrol::EnrolFile>> = Mutex::new(None);
/// Set after unenrol: the running agent still holds the old identity, so no new session until restart.
static RESET_PENDING: AtomicBool = AtomicBool::new(false);

fn agent() -> &'static Agent {
    AGENT.get_or_init(Agent::new)
}

fn active() -> Option<enrol::EnrolFile> {
    ACTIVE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// The owner's limits plus the trust from the active enrolment (none when not enrolled).
fn with_trust(p: nc_policy::OwnerPolicy) -> nc_policy::OwnerPolicy {
    match active() {
        Some(f) => f.apply_trust(p),
        None => p,
    }
}

/// Creates the agent. With a saved enrolment in `dir` it is `Agent::with_backend` (real coordinator, leases,
/// budget and today counters); otherwise `Agent::new()`. Call once at start-up, before anything else.
#[frb(sync)]
pub fn open_agent(dir: String) -> AgentStart {
    let dir = Path::new(&dir);
    if AGENT.get().is_some() {
        return AgentStart {
            enrolment: active().map(|f| f.summary(dir, true)),
            notice: None,
        };
    }
    let built = enrol::load(dir, enrol::dev_http_allowed()).and_then(|f| match f {
        None => Ok(None),
        Some(f) => {
            let night = settings::load(dir).settings.night_data;
            let budget = enrol::budget(&f, &night)?;
            enrol::build_agent(dir, &f, &budget).map(|a| Some((f, a)))
        }
    });
    match built {
        Ok(Some((f, a))) => {
            let summary = f.summary(dir, true);
            if AGENT.set(a).is_ok() {
                *ACTIVE.lock().unwrap_or_else(PoisonError::into_inner) = Some(f);
            }
            AgentStart {
                enrolment: Some(summary),
                notice: None,
            }
        }
        Ok(None) => {
            agent();
            AgentStart {
                enrolment: None,
                notice: None,
            }
        }
        Err(e) => {
            agent();
            AgentStart {
                enrolment: None,
                notice: Some(e),
            }
        }
    }
}

/// Registers this device with the coordinator and saves the enrolment in `dir`. Takes effect on the next
/// start of the app. Plain-English errors; the secret is never logged or stored.
pub fn enrol(dir: String, request: EnrolRequest) -> Result<Enrolment, String> {
    if RESET_PENDING.load(Ordering::SeqCst) {
        return Err("Restart the app to finish the reset first.".into());
    }
    enrol::enrol(Path::new(&dir), request, enrol::dev_http_allowed())
}

/// Stops helping, then removes the enrolment and this device's identity, journal and cache from `dir`.
/// Until the app restarts, no new helper session can start.
pub fn unenrol(dir: String) -> Result<(), String> {
    agent().stop_helper_session();
    if active().is_some() {
        RESET_PENDING.store(true, Ordering::SeqCst);
    }
    enrol::unenrol(Path::new(&dir))
}

/// Asks the coordinator (signed `POST /v1/devices/me`) whether it still knows this device. Call after
/// [`open_agent`]; it makes a network call. When the device is not registered any more, helping stops.
pub fn check_enrolment(dir: String) -> EnrolmentCheck {
    let dir = Path::new(&dir);
    let dev = enrol::dev_http_allowed();
    let file = match enrol::load(dir, dev) {
        Ok(Some(f)) => f,
        Ok(None) => return EnrolmentCheck::of(EnrolmentState::NotEnrolled),
        Err(message) => {
            return EnrolmentCheck {
                state: EnrolmentState::Unknown,
                message: Some(message),
            }
        }
    };
    match enrol::check_registration(dir, &file, dev) {
        enrol::Registration::Registered => EnrolmentCheck::of(EnrolmentState::Registered),
        enrol::Registration::NotRegistered => {
            // Fail closed: the coordinator would refuse every signed call from this key anyway.
            if active().is_some() {
                agent().stop_helper_session();
            }
            EnrolmentCheck::of(EnrolmentState::NotRegistered)
        }
        enrol::Registration::Unknown(message) => EnrolmentCheck {
            state: EnrolmentState::Unknown,
            message: Some(message),
        },
    }
}

/// Stops helping, then tells the coordinator to forget this device (signed `POST /v1/devices/deregister`).
/// Call before [`unenrol`]. `Ok` when the coordinator removed it, already did not know it, or nothing is
/// saved. On `Err` (plain English, e.g. offline) the owner may still reset with [`unenrol`].
pub fn deregister_device(dir: String) -> Result<(), String> {
    agent().stop_helper_session();
    enrol::deregister(Path::new(&dir), enrol::dev_http_allowed())
}

/// Requester: reads the module and input files, signs the job with this device's key (as `nc submit`),
/// records it on this device, then sends it. A job recorded but not yet sent comes back as `Uploading`
/// with a message; [`job_status`] sends it later. Helpers must trust this device's key (Coordinator page).
pub fn submit_job(dir: String, request: SubmitJobRequest) -> Result<RequestedJob, String> {
    let dir = Path::new(&dir);
    let c = requests::ctx(dir, enrol::dev_http_allowed())?;
    requests::submit(&c, &request, chrono::Utc::now().timestamp_millis())
}

/// Requester: every job recorded on this device, newest first (local files; survives restarts).
pub fn list_jobs(dir: String) -> Vec<RequestedJob> {
    let dir = Path::new(&dir);
    requests::list(&requests::store(dir), dir)
}

/// Requester: asks the coordinator about one job (network, retried briefly), sending it first if needed.
pub fn job_status(dir: String, job_id: String) -> Result<RequestedJob, String> {
    let c = requests::ctx(Path::new(&dir), enrol::dev_http_allowed())?;
    requests::status(&c, &job_id)
}

/// Requester: downloads the committed output, checks its SHA-256 and size, and saves it to `out_path`.
pub fn fetch_output(
    dir: String,
    job_id: String,
    out_path: String,
) -> Result<FetchedOutput, String> {
    let c = requests::ctx(Path::new(&dir), enrol::dev_http_allowed())?;
    requests::fetch(&c, &job_id, &out_path)
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
    if RESET_PENDING.load(Ordering::SeqCst) {
        return Err("Restart the app to finish the reset.".into());
    }
    let p = with_trust(settings::to_policy(&policy)?);
    agent().start_helper_session(p).map_err(|e| e.to_string())
}

/// `Agent::update_policy(policy)`: applies new limits to the running session (and later ones) at once.
/// A running job the new limits no longer allow is cancelled by nc-agent.
pub fn update_policy(policy: PolicySummary) -> Result<(), String> {
    let p = with_trust(settings::to_policy(&policy)?);
    agent().update_policy(p).map_err(|e| e.to_string())
}

/// `Agent::update_network_budget(Metered(profile))` from the night-data settings, applied live.
pub fn update_night_data(night_data: NightDataSettings) -> Result<(), String> {
    let budget = match active() {
        Some(f) => enrol::budget(&f, &night_data)?,
        None => nc_agent::NetworkBudget::Metered(night::to_profile(&night_data)?),
    };
    agent()
        .update_network_budget(budget)
        .map_err(|e| e.to_string())
}

/// `Agent::stop_helper_session()`. Returns when the session is `Stopped`.
pub fn stop_helper_session() {
    agent().stop_helper_session();
}

/// Version of the Rust core (nc-bridge), for the About page.
#[frb(sync)]
pub fn core_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// `Agent::today()`: today's counters.
#[frb(sync)]
pub fn today_summary() -> TodaySummary {
    agent().today().into()
}

/// `Agent::activity()`: what the helper is doing now (low frequency; the UI polls it with the status).
#[frb(sync)]
pub fn helper_activity() -> HelperActivity {
    agent().activity().into()
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
    fn invalid_update_is_refused_and_valid_one_applies() {
        let mut s = default_policy();
        s.cpu_budget_percent = 0;
        assert!(update_policy(s).is_err());
        let mut ok = default_policy();
        ok.cpu_budget_percent = 40;
        update_policy(ok).unwrap();
        assert_eq!(agent().policy().cpu_budget_percent, 40);
    }

    #[test]
    fn night_data_update_validates() {
        let mut n = default_settings().night_data;
        update_night_data(n.clone()).unwrap();
        n.nightly_budget_mb = 0;
        assert!(update_night_data(n).is_err());
    }

    #[test]
    fn today_comes_from_the_agent() {
        let t = today_summary();
        assert_eq!(t.jobs_done, 0);
        assert_eq!(helper_activity().kind, ActivityKind::Off);
    }

    #[test]
    fn state_mirror_is_complete() {
        assert_eq!(
            HelperState::from(SessionState::NotAccepting),
            HelperState::NotAccepting
        );
    }
}
