//! `nc`: command line for the Network Connector agent.
//!
//! Helper side (talks to the local `nc-agent` daemon over its loopback control socket):
//! `nc status`, `nc start`, `nc stop`, `nc policy show`, `nc policy set <policy.json>`, `nc shutdown`.
//!
//! Requester side (talks to the coordinator directly, signing as this device):
//! `nc submit <module.wasm> <input> [--out <file>]` signs the job manifest (this device is the publisher),
//! uploads the module and input, submits, polls `GET /v1/jobs/{id}` and downloads the verified output.

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use nc_agent::config::AgentConfig;
use nc_agent::control::{self, Command};
use nc_agent::coord::Coordinator;
use nc_agent::requester::{self, JobSpec};
use nc_policy::OwnerPolicy;
use nc_protocol::pb::v1::RegisterRequest;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(name = "nc", version, about = "Network Connector command line")]
struct Cli {
    /// Agent config file (default: env NC_AGENT_CONFIG).
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Show the helper state and today's counters.
    Status,
    /// Start the helper session with the saved policy.
    Start,
    /// Stop the helper session (every job halts within 1 s).
    Stop,
    /// Stop the session and exit the daemon.
    Shutdown,
    /// Show or change the owner policy.
    Policy {
        #[command(subcommand)]
        action: PolicyCmd,
    },
    /// Submit a Wasm job and wait for its output.
    Submit {
        /// Wasm module (profile cpu-core-v1).
        module: PathBuf,
        /// Input file.
        input: PathBuf,
        /// Write the output here instead of stdout.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Seconds to wait for the result.
        #[arg(long, default_value_t = 600)]
        timeout: u64,
        /// Fuel limit.
        #[arg(long)]
        fuel: Option<u64>,
        /// Wall-clock limit, ms.
        #[arg(long)]
        wall_ms: Option<u64>,
        /// Linear memory limit, bytes.
        #[arg(long)]
        memory: Option<u64>,
        /// Output limit, bytes.
        #[arg(long)]
        output_limit: Option<u64>,
    },
}

#[derive(Subcommand, Debug)]
enum PolicyCmd {
    /// Print the policy in force.
    Show,
    /// Validate, save and apply a policy file (JSON, nc-policy format).
    Set {
        /// Policy file.
        file: PathBuf,
    },
}

fn config(path: Option<PathBuf>) -> Result<AgentConfig> {
    let p = path
        .or_else(|| std::env::var_os("NC_AGENT_CONFIG").map(PathBuf::from))
        .context("pass --config or set NC_AGENT_CONFIG")?;
    AgentConfig::load(&p).context("loading config")
}

fn daemon(cfg: &AgentConfig, cmd: Command) -> Result<control::Reply> {
    let token = std::fs::read_to_string(cfg.token_path())
        .context("nc-agent does not seem to be running (no control token)")?;
    let r = control::call(cfg.control_port, token.trim(), cmd)
        .context("cannot reach nc-agent on the loopback control port")?;
    if !r.ok {
        bail!("{}", r.error.clone().unwrap_or_else(|| "failed".into()));
    }
    Ok(r)
}

fn submit(cfg: &AgentConfig, args: SubmitArgs) -> Result<()> {
    let module = std::fs::read(&args.module).context("reading module")?;
    let input = std::fs::read(&args.input).context("reading input")?;
    let mut spec = JobSpec::new(module, input);
    if let Some(f) = args.fuel {
        spec.limits.fuel = f;
    }
    if let Some(w) = args.wall_ms {
        spec.limits.wall_ms = w;
    }
    if let Some(m) = args.memory {
        spec.limits.memory_bytes = m;
    }
    if let Some(o) = args.output_limit {
        spec.limits.output_bytes = o;
    }
    let key = cfg.device_key()?;
    let signer = nc_identity::RootSigner(key.as_ref());
    let job = requester::build(&spec, &signer, chrono::Utc::now().timestamp_millis())
        .context("building the job")?;
    let coord = cfg.coordinator(key.clone());
    coord
        .register(&RegisterRequest {
            name: cfg.device_name.clone(),
            platform: std::env::consts::OS.into(),
        })
        .context("registering with the coordinator")?;
    let id = requester::submit(&coord, &job).context("submitting")?;
    eprintln!("submitted job {id}; waiting");
    let status = requester::wait(
        &coord,
        &id,
        Duration::from_secs(args.timeout),
        Duration::from_secs(2),
    )
    .context("waiting for the result")?;
    let out = status.output.context("committed task has no output")?;
    let bytes = requester::fetch_output(&coord, &out).context("downloading the output")?;
    match args.out {
        Some(p) => std::fs::write(p, &bytes).context("writing output")?,
        None => std::io::stdout().write_all(&bytes)?,
    }
    eprintln!("job {id} committed; output {} bytes", bytes.len());
    Ok(())
}

struct SubmitArgs {
    module: PathBuf,
    input: PathBuf,
    out: Option<PathBuf>,
    timeout: u64,
    fuel: Option<u64>,
    wall_ms: Option<u64>,
    memory: Option<u64>,
    output_limit: Option<u64>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = config(cli.config)?;
    match cli.cmd {
        Cmd::Status => {
            let r = daemon(&cfg, Command::Status)?;
            println!("state: {}", r.state.unwrap_or_default());
            println!("activity: {}", r.activity.unwrap_or_default());
            println!("jobs done today: {}", r.jobs_done_today.unwrap_or(0));
            println!(
                "time helped today: {} min",
                r.helping_seconds_today.unwrap_or(0) / 60
            );
        }
        Cmd::Start => {
            daemon(&cfg, Command::Start)?;
            println!("helper session started");
        }
        Cmd::Stop => {
            daemon(&cfg, Command::Stop)?;
            println!("stopped");
        }
        Cmd::Shutdown => {
            daemon(&cfg, Command::Shutdown)?;
            println!("nc-agent exiting");
        }
        Cmd::Policy { action } => match action {
            PolicyCmd::Show => {
                let r = daemon(&cfg, Command::PolicyShow)?;
                println!("{}", serde_json::to_string_pretty(&r.policy)?);
            }
            PolicyCmd::Set { file } => {
                let text = std::fs::read(&file).context("reading policy file")?;
                let policy: OwnerPolicy =
                    serde_json::from_slice(&text).context("policy file is not valid")?;
                policy.validate().context("policy refused")?;
                daemon(&cfg, Command::PolicySet { policy })?;
                println!("policy saved and applied");
            }
        },
        Cmd::Submit {
            module,
            input,
            out,
            timeout,
            fuel,
            wall_ms,
            memory,
            output_limit,
        } => submit(
            &cfg,
            SubmitArgs {
                module,
                input,
                out,
                timeout,
                fuel,
                wall_ms,
                memory,
                output_limit,
            },
        )?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
        let c = Cli::try_parse_from(["nc", "policy", "set", "p.json"]);
        assert!(c.is_ok());
        let c = Cli::try_parse_from(["nc", "submit", "m.wasm", "in.bin", "--out", "o"]);
        assert!(c.is_ok());
    }
}
