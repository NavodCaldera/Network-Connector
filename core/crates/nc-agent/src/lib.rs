//! `nc-agent`: the device supervisor (AGENT.md §3 shared contract).
//!
//! [`Agent`] is the typed API that `nc-bridge` wraps 1:1 and the `nc-agent` daemon drives:
//! [`Agent::start_helper_session`], [`Agent::stop_helper_session`], [`Agent::watch_status`],
//! [`Agent::update_policy`], [`Agent::update_network_budget`], [`Agent::submit_job`], [`Agent::today`] and
//! [`Agent::activity`].
//!
//! An agent made with [`Agent::new`] has no coordinator: its session only proves the start/stop/status path (the
//! app uses this until a coordinator is configured). An agent made with [`Agent::with_backend`] runs the real
//! helper loop in [`session`]: register, policy and tariff gates, lease, signed-manifest verification,
//! tariff-gated Range-resumable downloads, sandboxed execution in nc-exec, upload and Complete v2, with every
//! attempt journalled in nc-store for crash recovery.
//!
//! Stop (AGENT.md §1 rule 5): the running job's cancel token is fired at once (nc-exec kills the sandbox within
//! one 5 ms watchdog tick), and `stop_helper_session` returns within 1 s. If the session thread is still inside
//! a network call it is left to finish on its own; it never starts a job after Stop.

pub mod budget;
pub mod config;
pub mod control;
pub mod coord;
pub mod journal;
pub mod probe;
pub mod requester;
pub mod runner;
pub mod session;
pub mod today;
pub mod transfer;

pub use budget::NetworkBudget;
pub use session::{Activity, Backend, Tuning};
pub use today::Today;

use nc_policy::OwnerPolicy;
use session::{lock, Ctl, Live, Session};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// How often the placeholder session loop wakes when nothing happens. Stop never waits for this tick.
const IDLE_TICK: Duration = Duration::from_millis(250);
/// Longest `stop_helper_session` waits for the session thread before leaving it to finish by itself.
const STOP_WAIT: Duration = Duration::from_millis(800);

/// Where the helper session is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    /// No session. The device does no work for anyone.
    Stopped,
    /// Session thread is starting.
    Starting,
    /// Session is running and the owner has allowed jobs.
    WaitingForJobs,
    /// Session is running but the owner has not allowed jobs ("Accept jobs" is off).
    NotAccepting,
    /// Stop requested; tearing down.
    Stopping,
}

/// Status snapshot sent to watchers. Low frequency (state changes only); never contains job data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    /// Session state.
    pub state: SessionState,
    /// Monotonic change counter, starting at 0 for the initial `Stopped`.
    pub seq: u64,
}

/// Errors from the agent API.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AgentError {
    /// The owner policy is invalid; the session is not started.
    #[error("{0}")]
    InvalidPolicy(#[from] nc_policy::ValidationError),
    /// A session is already running.
    #[error("A helper session is already running.")]
    AlreadyRunning,
    /// The session thread could not be started.
    #[error("Could not start the helper session.")]
    Spawn,
    /// The network budget is invalid.
    #[error("The network budget settings are invalid.")]
    InvalidBudget,
    /// This agent has no coordinator configured.
    #[error("No coordinator is configured.")]
    NoCoordinator,
    /// Submitting a job failed.
    #[error("Could not submit the job: {0}")]
    Submit(String),
}

struct Running {
    ctl: Arc<Ctl>,
    thread: JoinHandle<()>,
    done: Receiver<()>,
}

struct Shared {
    status: Status,
    watchers: Vec<Sender<Status>>,
}

impl Shared {
    fn set(&mut self, state: SessionState) {
        if self.status.state == state {
            return;
        }
        self.status = Status {
            state,
            seq: self.status.seq + 1,
        };
        let status = self.status.clone();
        self.watchers.retain(|w| w.send(status.clone()).is_ok());
    }
}

/// The device agent. Cheap to share behind an `Arc`; all methods take `&self`.
pub struct Agent {
    shared: Arc<Mutex<Shared>>,
    running: Mutex<Option<Running>>,
    live: Arc<Live>,
    backend: Option<Arc<Backend>>,
}

impl Default for Agent {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Agent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agent")
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

impl Agent {
    /// New agent with no session (`Stopped`) and no coordinator.
    pub fn new() -> Self {
        Self::build(None)
    }

    /// New agent that runs the real helper loop against `backend`.
    pub fn with_backend(backend: Backend) -> Self {
        Self::build(Some(Arc::new(backend)))
    }

