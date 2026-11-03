//! `nc`: command line for the Network Connector agent.
//!
//! Helper side (talks to the local `nc-agent` daemon over its loopback control socket):
//! `nc key` prints this device's public key (base64url) to share as a trusted publisher key.
//! `nc status`, `nc start`, `nc stop`, `nc policy show`, `nc policy set <policy.json>`, `nc shutdown`.
//!
//! Requester side (talks to the coordinator directly, signing as this device):
//! `nc submit <module.wasm> <input>` signs the job manifest (this device is the publisher), records it under
//! `<data_dir>/requests/<id>/`, uploads the module and input, submits and prints the job id (detached).
//! With `--wait` or `--out <file>` it also waits and writes the verified output (the old blocking form).
//! `nc job status|wait <id>` and `nc job fetch <id> -o <file>` pick the job up from any later process, also
//! after a crash or restart: a job the coordinator never got is uploaded and submitted first. Transient
//! coordinator failures are retried until `--timeout`; the output is checked against the committed hash.
//! `--replicas 2` asks for two-helper validation. With `"peer_listen": true` in the config, `nc submit` with
//! `--wait`/`--out` listens on random ports while it waits and serves the job's blobs directly to the helpers
//! the coordinator lists (charged to this device's data budget); off by default.

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use nc_agent::config::AgentConfig;
use nc_agent::control::{self, Command};
use nc_agent::coord::Coordinator;
use nc_agent::jobs::{self, JobStore, Retry};
use nc_agent::requester::{self, JobSpec};
use nc_agent::serve::Serving;
use nc_policy::OwnerPolicy;
use nc_protocol::pb::v1::RegisterRequest;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
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
    /// Print this device's public key (base64url) to share as a trusted publisher key.
    Key,
    /// Show or change the owner policy.
    Policy {
        #[command(subcommand)]
        action: PolicyCmd,
    },
    /// Submit a Wasm job and print its id (add --wait or --out to block for the output).
    Submit {
        /// Wasm module (profile cpu-core-v1).
        module: PathBuf,
        /// Input file.
        input: PathBuf,
        /// Write the output here instead of stdout.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Wait for the output (to stdout unless --out).
        #[arg(long)]
        wait: bool,
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
        /// Independent helpers that must return the same output before the task commits (1 or 2).
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..=2))]
        replicas: u32,
    },
    /// Follow up a submitted job (works from any later process).
    Job {
        #[command(subcommand)]
        action: JobCmd,
    },
}

