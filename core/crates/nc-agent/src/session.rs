//! The real helper session loop (PLAN.md §3, §6, §7; AGENT.md §1 rules 1, 3–5, 8, 10).
//!
//! ```text
//! register → loop { recover journal → policy + tariff gate → heartbeat → lease
//!                   → verify signed manifest → owner policy → budget pre-check
//!                   → download (Range resume, tariff-gated) → run in nc-exec (Stop < 1 s)
//!                   → upload output → Complete v2 }
//! ```
//! From the grant until Complete or Fail a renewal thread keeps the lease alive (`POST /v1/leases/renew` every
//! ~20 s, sooner when under 30 s are left). If the coordinator says the lease is gone (404, 409
//! `lease_expired` / `attempt_mismatch`), or the lease runs out because renewals keep failing or the server cap
//! stopped extending it, the job is stopped at once and its result is never uploaded or completed.
//! Any refusal after a lease is reported with `POST /v1/leases/fail` and the matching reason. Logs carry
//! reason codes and sizes only, never manifests, blob bytes, keys or tokens.

use crate::budget::{BudgetGate, Refusal};
use crate::coord::{CoordError, Coordinator};
use crate::journal::{Entry, Journal, Local};
use crate::parked::ParkedResults;
use crate::probe::{Clock, PlatformProbe};
use crate::runner::JobRunner;
use crate::today::Counters;
use crate::transfer::{
    self, BlobCache, TransferError, UploadError, UploadGate, Uploader, UPLOAD_CHUNK,
    UPLOAD_OVERHEAD,
};
use nc_exec::{CancelToken, ExecError, Termination};
use nc_policy::{
    admit, check_running, Admission, DeviceState, OwnerPolicy, PeerKey, RunningDecision,
    RunningTask, TaskOffer,
};
use nc_protocol::jobs::verify_job;
use nc_protocol::pb::v1::{
    CompleteRequest, HeartbeatRequest, JobManifest, LeaseFailReason, LeaseFailRequest,
    LeaseRenewRequest, LeaseResponse, RegisterRequest, TaskManifest, TransferRef,
};
use nc_protocol::PROFILE_CPU_CORE_V1;
use nc_store::{CommitOutcome, TaskState};
use nc_tariff::TrafficClass;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// Approximate wire bytes charged for one control call (heartbeat, lease request, renew, fail, complete).
const CONTROL_BYTES: u64 = 4096;

/// When the server cap stops extending a lease, the job is stopped this long before the lease ends.
const CAPPED_GUARD_MS: i64 = 5_000;

/// Longest wait between renewal retries after a network error.
const RENEW_MAX_BACKOFF: Duration = Duration::from_secs(5);

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Timing knobs.
#[derive(Clone, Copy, Debug)]
pub struct Tuning {
    /// Poll interval when there is no work (plus up to 50 % jitter).
    pub poll: Duration,
    /// Backoff ceiling on network errors.
    pub max_backoff: Duration,
    /// How often the run monitor re-checks Stop and policy.
    pub monitor_tick: Duration,
    /// Lease renewal interval while a lease is held.
    pub renew_every: Duration,
    /// Renew early when less than this is left on the lease.
    pub renew_margin: Duration,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            poll: Duration::from_secs(10),
            max_backoff: Duration::from_secs(60),
            monitor_tick: Duration::from_millis(50),
            renew_every: Duration::from_secs(20),
            renew_margin: Duration::from_secs(30),
        }
    }
}

/// What the helper is doing right now (more detail than [`crate::SessionState`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Activity {
    /// No session.
    Off,
    /// Waiting for work.
    Idle,
    /// Not asking for work; owner-readable reason (no job data).
    Paused(String),
    /// Cannot reach the coordinator; retrying with backoff.
    Offline,
    /// Downloading a job's module and inputs.
    Downloading,
    /// Running a job in the sandbox.
    Running,
    /// Uploading a result.
    Uploading,
}

/// Everything the real session needs.
pub struct Backend {
    /// Coordinator client.
    pub coord: Arc<dyn Coordinator>,
    /// Platform probe.
    pub probe: Arc<dyn PlatformProbe>,
    /// Clock.
    pub clock: Arc<dyn Clock>,
    /// Job runner (nc-exec in production).
    pub runner: Arc<dyn JobRunner>,
    /// Lease journal (nc-store).
    pub journal: Mutex<Journal>,
    /// Encrypted input cache (nc-blob).
    pub cache: Mutex<BlobCache>,
    /// Encrypted finished results waiting for an upload window (nc-blob, survives restarts).
    pub parked: Mutex<ParkedResults>,
    /// Network budget gate (nc-tariff).
    pub budget: BudgetGate,
    /// Private staging folder for partial downloads.
    pub staging: PathBuf,
    /// Display name sent on register.
    pub device_name: String,
    /// Platform name sent on register.
    pub platform: String,
    /// Timing knobs.
    pub tuning: Tuning,
    /// nc-net node for direct job-byte transfers (1.L15); `None` keeps every byte on the HTTP path.
    pub peer: Option<crate::peer::AgentPeer>,
}

impl std::fmt::Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backend").finish_non_exhaustive()
    }
}

/// State shared between the [`crate::Agent`] facade and the session thread.
#[derive(Debug)]
pub(crate) struct Live {
    pub(crate) policy: Mutex<OwnerPolicy>,
    pub(crate) cancel: Mutex<Option<CancelToken>>,
    pub(crate) activity: Mutex<Activity>,
    pub(crate) counters: Mutex<Counters>,
}

