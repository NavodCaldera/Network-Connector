//! `nc-agent`: in-process supervisor API (AGENT.md §3 shared contract).
//!
//! Phase 0 spike (task 0.8): only the helper-session part of the contract exists yet:
//! [`Agent::start_helper_session`], [`Agent::stop_helper_session`] and [`Agent::watch_status`].
//! The session loop is a placeholder that runs no jobs; it only proves the start/stop/status path that the UI
//! needs, including the "Stop within 1 s" rule (AGENT.md §1 rule 5).

use nc_policy::OwnerPolicy;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

/// How often the placeholder session loop wakes when nothing happens. Stop never waits for this tick.
const IDLE_TICK: Duration = Duration::from_millis(250);

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
}

#[derive(Default)]
struct Control {
    stop: bool,
}

struct Running {
    control: Arc<(Mutex<Control>, Condvar)>,
    thread: JoinHandle<()>,
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
}

impl Default for Agent {
    fn default() -> Self {
        Self::new()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A poisoned lock only means a watcher panicked; the state itself stays consistent.
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Agent {
    /// New agent with no session (`Stopped`).
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Mutex::new(Shared {
                status: Status {
                    state: SessionState::Stopped,
                    seq: 0,
                },
                watchers: Vec::new(),
            })),
            running: Mutex::new(None),
        }
    }

    /// Current status.
    pub fn status(&self) -> Status {
        lock(&self.shared).status.clone()
    }

    /// Start offering this device as a helper, under `policy`. The policy is validated first.
    pub fn start_helper_session(&self, policy: OwnerPolicy) -> Result<(), AgentError> {
        policy.validate()?;
        let mut running = lock(&self.running);
        if running.is_some() {
            return Err(AgentError::AlreadyRunning);
        }
        lock(&self.shared).set(SessionState::Starting);
        let control = Arc::new((Mutex::new(Control::default()), Condvar::new()));
        let thread = {
            let control = Arc::clone(&control);
            let shared = Arc::clone(&self.shared);
            std::thread::Builder::new()
                .name("nc-helper-session".into())
                .spawn(move || session_loop(&policy, &control, &shared))
        };
        match thread {
            Ok(thread) => {
                *running = Some(Running { control, thread });
                Ok(())
            }
            Err(_) => {
                lock(&self.shared).set(SessionState::Stopped);
                Err(AgentError::Spawn)
            }
        }
    }

    /// Stop the helper session. Returns once the session is torn down and the status is `Stopped`.
    /// Safe to call when nothing is running.
    pub fn stop_helper_session(&self) {
        let taken = lock(&self.running).take();
        if let Some(Running { control, thread }) = taken {
            let (m, cv) = &*control;
            lock(m).stop = true;
            cv.notify_all();
            lock(&self.shared).set(SessionState::Stopping);
            // The loop checks the flag under the same lock it waits on, so this join is immediate.
            let _ = thread.join();
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
}

impl Drop for Agent {
    fn drop(&mut self) {
        self.stop_helper_session();
    }
}

fn session_loop(policy: &OwnerPolicy, control: &(Mutex<Control>, Condvar), shared: &Mutex<Shared>) {
    let state = if policy.accept_jobs {
        SessionState::WaitingForJobs
    } else {
        SessionState::NotAccepting
    };
    let (m, cv) = control;
    let mut c = lock(m);
    // Publish under the control lock so a concurrent Stop can never be followed by a "running" status.
    if c.stop {
        return;
    }
    lock(shared).set(state);
    while !c.stop {
        // Placeholder: real sessions will poll leases and run nc-exec jobs here.
        c = cv
            .wait_timeout(c, IDLE_TICK)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
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
