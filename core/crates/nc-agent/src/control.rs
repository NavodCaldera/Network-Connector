//! Local control channel between the `nc` CLI and the `nc-agent` daemon.
//!
//! One JSON request per TCP connection on **127.0.0.1 only** (AGENT.md §1 rule 7), authenticated by a random
//! token the daemon writes into its private data folder. Requests are capped at 64 KiB and time out after 2 s.

use crate::session::Activity;
use crate::{Agent, SessionState};
use nc_policy::OwnerPolicy;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

const MAX_REQUEST: u64 = 64 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(2);

/// A command.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    /// Session state, activity, today's counters.
    Status,
    /// Start the helper session with the saved policy.
    Start,
    /// Stop the helper session (within 1 s).
    Stop,
    /// Show the policy in force.
    PolicyShow,
    /// Validate, save and apply a policy.
    PolicySet {
        /// The new policy.
        policy: OwnerPolicy,
    },
    /// Stop the session and exit the daemon.
    Shutdown,
}

#[derive(Serialize, Deserialize)]
struct Request {
    token: String,
    #[serde(flatten)]
    cmd: Command,
}

/// A reply.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    /// Success.
    pub ok: bool,
    /// Plain-English error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Session state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// Activity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<String>,
    /// Jobs done today.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jobs_done_today: Option<u64>,
    /// Seconds helped today.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub helping_seconds_today: Option<u64>,
    /// Policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<OwnerPolicy>,
}

fn err(msg: impl Into<String>) -> Reply {
    Reply {
        ok: false,
        error: Some(msg.into()),
        ..Reply::default()
    }
}

/// Fresh random control token.
pub fn new_token() -> Result<String, getrandom::Error> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b)?;
    Ok(hex::encode(b))
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Bind the loopback control socket.
pub fn bind(port: u16) -> std::io::Result<TcpListener> {
    TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
}

/// What the daemon does for policy changes and shutdown.
pub trait Handler {
    /// Persist a validated policy.
    fn save_policy(&self, policy: &OwnerPolicy) -> Result<(), String>;
    /// The saved policy (used by Start).
    fn load_policy(&self) -> Result<OwnerPolicy, String>;
}

fn state_name(s: SessionState) -> &'static str {
    match s {
        SessionState::Stopped => "stopped",
        SessionState::Starting => "starting",
        SessionState::WaitingForJobs => "waiting_for_jobs",
        SessionState::NotAccepting => "not_accepting",
        SessionState::Stopping => "stopping",
    }
}

fn activity_text(a: &Activity) -> String {
    match a {
        Activity::Off => "off".into(),
        Activity::Idle => "idle".into(),
        Activity::Paused(why) => format!("paused: {why}"),
        Activity::Offline => "offline (retrying)".into(),
        Activity::Downloading => "downloading".into(),
        Activity::Running => "running a job".into(),
        Activity::Uploading => "uploading".into(),
    }
}

/// Handle one command. Returns the reply and whether the daemon should exit.
pub fn execute(agent: &Agent, h: &dyn Handler, cmd: Command) -> (Reply, bool) {
    match cmd {
        Command::Status => {
            let t = agent.today();
            (
                Reply {
                    ok: true,
                    state: Some(state_name(agent.status().state).into()),
                    activity: Some(activity_text(&agent.activity())),
                    jobs_done_today: Some(t.jobs_done),
                    helping_seconds_today: Some(t.helping_seconds),
                    ..Reply::default()
                },
                false,
            )
        }
        Command::Start => match h.load_policy() {
            Ok(p) => match agent.start_helper_session(p) {
                Ok(()) => (
                    Reply {
                        ok: true,
                        ..Reply::default()
                    },
                    false,
                ),
                Err(e) => (err(e.to_string()), false),
            },
            Err(e) => (err(e), false),
        },
        Command::Stop => {
            agent.stop_helper_session();
            (
                Reply {
                    ok: true,
                    state: Some("stopped".into()),
                    ..Reply::default()
                },
                false,
            )
        }
        Command::PolicyShow => (
            Reply {
                ok: true,
                policy: Some(agent.policy()),
                ..Reply::default()
            },
            false,
        ),
        Command::PolicySet { policy } => {
            if let Err(e) = policy.validate() {
                return (err(e.to_string()), false);
            }
            if let Err(e) = h.save_policy(&policy) {
                return (err(e), false);
            }
            match agent.update_policy(policy) {
                Ok(()) => (
                    Reply {
                        ok: true,
                        ..Reply::default()
                    },
                    false,
                ),
                Err(e) => (err(e.to_string()), false),
            }
        }
        Command::Shutdown => {
            agent.stop_helper_session();
            (
                Reply {
                    ok: true,
                    ..Reply::default()
                },
                true,
            )
        }
    }
}