#[derive(Subcommand, Debug)]
enum JobCmd {
    /// Print the job state (submits first if the coordinator never got it).
    Status {
        /// Job id from `nc submit`.
        id: String,
        /// Seconds to keep retrying an unreachable coordinator.
        #[arg(long, default_value_t = 60)]
        timeout: u64,
    },
    /// Wait until the job is committed or ends.
    Wait {
        /// Job id.
        id: String,
        /// Seconds to wait.
        #[arg(long, default_value_t = 600)]
        timeout: u64,
    },
    /// Wait for, download and verify the output.
    Fetch {
        /// Job id.
        id: String,
        /// Output file.
        #[arg(short = 'o', long = "out")]
        out: PathBuf,
        /// Seconds to wait.
        #[arg(long, default_value_t = 600)]
        timeout: u64,
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
    spec.replicas = args.replicas;
    let key = cfg.device_key()?;
    let signer = nc_identity::RootSigner(key.as_ref());
    let job = requester::build(&spec, &signer, chrono::Utc::now().timestamp_millis())
        .context("building the job")?;
    let coord = Arc::new(requester_coord(cfg)?);
    let store = JobStore::new(&cfg.data_dir);
    store
        .stage(&job, chrono::Utc::now().timestamp_millis())
        .context("recording the job")?;
    let retry = Retry::cli(Duration::from_secs(args.timeout));
    store
        .ensure_submitted(coord.as_ref(), &job.job_id, retry)
        .with_context(|| {
            format!(
                "submitting job {}; resume with `nc job status {}`",
                job.job_id, job.job_id
            )
        })?;
    let id = job.job_id;
    eprintln!("submitted job {id}");
    if !args.wait && args.out.is_none() {
        println!("{id}");
        return Ok(());
    }
    eprintln!("waiting for job {id}");
    // `peer_listen` in the config: serve the job's blobs to its helpers directly while waiting.
    let serving = cfg
        .direct_serving(coord.clone())
        .context("starting direct serving")?
        .map(|ctx| {
            eprintln!("serving the job's inputs directly to its helpers");
            let blobs = job.blobs.into_iter().map(|(_, b)| b).collect();
            Serving::spawn(ctx, id.clone(), blobs, Duration::from_secs(args.timeout))
        });
    let fetched = store.fetch(coord.as_ref(), &id, retry, POLL);
    drop(serving);
    let bytes = fetched.context("waiting for the result")?;
    match args.out {
        Some(p) => jobs::write_output(&p, &bytes).context("writing output")?,
        None => std::io::stdout().write_all(&bytes)?,
    }
    eprintln!("job {id} committed; output {} bytes", bytes.len());
    Ok(())
}

const POLL: Duration = Duration::from_secs(2);

fn requester_coord(cfg: &AgentConfig) -> Result<nc_agent::coord::HttpCoordinator> {
    let key = cfg.device_key()?;
    let coord = cfg.coordinator(key);
    coord
        .register(&RegisterRequest {
            name: cfg.device_name.clone(),
            platform: std::env::consts::OS.into(),
        })
        .context("registering with the coordinator")?;
    Ok(coord)
}

fn job_cmd(cfg: &AgentConfig, action: JobCmd) -> Result<()> {
    let store = JobStore::new(&cfg.data_dir);
    match action {
        JobCmd::Status { id, timeout } => {
            store.load(&id).context("job")?;
            let coord = requester_coord(cfg)?;
            let t = store
                .status(&coord, &id, Retry::cli(Duration::from_secs(timeout)))
                .context("job status")?;
            println!(
                "{}",
                if t.state.is_empty() {
                    "unknown"
                } else {
                    &t.state
                }
            );
        }
        JobCmd::Wait { id, timeout } => {
            store.load(&id).context("job")?;
            let coord = requester_coord(cfg)?;
            let t = store
                .wait(&coord, &id, Retry::cli(Duration::from_secs(timeout)), POLL)
                .context("waiting for the job")?;
            println!("{}", t.state);
        }
        JobCmd::Fetch { id, out, timeout } => {
            store.load(&id).context("job")?;
            let coord = requester_coord(cfg)?;
            let bytes = store
                .fetch(&coord, &id, Retry::cli(Duration::from_secs(timeout)), POLL)
                .context("fetching the output")?;
            jobs::write_output(&out, &bytes).context("writing output")?;
            eprintln!("job {id} output verified; {} bytes", bytes.len());
        }
    }
    Ok(())
}

struct SubmitArgs {
    module: PathBuf,
    input: PathBuf,
    out: Option<PathBuf>,
    wait: bool,
    timeout: u64,
    fuel: Option<u64>,
    wall_ms: Option<u64>,
    memory: Option<u64>,
    output_limit: Option<u64>,
    replicas: u32,
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
        Cmd::Key => {
            // Public key only; the private key never leaves the key store.
            println!(
                "{}",
                cfg.public_key_b64().context("opening the device key")?
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
            wait,
            timeout,
            fuel,
            wall_ms,
            memory,
            output_limit,
            replicas,
        } => submit(
            &cfg,
            SubmitArgs {
                module,
                input,
                out,
                wait,
                timeout,
                fuel,
                wall_ms,
                memory,
                output_limit,
                replicas,
            },
        )?,
        Cmd::Job { action } => job_cmd(&cfg, action)?,
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
        let c = Cli::try_parse_from(["nc", "submit", "m.wasm", "i", "--replicas", "2"]);
        assert!(matches!(
            c.map(|c| c.cmd),
            Ok(Cmd::Submit { replicas: 2, .. })
        ));
        assert!(Cli::try_parse_from(["nc", "submit", "m.wasm", "i", "--replicas", "3"]).is_err());
        let c = Cli::try_parse_from(["nc", "job", "fetch", "id", "-o", "out.bin"]);
        assert!(matches!(
            c.map(|c| c.cmd),
            Ok(Cmd::Job {
                action: JobCmd::Fetch { .. }
            })
        ));
        let c = Cli::try_parse_from(["nc", "job", "wait", "id", "--timeout", "5"]);
        assert!(matches!(
            c.map(|c| c.cmd),
            Ok(Cmd::Job {
                action: JobCmd::Wait { timeout: 5, .. }
            })
        ));
        assert!(Cli::try_parse_from(["nc", "job", "fetch", "id"]).is_err());
        let c = Cli::try_parse_from(["nc", "submit", "m.wasm", "i", "--wait"]);
        assert!(matches!(
            c.map(|c| c.cmd),
            Ok(Cmd::Submit { wait: true, .. })
        ));
        let c = Cli::try_parse_from(["nc", "key"]);
        assert!(matches!(c.map(|c| c.cmd), Ok(Cmd::Key)));
    }
}
