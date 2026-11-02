//! `nc-agent`: the headless helper daemon.
//!
//! ```text
//! nc-agent run --config <agent.json>
//! nc-agent service install --config <agent.json>     (Windows, elevated)
//! nc-agent service uninstall                        (Windows, elevated; keeps the data dir)
//! nc-agent service start | stop                       (Windows, elevated)
//! ```
//! Loads the config and the owner policy, starts the helper session and serves the loopback control socket
//! that `nc status|start|stop|policy` talk to. The enrolment secret comes from `NC_ENROLL_SECRET` (console) or
//! the ACL-protected `enroll.secret` file in the data dir (service).

#[cfg(windows)]
mod service;

use anyhow::{bail, Context, Result};
use nc_agent::config::AgentConfig;
use nc_agent::control;
use nc_policy::OwnerPolicy;
use std::path::{Path, PathBuf};

struct Daemon {
    cfg: AgentConfig,
}

impl control::Handler for Daemon {
    fn save_policy(&self, p: &OwnerPolicy) -> Result<(), String> {
        self.cfg.save_policy(p).map_err(|e| e.to_string())
    }
    fn load_policy(&self) -> Result<OwnerPolicy, String> {
        self.cfg.load_policy().map_err(|e| e.to_string())
    }
}

/// Parsed command line.
#[derive(Debug, PartialEq, Eq)]
enum Cmd {
    Run(PathBuf),
    ServiceRun(PathBuf),
    Install(PathBuf),
    Uninstall,
    Start,
    Stop,
}

const USAGE: &str = "usage: nc-agent run --config <agent.json>\n       \
nc-agent service install --config <agent.json> | uninstall | start | stop";

fn parse_args(args: &[String]) -> Result<Cmd> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    Ok(match a.as_slice() {
        ["run", "--config", p] => Cmd::Run(PathBuf::from(p)),
        ["service", "run", "--config", p] => Cmd::ServiceRun(PathBuf::from(p)),
        ["service", "install", "--config", p] => Cmd::Install(PathBuf::from(p)),
        ["service", "uninstall"] => Cmd::Uninstall,
        ["service", "start"] => Cmd::Start,
        ["service", "stop"] => Cmd::Stop,
        _ => bail!(USAGE),
    })
}

fn init_console_logs() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
}

/// Run the daemon until a Shutdown command. `on_ready(port, token)` is called once the control socket is up
/// (the service host uses it to turn an SCM Stop into an authenticated loopback Shutdown).
fn run_daemon(cfg: AgentConfig, on_ready: impl FnOnce(u16, &str)) -> Result<()> {
    let policy = cfg.load_policy().context("loading policy")?;
    let listener = control::bind(cfg.control_port).context("binding the loopback control port")?;
    let token = control::new_token().context("no system randomness")?;
    std::fs::create_dir_all(&cfg.data_dir).context("creating data_dir")?;
    std::fs::write(cfg.token_path(), &token).context("writing control token")?;
    let agent = cfg.build_agent().context("starting the agent")?;
    if let Err(e) = agent.start_helper_session(policy) {
        tracing::warn!(error = %e, "helper session not started");
    }
    tracing::info!(port = cfg.control_port, "nc-agent running");
    on_ready(cfg.control_port, &token);
    let daemon = Daemon { cfg };
    control::serve(&listener, &token, &agent, &daemon);
    agent.stop_helper_session();
    let _ = std::fs::remove_file(daemon.cfg.token_path());
    tracing::info!("nc-agent stopped");
    Ok(())
}

fn load(path: &Path) -> Result<AgentConfig> {
    AgentConfig::load(path).context("loading config")
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse_args(&args)? {
        Cmd::Run(p) => {
            init_console_logs();
            run_daemon(load(&p)?, |_, _| {})
        }
        #[cfg(windows)]
        Cmd::ServiceRun(p) => service::dispatch(p),
        #[cfg(windows)]
        Cmd::Install(p) => service::install(&p, &load(&p)?),
        #[cfg(windows)]
        Cmd::Uninstall => service::uninstall(),
        #[cfg(windows)]
        Cmd::Start => service::start(),
        #[cfg(windows)]
        Cmd::Stop => service::stop(),
        #[cfg(not(windows))]
        _ => bail!("Windows service commands are only available on Windows"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn args_parse() {
        assert_eq!(
            parse_args(&v(&["run", "--config", "x.json"])).ok(),
            Some(Cmd::Run(PathBuf::from("x.json")))
        );
        assert_eq!(
            parse_args(&v(&["service", "install", "--config", "x.json"])).ok(),
            Some(Cmd::Install(PathBuf::from("x.json")))
        );
        assert_eq!(
            parse_args(&v(&["service", "uninstall"])).ok(),
            Some(Cmd::Uninstall)
        );
        assert_eq!(parse_args(&v(&["service", "stop"])).ok(), Some(Cmd::Stop));
        assert!(parse_args(&[]).is_err());
        assert!(parse_args(&v(&["service", "install"])).is_err());
        assert!(parse_args(&v(&["service", "uninstall", "--purge"])).is_err());
    }
}