fn serve_one(mut s: TcpStream, token: &str, agent: &Agent, h: &dyn Handler) -> bool {
    let _ = s.set_read_timeout(Some(IO_TIMEOUT));
    let _ = s.set_write_timeout(Some(IO_TIMEOUT));
    let mut buf = Vec::new();
    if (&mut s)
        .take(MAX_REQUEST + 1)
        .read_to_end(&mut buf)
        .is_err()
        || buf.len() as u64 > MAX_REQUEST
    {
        let _ = s.write_all(&serde_json::to_vec(&err("bad request")).unwrap_or_default());
        return false;
    }
    let (reply, exit) = match serde_json::from_slice::<Request>(&buf) {
        Ok(r) if ct_eq(r.token.as_bytes(), token.as_bytes()) => execute(agent, h, r.cmd),
        Ok(_) => (err("not authorised"), false),
        Err(_) => (err("bad request"), false),
    };
    let _ = s.write_all(&serde_json::to_vec(&reply).unwrap_or_default());
    exit
}

/// Serve until a Shutdown command.
pub fn serve(listener: &TcpListener, token: &str, agent: &Agent, h: &dyn Handler) {
    for conn in listener.incoming() {
        let Ok(s) = conn else { continue };
        // Defence in depth: the socket is bound to loopback, but refuse anything else anyway.
        if s.peer_addr().map(|a| !a.ip().is_loopback()).unwrap_or(true) {
            continue;
        }
        if serve_one(s, token, agent, h) {
            return;
        }
    }
}

/// Client: send one command to the daemon on `port`.
pub fn call(port: u16, token: &str, cmd: Command) -> std::io::Result<Reply> {
    let mut s =
        TcpStream::connect_timeout(&SocketAddr::from((Ipv4Addr::LOCALHOST, port)), IO_TIMEOUT)?;
    // Stop may take up to 1 s; leave room.
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    let req = Request {
        token: token.into(),
        cmd,
    };
    s.write_all(&serde_json::to_vec(&req).map_err(std::io::Error::other)?)?;
    s.shutdown(std::net::Shutdown::Write)?;
    let mut out = Vec::new();
    s.take(MAX_REQUEST).read_to_end(&mut out)?;
    serde_json::from_slice(&out).map_err(std::io::Error::other)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct H(Mutex<Option<OwnerPolicy>>);
    impl Handler for H {
        fn save_policy(&self, p: &OwnerPolicy) -> Result<(), String> {
            *self.0.lock().unwrap() = Some(p.clone());
            Ok(())
        }
        fn load_policy(&self) -> Result<OwnerPolicy, String> {
            Ok(self.0.lock().unwrap().clone().unwrap_or_default())
        }
    }

    #[test]
    fn token_required_and_commands_work() {
        let agent = Agent::new();
        let h = H(Mutex::new(None));
        let l = bind(0).unwrap();
        let port = l.local_addr().unwrap().port();
        assert!(l.local_addr().unwrap().ip().is_loopback());
        std::thread::scope(|s| {
            s.spawn(|| serve(&l, "secret-token", &agent, &h));
            let r = call(port, "wrong", Command::Status).unwrap();
            assert!(!r.ok);
            let r = call(port, "secret-token", Command::Status).unwrap();
            assert_eq!(r.state.as_deref(), Some("stopped"));
            let bad = OwnerPolicy {
                version: 0,
                ..OwnerPolicy::default()
            };
            assert!(
                !call(port, "secret-token", Command::PolicySet { policy: bad })
                    .unwrap()
                    .ok
            );
            let good = OwnerPolicy {
                accept_jobs: true,
                ..OwnerPolicy::default()
            };
            assert!(
                call(
                    port,
                    "secret-token",
                    Command::PolicySet {
                        policy: good.clone()
                    }
                )
                .unwrap()
                .ok
            );
            let r = call(port, "secret-token", Command::PolicyShow).unwrap();
            assert_eq!(r.policy, Some(good));
            assert!(call(port, "secret-token", Command::Start).unwrap().ok);
            let r = call(port, "secret-token", Command::Stop).unwrap();
            assert!(r.ok);
            assert!(call(port, "secret-token", Command::Shutdown).unwrap().ok);
        });
    }
}