    fn build(backend: Option<Arc<Backend>>) -> Self {
        Self {
            shared: Arc::new(Mutex::new(Shared {
                status: Status {
                    state: SessionState::Stopped,
                    seq: 0,
                },
                watchers: Vec::new(),
            })),
            running: Mutex::new(None),
            live: Arc::new(Live::new()),
            backend,
        }
    }

    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        self.backend
            .as_ref()
            .map_or_else(chrono::Utc::now, |b| b.clock.now())
    }

    /// Current status.
    pub fn status(&self) -> Status {
        lock(&self.shared).status.clone()
    }

    /// What the helper is doing right now.
    pub fn activity(&self) -> Activity {
        lock(&self.live.activity).clone()
    }

    /// Today's counters (jobs done, time helped).
    pub fn today(&self) -> Today {
        lock(&self.live.counters).snapshot(self.now())
    }

    /// The policy in force.
    pub fn policy(&self) -> OwnerPolicy {
        self.live.policy()
    }

    /// Start offering this device as a helper, under `policy`. The policy is validated first.
    pub fn start_helper_session(&self, policy: OwnerPolicy) -> Result<(), AgentError> {
        policy.validate()?;
        let mut running = lock(&self.running);
        if running.is_some() {
            return Err(AgentError::AlreadyRunning);
        }
        lock(&self.shared).set(SessionState::Starting);
        let tz = policy.timezone;
        *lock(&self.live.policy) = policy;
        let ctl = Arc::new(Ctl::default());
        let (done_tx, done) = mpsc::channel::<()>();
        let thread = {
            let ctl = Arc::clone(&ctl);
            let shared = Arc::clone(&self.shared);
            let live = Arc::clone(&self.live);
            let backend = self.backend.clone();
            std::thread::Builder::new()
                .name("nc-helper-session".into())
                .spawn(move || {
                    // Dropped when this thread ends (normally or by panic): tells Stop we are done.
                    let _done = done_tx;
                    session_main(&ctl, &shared, &live, backend.as_deref());
                    live.set_activity(Activity::Off);
                })
        };
        match thread {
            Ok(thread) => {
                lock(&self.live.counters).session_started(tz, self.now());
                *running = Some(Running { ctl, thread, done });
                Ok(())
            }
            Err(_) => {
                lock(&self.shared).set(SessionState::Stopped);
                Err(AgentError::Spawn)
            }
        }
    }

    /// Stop the helper session: cancels any running job at once and returns within 1 s with status `Stopped`.
    /// Safe to call when nothing is running.
    pub fn stop_helper_session(&self) {
        let taken = lock(&self.running).take();
        if let Some(Running { ctl, thread, done }) = taken {
            lock(&ctl.m).stop = true;
            ctl.cv.notify_all();
            self.live.cancel_job();
            lock(&self.shared).set(SessionState::Stopping);
            match done.recv_timeout(STOP_WAIT) {
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // Stuck in a network call; the job (if any) is already cancelled. Let it finish alone.
                    tracing::warn!(
                        "session thread still in a network call after Stop; detaching it"
                    );
                }
                _ => {
                    let _ = thread.join();
                }
            }
            lock(&self.live.counters).session_stopped(self.now());
        }
        lock(&self.shared).set(SessionState::Stopped);
    }

    /// Subscribe to status changes. The current status is sent first.
    pub fn watch_status(&self) -> Receiver<Status> {
        let (tx, rx) = mpsc::channel();
        let mut shared = lock(&self.shared);
        if tx.send(shared.status.clone()).is_ok() {
            shared.watchers.push(tx);
        }
        rx
    }

    /// Apply a new owner policy to the running session (and future ones). A running job that the new policy
    /// no longer allows is cancelled within one monitor tick and reported as STOPPED_BY_OWNER.
    pub fn update_policy(&self, policy: OwnerPolicy) -> Result<(), AgentError> {
        policy.validate()?;
        *lock(&self.live.policy) = policy;
        if let Some(r) = lock(&self.running).as_ref() {
            r.ctl.wake();
        }
        Ok(())
    }

    /// Replace the network budget (tariff profile or unmetered).
    pub fn update_network_budget(&self, budget: NetworkBudget) -> Result<(), AgentError> {
        if let NetworkBudget::Metered(p) = &budget {
            p.validate().map_err(|_| AgentError::InvalidBudget)?;
        }
        if let Some(b) = &self.backend {
            b.budget
                .update(&budget, b.clock.now())
                .map_err(|_| AgentError::InvalidBudget)?;
        }
        Ok(())
    }

    /// Requester side: upload the blobs (hash hex, bytes) and submit a signed job. Returns the job id.
    pub fn submit_job(
        &self,
        manifest: nc_protocol::pb::v1::SignedJobManifest,
        blobs: Vec<(String, Vec<u8>)>,
    ) -> Result<String, AgentError> {
        let b = self.backend.as_ref().ok_or(AgentError::NoCoordinator)?;
        let job = requester::BuiltJob {
            signed: manifest,
            job_id: String::new(),
            blobs,
        };
        requester::submit(b.coord.as_ref(), &job).map_err(|e| AgentError::Submit(e.to_string()))
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        self.stop_helper_session();
    }
}