impl Live {
    pub(crate) fn new() -> Self {
        Self {
            policy: Mutex::new(OwnerPolicy::default()),
            cancel: Mutex::new(None),
            activity: Mutex::new(Activity::Off),
            counters: Mutex::new(Counters::new()),
        }
    }
    pub(crate) fn policy(&self) -> OwnerPolicy {
        lock(&self.policy).clone()
    }
    pub(crate) fn set_activity(&self, a: Activity) {
        *lock(&self.activity) = a;
    }
    /// Cancel the running job, if any (Stop path; nc-exec kills it within one watchdog tick).
    pub(crate) fn cancel_job(&self) {
        if let Some(c) = lock(&self.cancel).as_ref() {
            c.cancel();
        }
    }
}

/// Stop/wake control shared with the facade.
#[derive(Debug, Default)]
pub(crate) struct Ctl {
    pub(crate) m: Mutex<CtlState>,
    pub(crate) cv: Condvar,
}

#[derive(Debug, Default)]
pub(crate) struct CtlState {
    pub(crate) stop: bool,
    pub(crate) wake: bool,
}

impl Ctl {
    pub(crate) fn stopped(&self) -> bool {
        lock(&self.m).stop
    }
    /// Sleep up to `d`; returns true if Stop was requested. A wake (policy change) ends the wait early.
    pub(crate) fn wait(&self, d: Duration) -> bool {
        let deadline = Instant::now() + d;
        let mut g = lock(&self.m);
        loop {
            if g.stop {
                return true;
            }
            if g.wake {
                g.wake = false;
                return false;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            g = self
                .cv
                .wait_timeout(g, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
    pub(crate) fn wake(&self) {
        lock(&self.m).wake = true;
        self.cv.notify_all();
    }
}

/// A refusal to report with LeaseFail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Fail {
    pub(crate) reason: LeaseFailReason,
    pub(crate) code: &'static str,
}

fn fail(reason: LeaseFailReason, code: &'static str) -> Fail {
    Fail { reason, code }
}

/// Activity text while a finished result waits for the upload window (an `Activity::Paused` reason, so the
/// UI bridge needs no new variant).
pub const PARKED_TEXT: &str = "A finished result is kept until the next upload window.";

/// How `process` ended without a failure.
enum Done {
    /// Uploaded and journalled; send Complete.
    Uploaded(Entry),
    /// The upload is not allowed now; the result is parked.
    Parked,
}

/// Why an upload attempt did not happen.
enum UploadErr {
    /// The tariff does not allow it now (window or budget); try again later.
    NotNow(Refusal),
    /// The upload itself failed with a network error; try again later.
    Transient,
    /// Give up on this attempt.
    Fail(Fail),
}

/// Digest that ties a parked result to the exact signed job it was computed for.
fn job_digest(m: &nc_protocol::pb::v1::SignedJobManifest) -> String {
    hex::encode(Sha256::digest(prost::Message::encode_to_vec(m)))
}

fn jitter(max: Duration) -> Duration {
    let mut b = [0u8; 4];
    if getrandom::fill(&mut b).is_err() {
        return Duration::ZERO;
    }
    let ms = u64::try_from(max.as_millis()).unwrap_or(u64::MAX);
    if ms == 0 {
        return Duration::ZERO;
    }
    Duration::from_millis(u64::from(u32::from_le_bytes(b)) % ms)
}

struct Backoff {
    next: Duration,
    max: Duration,
}

impl Backoff {
    fn new(max: Duration) -> Self {
        Self {
            next: Duration::from_millis(500),
            max,
        }
    }
    fn step(&mut self) -> Duration {
        let d = self.next;
        self.next = (self.next * 2).min(self.max);
        d + jitter(d / 2 + Duration::from_millis(1))
    }
    fn reset(&mut self) {
        self.next = Duration::from_millis(500);
    }
}

/// The session context for one run of the loop.
pub(crate) struct Session<'a> {
    pub(crate) b: &'a Backend,
    pub(crate) live: &'a Live,
    pub(crate) ctl: &'a Ctl,
    /// Called when the owner's consent changes, to publish the facade state.
    pub(crate) on_consent: &'a dyn Fn(bool),
}

fn device_state(b: &Backend, running: Vec<RunningTask>) -> DeviceState {
    let d = b.probe.device();
    DeviceState {
        now: b.clock.now(),
        on_ac_power: d.on_ac_power,
        battery_percent: d.battery_percent,
        thermal_level: d.thermal_level,
        user_idle_seconds: d.user_idle_seconds,
        running_tasks: running,
        memory_in_use_mb: 0,
        // Parked results count against the owner's disk cap.
        disk_in_use_mb: mib_ceil(lock(&b.parked).bytes()),
        tariff_decision: None,
    }
}

fn reason_code(r: &nc_policy::Reason) -> &'static str {
    use nc_policy::Reason as R;
    match r {
        R::InvalidPolicy(_) => "INVALID_POLICY",
        R::PolicyWithdrawn => "CONSENT_OFF",
        R::BlockedPeer => "BLOCKED_PUBLISHER",
        R::UntrustedPeer => "UNTRUSTED_PUBLISHER",
        R::ExecutionProfileNotAllowed => "PROFILE",
        R::ConcurrencyLimit => "CONCURRENCY",
        R::MemoryLimit => "MEMORY_LIMIT",
        R::DiskLimit => "DISK_LIMIT",
        R::InvalidTaskDuration | R::TaskDurationLimit | R::TaskClockInvalid => "DURATION_LIMIT",
        R::ChargingRequired => "CHARGING",
        R::BatteryUnknown | R::InvalidBatteryReading | R::BatteryTooLow => "BATTERY",
        R::ThermalUnknown | R::ThermalThreshold | R::CriticalThermal => "THERMAL",
        R::UserActivityUnknown | R::UserActive => "USER_ACTIVE",
        R::OutsideHours | R::HoursUnavailable => "HOURS",
        _ => "TARIFF",
    }
}

fn hex_ok(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

/// Check the lease's transfer refs against the signed manifest. v1 helpers accept only unencrypted transfers
/// (transport bytes = plaintext), so the transport hash must equal the signed plaintext hash.
fn check_transfers(job: &JobManifest, task: &TaskManifest, t: &[TransferRef]) -> Result<(), Fail> {
    let bad = || fail(LeaseFailReason::VerifyFailed, "TRANSFERS");
    let module = job.module_transfer.as_ref().ok_or_else(bad)?;
    if module.sha256 != task.module_sha256 || job.input_transfers != task.inputs {
        return Err(fail(LeaseFailReason::VerifyFailed, "ENCRYPTED_UNSUPPORTED"));
    }
    if t.len() != 1 + task.inputs.len() {
        return Err(bad());
    }
    let refs = std::iter::once(module).chain(job.input_transfers.iter());
    for (r, signed) in t.iter().zip(refs) {
        if !hex_ok(&r.sha256)
            || r.sha256 != hex::encode(&signed.sha256)
            || r.size != signed.size
            || r.path != format!("/v1/blobs/{}", r.sha256)
        {
            return Err(bad());
        }
    }
    Ok(())
}

fn mib_ceil(bytes: u64) -> u64 {
    bytes.div_ceil(1024 * 1024)
}

fn transfer_fail(e: &TransferError) -> Fail {
    match e {
        TransferError::Stopped => fail(LeaseFailReason::StoppedByOwner, "STOP"),
        TransferError::Window(code) => fail(LeaseFailReason::TariffWindow, code),
        TransferError::Budget => fail(LeaseFailReason::Budget, "BUDGET"),
        TransferError::Verify => fail(LeaseFailReason::VerifyFailed, "BLOB_HASH"),
        TransferError::Coord(_) => fail(LeaseFailReason::Other, "NETWORK"),
        TransferError::Io => fail(LeaseFailReason::Other, "LOCAL_IO"),
    }
}

fn termination_fail(t: &Termination, cause: StopCause) -> Option<Fail> {
    use LeaseFailReason as R;
    Some(match t {
        Termination::Exited(0) => return None,
        Termination::Cancelled => match cause {
            StopCause::Policy => fail(R::StoppedByOwner, "POLICY"),
            _ => fail(R::StoppedByOwner, "STOP"),
        },
        Termination::OutOfFuel => fail(R::Timeout, "FUEL"),
        Termination::EpochDeadline => fail(R::Timeout, "EPOCH"),
        Termination::WatchdogKilled => fail(R::Timeout, "WATCHDOG"),
        Termination::MemoryLimit | Termination::JobMemoryKilled => fail(R::Crashed, "MEMORY"),
        Termination::StackOverflow => fail(R::Crashed, "STACK"),
        Termination::OutputLimit => fail(R::Crashed, "OUTPUT_LIMIT"),
        Termination::Trap(_) => fail(R::Crashed, "TRAP"),
        Termination::EscapeAttempt => fail(R::Crashed, "ESCAPE"),
        Termination::WorkerCrashed(_) => fail(R::Crashed, "WORKER"),
        Termination::Exited(_) => fail(R::Crashed, "EXIT_NONZERO"),
    })
}

fn exec_fail(e: &ExecError) -> Fail {
    use LeaseFailReason as R;
    match e {
        ExecError::Sandbox(_) => fail(R::SandboxUnavailable, "SANDBOX"),
        ExecError::HashMismatch | ExecError::SignatureInvalid(_) => fail(R::VerifyFailed, "MODULE"),
        ExecError::BadLimits(_) | ExecError::TooLarge => fail(R::VerifyFailed, "LIMITS"),
        ExecError::Refused(_) => fail(R::VerifyFailed, "MODULE_REFUSED"),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StopCause {
    None,
    Stop,
    Policy,
    Lease,
}

/// Shared between the job and its lease-renewal thread.
#[derive(Debug, Default)]
struct LeaseWatch {
    /// Set when the lease is gone or about to end; the job must stop and drop its result.
    lost: AtomicBool,
    /// Set when the job no longer needs the lease (renewals stop).
    done: AtomicBool,
    /// Why the lease was lost (log code only).
    why: Mutex<&'static str>,
}

impl LeaseWatch {
    fn lose(&self, why: &'static str) {
        *lock(&self.why) = why;
        self.lost.store(true, Ordering::SeqCst);
    }
    fn lost(&self) -> bool {
        self.lost.load(Ordering::SeqCst)
    }
}

fn lease_lost_fail() -> Fail {
    fail(LeaseFailReason::Timeout, "LEASE_LOST")
}

fn unix_ms(secs: u64) -> i64 {
    i64::try_from(secs)
        .unwrap_or(i64::MAX / 1000)
        .saturating_mul(1000)
}

/// Charge a control call to the budget; `false` means the tariff forbids even control traffic now.
fn control_allowed(b: &Backend) -> bool {
    let net = b.probe.network();
    match b
        .budget
        .authorize(TrafficClass::Control, CONTROL_BYTES, b.clock.now(), &net)
    {
        Ok(g) => {
            b.budget.settle(g, CONTROL_BYTES);
            true
        }
        Err(_) => false,
    }
}

/// Refresh the peer registration when due, charged to the tariff as control traffic.
fn refresh_peer(b: &Backend) {
    if let Some(p) = &b.peer {
        p.net
            .refresh_if_due(b.coord.as_ref(), &p.device_public, || control_allowed(b));
    }
}

/// Renewal thread body: keeps the lease alive until `watch.done`, or marks it lost.
fn keep_lease(b: &Backend, l: &LeaseResponse, watch: &LeaseWatch) {
    let t = b.tuning;
    let req = LeaseRenewRequest {
        lease_id: l.lease_id.clone(),
        attempt_id: l.attempt_id.clone(),
    };
    let mut expires_ms = unix_ms(l.expires_at);
    let margin_ms = i64::try_from(t.renew_margin.as_millis()).unwrap_or(i64::MAX);
    let min_gap = t.renew_every / 4;
    let mut backoff = Backoff::new(t.max_backoff.min(RENEW_MAX_BACKOFF));
    let mut last_ok = Instant::now();
    let mut next_try = Instant::now();
    let mut capped_stuck = false;
    while !watch.done.load(Ordering::SeqCst) {
        let now = b.clock.now().timestamp_millis();
        if now >= expires_ms {
            watch.lose("LEASE_EXPIRED");
            return;
        }
        if capped_stuck && now >= expires_ms.saturating_sub(CAPPED_GUARD_MS) {
            watch.lose("LEASE_CAPPED");
            return;
        }
        let due = !capped_stuck
            && Instant::now() >= next_try
            && (last_ok.elapsed() >= t.renew_every || expires_ms.saturating_sub(now) < margin_ms);
        refresh_peer(b);
        if due {
            if control_allowed(b) {
                match b.coord.renew(&req) {
                    Ok(r) => {
                        backoff.reset();
                        last_ok = Instant::now();
                        next_try = last_ok + min_gap;
                        let new = unix_ms(r.expires_at);
                        if r.capped && new <= expires_ms {
                            capped_stuck = true;
                        }
                        // Never shorten what we already hold.
                        expires_ms = expires_ms.max(new);
                    }
                    Err(e) if e.lease_gone() => {
                        watch.lose("LEASE_GONE");
                        return;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "lease renew failed; retrying");
                        next_try = Instant::now() + backoff.step();
                    }
                }
            } else {
                // The tariff forbids even control traffic now; retry until the lease runs out.
                next_try = Instant::now() + backoff.step();
            }
        }
        std::thread::sleep(t.monitor_tick);
    }
}

impl Session<'_> {
    fn now_ms(&self) -> i64 {
        self.b.clock.now().timestamp_millis()
    }

    /// Charge a control call to the budget; `false` means the tariff forbids even control traffic now.
    fn control_allowed(&self) -> bool {
        control_allowed(self.b)
    }

    /// Main loop. Returns when Stop is requested.
    pub(crate) fn run(&self) {
        let mut backoff = Backoff::new(self.b.tuning.max_backoff);
        // Register (idempotent) before anything else.
        loop {
            if self.ctl.stopped() {
                return;
            }
            if !self.control_allowed() {
                self.live
                    .set_activity(Activity::Paused("Outside the data window.".into()));
                if self.ctl.wait(self.b.tuning.poll) {
                    return;
                }
                continue;
            }
            let req = RegisterRequest {
                name: self.b.device_name.clone(),
                platform: self.b.platform.clone(),
            };
            match self.b.coord.register(&req) {
                Ok(()) => break,
                Err(e) => {
                    tracing::warn!(error = %e, "register failed; retrying");
                    self.live.set_activity(Activity::Offline);
                    let d = if e.is_transient() {
                        backoff.step()
                    } else {
                        self.b.tuning.max_backoff
                    };
                    if self.ctl.wait(d) {
                        return;
                    }
                }
            }
        }
        backoff.reset();
        let mut last_consent = None;
        loop {
            if self.ctl.stopped() {
                return;
            }
            self.recover();
            let policy = self.live.policy();
            if last_consent != Some(policy.accept_jobs) {
                last_consent = Some(policy.accept_jobs);
                (self.on_consent)(policy.accept_jobs);
            }
            if let Err(why) = self.ready(&policy) {
                self.live.set_activity(Activity::Paused(why));
                if self.ctl.wait(self.b.tuning.poll) {
                    return;
                }
                continue;
            }
            if !self.control_allowed() {
                self.live
                    .set_activity(Activity::Paused("Outside the data window.".into()));
                if self.ctl.wait(self.b.tuning.poll) {
                    return;
                }
                continue;
            }
            refresh_peer(self.b);
            let hb = HeartbeatRequest {
                status: "idle".into(),
            };
            let res = self
                .b
                .coord
                .heartbeat(&hb)
                .and_then(|()| self.b.coord.lease());
            match res {
                Ok(None) => {
                    backoff.reset();
                    self.live.set_activity(Activity::Idle);
                    let d = self.b.tuning.poll + jitter(self.b.tuning.poll / 2);
                    if self.ctl.wait(d) {
                        return;
                    }
                }
                Ok(Some(lease)) => {
                    backoff.reset();
                    self.handle_lease(&lease);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "coordinator call failed; backing off");
                    self.live.set_activity(Activity::Offline);
                    if self.ctl.wait(backoff.step()) {
                        return;
                    }
                }
            }
        }
    }

    /// May this device ask for work right now? `Err` carries an owner-readable reason.
    pub(crate) fn ready(&self, policy: &OwnerPolicy) -> Result<(), String> {
        if !policy.accept_jobs {
            return Err("The owner has disabled helper work.".into());
        }
        if policy.trusted_peers.is_empty() {
            return Err("No trusted publishers are set.".into());
        }
        if !policy
            .allowed_execution_profiles
            .iter()
            .any(|p| p == PROFILE_CPU_CORE_V1)
        {
            return Err("The cpu-core-v1 profile is not allowed.".into());
        }
        match check_running(policy, &device_state(self.b, Vec::new())) {
            RunningDecision::Continue => {}
            RunningDecision::Pause { reason } | RunningDecision::Stop { reason } => {
                return Err(reason.to_string())
            }
        }
        let net = self.b.probe.network();
        match self
            .b
            .budget
            .check(TrafficClass::Bulk, 1, self.b.clock.now(), &net)
        {
            Ok(_) => Ok(()),
            Err(Refusal::Budget) => Err("The network data budget is used up.".into()),
            Err(Refusal::Window) => Err("Outside the data window or on another network.".into()),
        }
    }

    fn send_fail(&self, e: &Entry, f: &Fail) -> Result<(), CoordError> {
        tracing::info!(reason = ?f.reason, code = f.code, "reporting lease failure");
        let req = LeaseFailRequest {
            lease_id: e.lease_id.clone(),
            attempt_id: e.attempt_id.clone(),
            reason: f.reason as i32,
            code: Some(f.code.to_string()),
        };
        self.b.coord.fail(&req).map(|_| ())
    }

    /// Finish attempts a crash (or an unreachable coordinator) left open.
    pub(crate) fn recover(&self) {
        let pending = match lock(&self.b.journal).pending() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "journal unreadable");
                return;
            }
        };
        lock(&self.b.parked).sweep(self.now_ms());
        for (entry, local, state) in pending {
            match state {
                TaskState::Uploaded | TaskState::Verified => self.complete(&entry, local),
                TaskState::Queued | TaskState::Leased | TaskState::Running
                    if lock(&self.b.parked).by_attempt(&entry.attempt_id).is_some() =>
                {
                    self.resume_parked(&entry, local);
                }
                TaskState::Queued | TaskState::Leased | TaskState::Running => {
                    let f = fail(LeaseFailReason::Crashed, "AGENT_RESTART");
                    match self.send_fail(&entry, &f) {
                        Err(e) if e.is_transient() => {}
                        _ => {
                            let _ = lock(&self.b.journal).failed(local, self.now_ms());
                        }
                    }
                }
                TaskState::Committed | TaskState::Failed => {
                    let _ = lock(&self.b.journal).failed(local, self.now_ms());
                }
            }
        }
    }

    /// A result parked by an earlier run (or before a Stop): upload it if the tariff allows it now, otherwise
    /// keep its lease alive where control traffic is allowed. A lease the coordinator no longer holds closes
    /// the local record; the parked result stays for a later lease of the same task.
    fn resume_parked(&self, entry: &Entry, local: Local) {
        match self.upload_parked(entry, local, None) {
            Ok(done) => self.complete(&done, local),
            Err(UploadErr::NotNow(_) | UploadErr::Transient) => {
                if !self.control_allowed() {
                    return;
                }
                let req = LeaseRenewRequest {
                    lease_id: entry.lease_id.clone(),
                    attempt_id: entry.attempt_id.clone(),
                };
                match self.b.coord.renew(&req) {
                    Err(e) if e.lease_gone() => {
                        tracing::info!("parked lease ended; result kept for a new lease");
                        let _ = lock(&self.b.journal).failed(local, self.now_ms());
                    }
                    Err(e) => tracing::warn!(error = %e, "parked lease renew failed; retrying"),
                    Ok(_) => {}
                }
            }
            Err(UploadErr::Fail(f)) => {
                let _ = self.send_fail(entry, &f);
                let _ = lock(&self.b.journal).failed(local, self.now_ms());
            }
        }
    }

    /// Upload the parked output bound to `entry`'s attempt.
    fn upload_parked(
        &self,
        entry: &Entry,
        local: Local,
        watch: Option<&LeaseWatch>,
    ) -> Result<Entry, UploadErr> {
        let output = {
            let p = lock(&self.b.parked);
            match p.by_attempt(&entry.attempt_id) {
                Some(x) => p.read(&x.task_id.clone()),
                None => None,
            }
        };
        let Some(output) = output else {
            return Err(UploadErr::Fail(fail(LeaseFailReason::Other, "PARKED_LOST")));
        };
        self.upload(local, entry, &output, watch)
    }

    /// Wait (lease kept alive by the renewal thread) until the tariff allows the parked result's upload, then
    /// upload it. `Ok(Done::Parked)` means it is still parked: Stop was requested or the lease was lost.
    fn hold_parked(&self, entry: &Entry, local: Local, watch: &LeaseWatch) -> Result<Done, Fail> {
        loop {
            if watch.lost() || self.ctl.stopped() {
                return Ok(Done::Parked);
            }
            self.live.set_activity(Activity::Paused(PARKED_TEXT.into()));
            match self.upload_parked(entry, local, Some(watch)) {
                Ok(done) => return Ok(Done::Uploaded(done)),
                Err(UploadErr::NotNow(_) | UploadErr::Transient) => {}
                Err(UploadErr::Fail(f)) => return Err(f),
            }
            if self.ctl.wait(self.b.tuning.poll) {
                return Ok(Done::Parked);
            }
        }
    }

    /// Budget-gated upload of `output`, then journal it. Never moves a byte the tariff does not allow.
    fn upload(
        &self,
        local: Local,
        entry: &Entry,
        output: &[u8],
        watch: Option<&LeaseWatch>,
    ) -> Result<Entry, UploadErr> {
        let lost = || watch.is_some_and(LeaseWatch::lost);
        let sha: [u8; 32] = Sha256::digest(output).into();
        let sha_hex = hex::encode(sha);
        let size = output.len() as u64;
        // Nothing is sent unless the tariff allows at least the first request now.
        self.b
            .budget
            .check(
                TrafficClass::ResultUpload,
                UPLOAD_OVERHEAD,
                self.b.clock.now(),
                &self.b.probe.network(),
            )
            .map_err(UploadErr::NotNow)?;
        if lost() {
            return Err(UploadErr::Fail(lease_lost_fail()));
        }
        self.live.set_activity(Activity::Uploading);
        let stopped = || self.ctl.stopped() || lost();
        let wait = |d: Duration| self.ctl.wait(d) || lost();
        let up = Uploader {
            coord: self.b.coord.as_ref(),
            gate: Some(UploadGate {
                budget: &self.b.budget,
                probe: self.b.probe.as_ref(),
                clock: self.b.clock.as_ref(),
                class: TrafficClass::ResultUpload,
            }),
            stopped: &stopped,
            wait: &wait,
            chunk: UPLOAD_CHUNK,
            retry_base: Duration::from_millis(200),
        };
        match up.upload(&sha_hex, output) {
            Ok(()) => {}
            Err(_) if lost() => return Err(UploadErr::Fail(lease_lost_fail())),
            Err(UploadError::NotNow(r)) => return Err(UploadErr::NotNow(r)),
            // Verified bytes stay on the server; a later attempt resumes from its offset.
            Err(UploadError::Stopped) => return Err(UploadErr::Transient),
            Err(UploadError::Coord(e))
                if e.is_transient()
                    || matches!(&e, CoordError::Status { status: 409, code } if code == "upload_limit") =>
            {
                return Err(UploadErr::Transient)
            }
            Err(UploadError::Verify) => {
                return Err(UploadErr::Fail(fail(
                    LeaseFailReason::Other,
                    "UPLOAD_VERIFY",
                )))
            }
            Err(UploadError::Coord(_)) => {
                return Err(UploadErr::Fail(fail(LeaseFailReason::Other, "UPLOAD")))
            }
        }
        lock(&self.b.journal)
            .uploaded(local, sha, size, self.now_ms())
            .map_err(|_| UploadErr::Fail(fail(LeaseFailReason::Other, "JOURNAL")))?;
        Ok(Entry {
            lease_id: entry.lease_id.clone(),
            attempt_id: entry.attempt_id.clone(),
            task_id: entry.task_id.clone(),
            output_sha256: Some(sha_hex),
            output_size: Some(size),
        })
    }

    /// Upload a finished output now, or park it when the tariff (or a dropped link) does not allow that.
    fn finish_output(
        &self,
        l: &LeaseResponse,
        local: Local,
        digest: &str,
        output: &[u8],
        policy: &OwnerPolicy,
        watch: &LeaseWatch,
    ) -> Result<Done, Fail> {
        use LeaseFailReason as R;
        let entry = Entry {
            lease_id: l.lease_id.clone(),
            attempt_id: l.attempt_id.clone(),
            task_id: l.task_id.clone(),
            output_sha256: None,
            output_size: None,
        };
        let why = match self.upload(local, &entry, output, Some(watch)) {
            Ok(e) => return Ok(Done::Uploaded(e)),
            Err(UploadErr::Fail(f)) => return Err(f),
            Err(UploadErr::NotNow(Refusal::Budget)) => fail(R::Budget, "UPLOAD_BUDGET"),
            Err(UploadErr::NotNow(Refusal::Window)) => fail(R::TariffWindow, "UPLOAD_WINDOW"),
            Err(UploadErr::Transient) => fail(R::Other, "UPLOAD"),
        };
        let cap = policy.max_disk_mb.saturating_mul(1024 * 1024);
        let parked = lock(&self.b.parked).park(
            &l.task_id,
            digest,
            &l.lease_id,
            &l.attempt_id,
            output,
            cap,
            self.now_ms(),
        );
        match parked {
            Ok(()) => {
                tracing::info!(
                    code = why.code,
                    output_bytes = output.len(),
                    "result parked until the next upload window"
                );
                Ok(Done::Parked)
            }
            Err(e) => {
                tracing::warn!(error = %e, code = why.code, "cannot park the result");
                Err(why)
            }
        }
    }

    /// Send (or resend) Complete v2 for an uploaded result. Idempotent end to end: the coordinator answers a
    /// duplicate with `duplicate: true`, and nc-store's commit is attempt-checked, so it is counted once.
    fn complete(&self, entry: &Entry, local: Local) {
        let (Some(sha), Some(size)) = (entry.output_sha256.clone(), entry.output_size) else {
            let _ = lock(&self.b.journal).failed(local, self.now_ms());
            return;
        };
        let req = CompleteRequest {
            lease_id: entry.lease_id.clone(),
            attempt_id: entry.attempt_id.clone(),
            result: String::new(),
            output_sha256: sha,
            output_size: size,
            version: 2,
        };
        match self.b.coord.complete(&req) {
            Ok(resp) => {
                tracing::info!(state = %resp.state, duplicate = resp.duplicate, "result accepted");
                let t = lock(&self.b.parked)
                    .by_attempt(&entry.attempt_id)
                    .map(|p| p.task_id.clone());
                if let Some(t) = t {
                    lock(&self.b.parked).remove(&t);
                }
                match lock(&self.b.journal).completed(local, self.now_ms()) {
                    Ok(CommitOutcome::Committed) => {
                        lock(&self.live.counters).job_committed(self.b.clock.now());
                    }
                    Ok(CommitOutcome::AlreadyCommitted) => {}
                    Err(e) => tracing::warn!(error = %e, "local commit failed"),
                }
            }
            Err(e) if e.is_transient() => {
                tracing::warn!(error = %e, "complete not delivered; will resend");
            }
            Err(e) => {
                tracing::warn!(error = %e, "coordinator refused the result");
                // A parked copy is kept only if the lease merely ended (a new lease may take it).
                if !e.lease_gone() {
                    let t = lock(&self.b.parked)
                        .by_attempt(&entry.attempt_id)
                        .map(|p| p.task_id.clone());
                    if let Some(t) = t {
                        lock(&self.b.parked).remove(&t);
                    }
                }
                let _ = lock(&self.b.journal).failed(local, self.now_ms());
            }
        }
    }

    pub(crate) fn handle_lease(&self, l: &LeaseResponse) {
        let entry = Entry {
            lease_id: l.lease_id.clone(),
            attempt_id: l.attempt_id.clone(),
            task_id: l.task_id.clone(),
            output_sha256: None,
            output_size: None,
        };
        let local = match lock(&self.b.journal).begin(entry.clone(), self.now_ms()) {
            Ok(local) => local,
            Err(e) => {
                tracing::warn!(error = %e, "cannot record lease; refusing it");
                let _ = self.send_fail(&entry, &fail(LeaseFailReason::Other, "JOURNAL"));
                return;
            }
        };
        let watch = LeaseWatch::default();
        let result = std::thread::scope(|s| {
            let b = self.b;
            let w = &watch;
            s.spawn(move || keep_lease(b, l, w));
            let r = match self.process(l, local, &watch) {
                Ok(Done::Parked) => self.hold_parked(&entry, local, &watch),
                r => r,
            };
            watch.done.store(true, Ordering::SeqCst);
            r
        });
        let parked = lock(&self.b.parked).by_attempt(&entry.attempt_id).is_some();
        if watch.lost() && parked {
            // The result is kept; only this lease ends. A capped lease is still valid for a few seconds, so
            // release it (1.L2: a tariff-window release does not count against the task).
            let why = *lock(&watch.why);
            tracing::info!(
                code = why,
                "lease ended while parked; result kept for a new lease"
            );
            if why == "LEASE_CAPPED" && self.control_allowed() {
                let _ = self.send_fail(
                    &entry,
                    &fail(LeaseFailReason::TariffWindow, "PARKED_RELEASE"),
                );
            }
            let _ = lock(&self.b.journal).failed(local, self.now_ms());
        } else if matches!(result, Ok(Done::Parked)) {
            // Stop while parked: the journal record and the parked result stay; the next start resumes them.
            tracing::info!("stopped with a parked result; it resumes on the next start");
        } else if watch.lost() {
            // The coordinator no longer holds this lease for us: no upload, no Complete, no Fail (it would be
            // refused). Only the local record is closed.
            tracing::warn!(code = *lock(&watch.why), "lease lost; job dropped");
            let _ = lock(&self.b.journal).failed(local, self.now_ms());
        } else {
            match result {
                Ok(Done::Uploaded(entry)) => self.complete(&entry, local),
                Ok(Done::Parked) => {}
                Err(f) => {
                    let _ = self.send_fail(&entry, &f);
                    let _ = lock(&self.b.journal).failed(local, self.now_ms());
                }
            }
        }
        self.live.set_activity(Activity::Idle);
    }

    /// Verify, download, run and upload. Returns the entry with output fields set, ready for Complete.
    fn process(&self, l: &LeaseResponse, local: Local, watch: &LeaseWatch) -> Result<Done, Fail> {
        use LeaseFailReason as R;
        // 1. Signed manifest, before anything big is downloaded.
        let signed = match (&l.manifest, l.version) {
            (Some(m), 2) => m,
            _ => return Err(fail(R::VerifyFailed, "LEASE_VERSION")),
        };
        let job =
            verify_job(signed, self.now_ms()).map_err(|_| fail(R::VerifyFailed, "MANIFEST"))?;
        let task = job
            .task
            .clone()
            .ok_or_else(|| fail(R::VerifyFailed, "MANIFEST"))?;
        let task_uuid =
            uuid::Uuid::from_slice(&task.task_id).map_err(|_| fail(R::VerifyFailed, "TASK_ID"))?;
        if uuid::Uuid::parse_str(&l.task_id).ok() != Some(task_uuid) {
            return Err(fail(R::VerifyFailed, "TASK_ID"));
        }
        check_transfers(&job, &task, &l.transfers)?;

        // 2. Owner policy: publisher trusted, profile allowed, limits within the owner's caps.
        let policy = self.live.policy();
        let publisher: [u8; 32] = task
            .publisher_key
            .as_slice()
            .try_into()
            .map_err(|_| fail(R::VerifyFailed, "MANIFEST"))?;
        let limits = task
            .limits
            .ok_or_else(|| fail(R::VerifyFailed, "MANIFEST"))?;
        let module_ref = &l.transfers[0];
        let disk = module_ref
            .size
            .saturating_add(job.total_input_size)
            .saturating_add(limits.output_bytes);
        let offer = TaskOffer {
            peer_key: PeerKey::from_bytes(publisher),
            profile: task.profile.clone(),
            est_memory_mb: mib_ceil(limits.memory_bytes),
            est_disk_mb: mib_ceil(disk),
            est_duration: Duration::from_millis(limits.wall_ms),
        };
        if let Admission::Reject { reason } =
            admit(&policy, &device_state(self.b, Vec::new()), &offer)
        {
            return Err(fail(R::PolicyRefused, reason_code(&reason)));
        }

        // 2b. A result this device already computed for this exact signed job and kept across a closed window:
        // upload it under this lease instead of downloading and running again.
        let digest = job_digest(signed);
        let reuse = {
            let p = lock(&self.b.parked);
            p.get(&l.task_id)
                .filter(|x| x.job_digest == digest)
                .and_then(|_| p.read(&l.task_id))
        };
        if let Some(output) = reuse {
            lock(&self.b.parked)
                .rebind(&l.task_id, &l.lease_id, &l.attempt_id)
                .map_err(|_| fail(R::Other, "JOURNAL"))?;
            lock(&self.b.journal)
                .start(local, self.now_ms())
                .map_err(|_| fail(R::Other, "JOURNAL"))?;
            tracing::info!(output_bytes = output.len(), "reusing a parked result");
            return self.finish_output(l, local, &digest, &output, &policy, watch);
        }

        // 3. Budget pre-check for everything this job will move, before the first byte.
        if self.b.budget.available_bytes().is_some_and(|a| a < disk) {
            return Err(fail(R::Budget, "PRECHECK"));
        }

        // 4. Downloads.
        self.live.set_activity(Activity::Downloading);
        let stopped = || self.ctl.stopped() || watch.lost();
        let ctx = transfer::Ctx {
            coord: self.b.coord.as_ref(),
            budget: &self.b.budget,
            probe: self.b.probe.as_ref(),
            clock: self.b.clock.as_ref(),
            stopped: &stopped,
            staging: &self.b.staging,
        };
        // Direct from the requester when the lease names it and it answers as that key; else HTTP.
        let lease_peer = crate::peer::LeasePeer::open(
            self.b.peer.as_ref().map(|p| p.net.as_ref()),
            l.peer.as_ref(),
        );
        let direct = lease_peer.as_ref().map(crate::peer::LeasePeer::source);
        let module = crate::peer::fetch(&ctx, direct, module_ref, TrafficClass::ModuleDownload)
            .map_err(|e| transfer_fail(&e))?;
        let mut input = Vec::new();
        for (r, signed_ref) in l.transfers[1..].iter().zip(task.inputs.iter()) {
            let sha: [u8; 32] = signed_ref
                .sha256
                .as_slice()
                .try_into()
                .map_err(|_| fail(R::VerifyFailed, "MANIFEST"))?;
            let cached = lock(&self.b.cache).get(sha, r.size);
            let bytes = match cached {
                Some(b) => b,
                None => {
                    let b = crate::peer::fetch(&ctx, direct, r, TrafficClass::Bulk)
                        .map_err(|e| transfer_fail(&e))?;
                    lock(&self.b.cache)
                        .put(sha, &b)
                        .map_err(|e| transfer_fail(&e))?;
                    b
                }
            };
            input.extend_from_slice(&bytes);
        }
        drop(lease_peer);
        if watch.lost() {
            return Err(lease_lost_fail());
        }
        if self.ctl.stopped() {
            return Err(fail(R::StoppedByOwner, "STOP"));
        }

        // 5. Run.
        lock(&self.b.journal)
            .start(local, self.now_ms())
            .map_err(|_| fail(R::Other, "JOURNAL"))?;
        self.live.set_activity(Activity::Running);
        let publishers: Vec<[u8; 32]> =
            policy.trusted_peers.iter().map(|k| *k.as_bytes()).collect();
        let started = self.b.clock.now();
        let t0 = Instant::now();
        let (result, cause) = self.run_watched(&task, &publishers, module, input, started, watch);
        lock(&self.live.counters).job_ran(self.b.clock.now(), t0.elapsed().as_secs());
        if watch.lost() {
            return Err(lease_lost_fail());
        }
        let outcome = result.map_err(|e| exec_fail(&e))?;
        if let Some(f) = termination_fail(&outcome.termination, cause) {
            return Err(f);
        }
        tracing::info!(
            fuel = outcome.fuel_used,
            output_bytes = outcome.output.len(),
            "job finished"
        );

        // 6. Upload the output and record it before sending Complete; park it if the window is closed.
        self.finish_output(l, local, &digest, &outcome.output, &policy, watch)
    }

    /// Run with a monitor that cancels on Stop or when the owner policy says Stop/Pause (consent off, hours
    /// over, battery, thermal, ...). nc-exec's watchdog sees the cancel within 5 ms.
    fn run_watched(
        &self,
        task: &TaskManifest,
        publishers: &[[u8; 32]],
        module: Vec<u8>,
        input: Vec<u8>,
        started: chrono::DateTime<chrono::Utc>,
        watch: &LeaseWatch,
    ) -> (Result<nc_exec::Outcome, ExecError>, StopCause) {
        let cancel = CancelToken::new();
        *lock(&self.live.cancel) = Some(cancel.clone());
        // A Stop that landed before the token was published is honoured here.
        if self.ctl.stopped() {
            cancel.cancel();
        }
        let done = AtomicBool::new(false);
        let cause = Mutex::new(StopCause::None);
        let running = RunningTask {
            peer_key: PeerKey::from_bytes(
                task.publisher_key
                    .as_slice()
                    .try_into()
                    .unwrap_or([0u8; 32]),
            ),
            profile: task.profile.clone(),
            started_at: started,
        };
        let result = std::thread::scope(|s| {
            s.spawn(|| {
                while !done.load(Ordering::SeqCst) {
                    if self.ctl.stopped() {
                        *lock(&cause) = StopCause::Stop;
                        cancel.cancel();
                        return;
                    }
                    if watch.lost() {
                        *lock(&cause) = StopCause::Lease;
                        cancel.cancel();
                        return;
                    }
                    let policy = self.live.policy();
                    let state = device_state(self.b, vec![running.clone()]);
                    if !matches!(check_running(&policy, &state), RunningDecision::Continue) {
                        *lock(&cause) = StopCause::Policy;
                        cancel.cancel();
                        return;
                    }
                    std::thread::sleep(self.b.tuning.monitor_tick);
                }
            });
            let r = self.b.runner.run(task, publishers, module, input, &cancel);
            done.store(true, Ordering::SeqCst);
            r
        });
        *lock(&self.live.cancel) = None;
        let cause = *lock(&cause);
        let cause = if cause == StopCause::None && self.ctl.stopped() {
            StopCause::Stop
        } else {
            cause
        };
        (result, cause)
    }
}
