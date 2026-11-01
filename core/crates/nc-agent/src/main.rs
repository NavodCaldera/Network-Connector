//! `nc-agent`: the headless helper daemon.
//!
//! ```text
//! nc-agent run --config <agent.json>
//! ```
//! Loads the config and the owner policy, starts the helper session and serves the loopback control socket
//! that `nc status|start|stop|policy` talk to. The enrolment secret comes from `NC_ENROLL_SECRET`.

use anyhow::{bail, Context, Result};
use nc_agent::config::AgentConfig;
use nc_agent::control;
use nc_policy::OwnerPolicy;
use std::path::PathBuf;

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

fn parse_args(args: &[String]) -> Result<PathBuf> {
    match args {
        [cmd, flag, path] if cmd == "run" && flag == "--config" => Ok(PathBuf::from(path)),
        _ => bail!("usage: nc-agent run --config <agent.json>"),
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = parse_args(&args)?;
    let cfg = AgentConfig::load(&path).context("loading config")?;
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
    let daemon = Daemon { cfg };
    control::serve(&listener, &token, &agent, &daemon);
    agent.stop_helper_session();
    let _ = std::fs::remove_file(daemon.cfg.token_path());
    tracing::info!("nc-agent stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_parse() {
        let a: Vec<String> = ["run", "--config", "x.json"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(parse_args(&a).ok(), Some(PathBuf::from("x.json")));
        assert!(parse_args(&[]).is_err());
    }
}