/// Publish `state` unless Stop was requested (checked under the control lock, so a Stop is never followed by
/// a "running" status).
fn publish(ctl: &Ctl, shared: &Mutex<Shared>, state: SessionState) {
    let g = lock(&ctl.m);
    if !g.stop {
        lock(shared).set(state);
    }
    drop(g);
}

fn session_main(ctl: &Ctl, shared: &Mutex<Shared>, live: &Live, backend: Option<&Backend>) {
    let consent_state = |accept: bool| {
        if accept {
            SessionState::WaitingForJobs
        } else {
            SessionState::NotAccepting
        }
    };
    publish(ctl, shared, consent_state(live.policy().accept_jobs));
    let Some(b) = backend else {
        // No coordinator: idle until Stop.
        let mut c = lock(&ctl.m);
        while !c.stop {
            c = ctl
                .cv
                .wait_timeout(c, IDLE_TICK)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        return;
    };
    transfer::sweep_staging(&b.staging);
    let on_consent = |accept: bool| publish(ctl, shared, consent_state(accept));
    Session {
        b,
        live,
        ctl,
        on_consent: &on_consent,
    }
    .run();
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn wait_for(rx: &Receiver<Status>, want: SessionState) -> Status {
        loop {
            let s = rx.recv_timeout(Duration::from_secs(5)).expect("status");
            if s.state == want {
                return s;
            }
        }
    }

    #[test]
    fn starts_stopped() {
        assert_eq!(Agent::new().status().state, SessionState::Stopped);
    }

    #[test]
    fn start_then_stop_reports_states() {
        let agent = Agent::new();
        let rx = agent.watch_status();
        assert_eq!(rx.recv().unwrap().state, SessionState::Stopped);
        let policy = OwnerPolicy {
            accept_jobs: true,
            ..OwnerPolicy::default()
        };
        agent.start_helper_session(policy).unwrap();
        assert_eq!(rx.recv().unwrap().state, SessionState::Starting);
        assert_eq!(rx.recv().unwrap().state, SessionState::WaitingForJobs);
        agent.stop_helper_session();
        assert_eq!(rx.recv().unwrap().state, SessionState::Stopping);
        assert_eq!(rx.recv().unwrap().state, SessionState::Stopped);
    }

    #[test]
    fn default_policy_does_not_accept_jobs() {
        let agent = Agent::new();
        let rx = agent.watch_status();
        agent.start_helper_session(OwnerPolicy::default()).unwrap();
        wait_for(&rx, SessionState::NotAccepting);
        agent.stop_helper_session();
    }

    #[test]
    fn second_start_is_refused() {
        let agent = Agent::new();
        agent.start_helper_session(OwnerPolicy::default()).unwrap();
        assert_eq!(
            agent.start_helper_session(OwnerPolicy::default()),
            Err(AgentError::AlreadyRunning)
        );
        agent.stop_helper_session();
    }

    #[test]
    fn invalid_policy_is_refused() {
        let agent = Agent::new();
        let bad = OwnerPolicy {
            cpu_budget_percent: 0,
            ..OwnerPolicy::default()
        };
        assert!(matches!(
            agent.start_helper_session(bad),
            Err(AgentError::InvalidPolicy(_))
        ));
        assert_eq!(agent.status().state, SessionState::Stopped);
    }

    #[test]
    fn stop_when_idle_is_harmless() {
        let agent = Agent::new();
        agent.stop_helper_session();
        assert_eq!(agent.status().state, SessionState::Stopped);
    }

    /// AGENT.md §1 rule 5: Stop within 1 s, measured from the stop call to the `Stopped` status.
    #[test]
    fn stop_latency_under_one_second() {
        let agent = Agent::new();
        let rx = agent.watch_status();
        let mut worst = Duration::ZERO;
        for _ in 0..20 {
            agent.start_helper_session(OwnerPolicy::default()).unwrap();
            wait_for(&rx, SessionState::NotAccepting);
            let t = Instant::now();
            agent.stop_helper_session();
            wait_for(&rx, SessionState::Stopped);
            worst = worst.max(t.elapsed());
        }
        eprintln!("stop latency worst of 20: {worst:?}");
        assert!(worst < Duration::from_secs(1), "{worst:?}");
        // Must also beat the idle tick, proving Stop does not wait for the loop to wake by itself.
        assert!(worst < IDLE_TICK, "{worst:?}");
    }
}
