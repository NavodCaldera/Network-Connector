//! Parent side: checks, sandbox spawn, external wall-clock watchdog, cancel, capped result reading, teardown.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::ipc::{WorkerRequest, WorkerResponse, WorkerStatus};
use crate::limits::{ceiling, Limits};
use crate::sandbox::{self, SandboxEvent, SandboxSpec};
use crate::verify::{sha256, ModuleVerifier, PublisherSignature};

/// How often the watchdog checks cancel, deadline and Job Object events.
pub const WATCHDOG_TICK: Duration = Duration::from_millis(5);

/// Cancel handle shared with whoever may press Stop. Cheap to clone.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// New, not cancelled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation; the running job is killed within one watchdog tick plus kill time.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether cancel was requested.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// A job ready to run: the module, what the manifest says about it, its input and limits.
#[derive(Debug, Clone)]
pub struct Job {
    /// Module bytes.
    pub module: Vec<u8>,
    /// SHA-256 the manifest declares for the module.
    pub module_sha256: [u8; 32],
    /// Publisher signature over `module_sha256`.
    pub publisher: PublisherSignature,
    /// Input bytes.
    pub input: Vec<u8>,
    /// Limits.
    pub limits: Limits,
}

/// Executor settings.
#[derive(Debug, Clone)]
pub struct ExecutorConfig {
    /// Path of the `nc-exec-worker` binary (in a folder readable by AppContainers on Windows).
    pub worker: PathBuf,
    /// Committed memory allowed for the worker on top of the job's linear memory (runtime, compiled code, I/O).
    pub process_overhead_bytes: u64,
    /// Extra wall time the parent watchdog allows on top of `limits.wall` (process start + compile).
    pub watchdog_grace: Duration,
}

impl ExecutorConfig {
    /// Defaults for a worker at `worker`.
    pub fn new(worker: impl Into<PathBuf>) -> Self {
        Self {
            worker: worker.into(),
            process_overhead_bytes: 192 * 1024 * 1024,
            watchdog_grace: Duration::from_secs(2),
        }
    }
}

/// Why a job was refused before or instead of running. A refused job never ran any guest code.
#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    /// Limits out of range.
    #[error("limits refused: {0}")]
    BadLimits(&'static str),
    /// Module or input bigger than the hard ceiling.
    #[error("job too large")]
    TooLarge,
    /// Module bytes do not hash to the manifest's hash.
    #[error("module hash mismatch")]
    HashMismatch,
    /// Publisher signature invalid or publisher not trusted.
    #[error("publisher signature refused: {0}")]
    SignatureInvalid(String),
    /// A sandbox layer could not be applied (no fallback).
    #[error(transparent)]
    Sandbox(#[from] sandbox::SandboxError),
    /// The worker refused the module (invalid, forbidden import, missing export).
    #[error("module refused: {0}")]
    Refused(String),
}

/// How a run that did start ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Termination {
    /// `run` returned this code.
    Exited(i32),
    /// Fuel ran out.
    OutOfFuel,
    /// Worker-side epoch deadline.
    EpochDeadline,
    /// Parent watchdog killed the process (wall time + grace exceeded).
    WatchdogKilled,
    /// Linear memory growth refused by the store limiter.
    MemoryLimit,
    /// Job Object memory cap hit; process killed.
    JobMemoryKilled,
    /// Wasm stack overflow.
    StackOverflow,
    /// Output or checkpoint cap hit (host API or parent pipe reader).
    OutputLimit,
    /// Another trap; message is the trap kind only.
    Trap(String),
    /// Cancelled through [`CancelToken`].
    Cancelled,
    /// Process tried to create another process; killed.
    EscapeAttempt,
    /// Worker died without a valid reply (exit code).
    WorkerCrashed(u32),
}

/// Measured times for one run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Timings {
    /// Sandbox profile creation.
    pub sandbox_profile: Duration,
    /// Job Object + process launch.
    pub sandbox_launch: Duration,
    /// Module compile inside the worker.
    pub compile: Duration,
    /// Guest run inside the worker.
    pub run: Duration,
    /// From kill request (cancel / watchdog / limit) to process exit; zero if not killed.
    pub kill_latency: Duration,
    /// Sandbox teardown.
    pub teardown: Duration,
    /// Whole call.
    pub total: Duration,
}

/// Result of a run that started.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// How it ended.
    pub termination: Termination,
    /// Output (empty unless the worker replied).
    pub output: Vec<u8>,
    /// Last checkpoint.
    pub checkpoint: Vec<u8>,
    /// Fuel consumed.
    pub fuel_used: u64,
    /// Peak committed memory of the worker process (0 where unknown).
    pub peak_memory_bytes: u64,
    /// Timings.
    pub timings: Timings,
}

/// Runs jobs, one sandboxed worker process per job.
pub struct Executor {
    config: ExecutorConfig,
    verifier: Box<dyn ModuleVerifier>,
}

impl std::fmt::Debug for Executor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Executor")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

/// Read `r` to EOF keeping at most `cap` bytes. Sets `over` (and stops) as soon as the cap is passed.
fn drain_capped(
    mut r: impl Read + Send + 'static,
    cap: u64,
    over: Arc<AtomicBool>,
) -> JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            match r.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if buf.len() as u64 + n as u64 > cap {
                        over.store(true, Ordering::SeqCst);
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
            }
        }
        buf
    })
}

impl Executor {
    /// New executor. Call [`sandbox::sweep_leftovers`] once at agent start-up as well.
    pub fn new(config: ExecutorConfig, verifier: Box<dyn ModuleVerifier>) -> Self {
        Self { config, verifier }
    }

    /// Start the worker once inside a full sandbox without resuming it, then tear down. The first launch of a
    /// newly written worker binary took 2.8–4.8 s on the dev PC (inside `CreateProcessW`, probably the
    /// anti-malware scan); call this at agent start-up and after updates so Stop and job start stay fast.
    /// No guest code runs. Returns the time taken.
    pub fn warm_up(&self) -> Result<Duration, ExecError> {
        let t = Instant::now();
        let spec = SandboxSpec {
            worker: self.config.worker.clone(),
            process_memory_bytes: self.config.process_overhead_bytes,
        };
        let (child, _pipes) = sandbox::spawn(&spec)?;
        child.teardown();
        Ok(t.elapsed())
    }

    /// Run `job`. Refusals return `Err` without running guest code; any started run returns an [`Outcome`]
    /// and its sandbox has been torn down before this returns.
    pub fn run(&self, job: &Job, cancel: &CancelToken) -> Result<Outcome, ExecError> {
        let t_start = Instant::now();
        job.limits.validate().map_err(ExecError::BadLimits)?;
        if job.module.len() as u64 > ceiling::MODULE_BYTES
            || job.input.len() as u64 > ceiling::INPUT_BYTES
        {
            return Err(ExecError::TooLarge);
        }
        if sha256(&job.module) != job.module_sha256 {
            return Err(ExecError::HashMismatch);
        }
        self.verifier
            .verify(&job.module_sha256, &job.publisher)
            .map_err(ExecError::SignatureInvalid)?;
        if cancel.is_cancelled() {
            return Ok(Outcome {
                termination: Termination::Cancelled,
                output: Vec::new(),
                checkpoint: Vec::new(),
                fuel_used: 0,
                peak_memory_bytes: 0,
                timings: Timings {
                    total: t_start.elapsed(),
                    ..Timings::default()
                },
            });
        }

        let spec = SandboxSpec {
            worker: self.config.worker.clone(),
            process_memory_bytes: job
                .limits
                .memory_bytes
                .saturating_add(self.config.process_overhead_bytes),
        };
        // The worker starts suspended: a Stop that arrives while CreateProcess is still running (the first
        // launch of a new binary can take > 1 s on this PC) is honoured before any guest code runs.
        let (mut child, pipes) = sandbox::spawn(&spec)?;
        let setup = child.timings();
        if cancel.is_cancelled() {
            let teardown = child.teardown();
            return Ok(Outcome {
                termination: Termination::Cancelled,
                output: Vec::new(),
                checkpoint: Vec::new(),
                fuel_used: 0,
                peak_memory_bytes: 0,
                timings: Timings {
                    sandbox_profile: setup.profile,
                    sandbox_launch: setup.launch,
                    teardown,
                    total: t_start.elapsed(),
                    ..Timings::default()
                },
            });
        }
        child.resume()?;

        let req = WorkerRequest {
            limits: job.limits,
            module_sha256: job.module_sha256,
            module: job.module.clone(),
            input: job.input.clone(),
        };
        let mut stdin = pipes.stdin;
        let writer = std::thread::spawn(move || {
            let mut w = std::io::BufWriter::new(&mut stdin);
            let _ = req.write_to(&mut w);
            let _ = w.flush();
        });
        let out_cap =
            WorkerResponse::max_encoded_len(job.limits.output_bytes, job.limits.checkpoint_bytes);
        let over = Arc::new(AtomicBool::new(false));
        let reader = drain_capped(pipes.stdout, out_cap, over.clone());
        let stderr = drain_capped(pipes.stderr, 64 * 1024, Arc::new(AtomicBool::new(false)));

        let deadline = Instant::now() + job.limits.wall + self.config.watchdog_grace;
        let mut forced: Option<Termination> = None;
        let mut kill_at: Option<Instant> = None;
        let exit_code = loop {
            if forced.is_none() {
                for ev in child.events() {
                    forced.get_or_insert(match ev {
                        SandboxEvent::MemoryLimit => Termination::JobMemoryKilled,
                        SandboxEvent::ProcessLimit => Termination::EscapeAttempt,
                    });
                }
                if forced.is_none() && cancel.is_cancelled() {
                    forced = Some(Termination::Cancelled);
                }
                if forced.is_none() && Instant::now() >= deadline {
                    forced = Some(Termination::WatchdogKilled);
                }
                if forced.is_none() && over.load(Ordering::SeqCst) {
                    forced = Some(Termination::OutputLimit);
                }
                if forced.is_some() {
                    kill_at = Some(Instant::now());
                    child.kill();
                }
            }
            if let Some(code) = child.wait(WATCHDOG_TICK) {
                break code;
            }
        };
        let kill_latency = kill_at.map(|t| t.elapsed()).unwrap_or_default();
        let peak = child.peak_memory_bytes();
        let teardown = child.teardown();

        let bytes = reader.join().unwrap_or_default();
        let over = over.load(Ordering::SeqCst);
        let _ = writer.join();
        let _ = stderr.join();

        let mut outcome = Outcome {
            termination: Termination::WorkerCrashed(exit_code),
            output: Vec::new(),
            checkpoint: Vec::new(),
            fuel_used: 0,
            peak_memory_bytes: peak,
            timings: Timings {
                sandbox_profile: setup.profile,
                sandbox_launch: setup.launch,
                kill_latency,
                teardown,
                ..Timings::default()
            },
        };
        if let Some(t) = forced {
            outcome.termination = t;
        } else if over {
            outcome.termination = Termination::OutputLimit;
        } else if let Ok(resp) = WorkerResponse::read_from(
            &mut bytes.as_slice(),
            job.limits.output_bytes,
            job.limits.checkpoint_bytes,
        ) {
            outcome.timings.compile = Duration::from_micros(resp.compile_us);
            outcome.timings.run = Duration::from_micros(resp.run_us);
            outcome.fuel_used = resp.fuel_used;
            outcome.termination = match resp.status {
                WorkerStatus::Exited => Termination::Exited(resp.exit_code),
                WorkerStatus::OutOfFuel => Termination::OutOfFuel,
                WorkerStatus::EpochDeadline => Termination::EpochDeadline,
                WorkerStatus::MemoryLimit => Termination::MemoryLimit,
                WorkerStatus::StackOverflow => Termination::StackOverflow,
                WorkerStatus::OutputLimit => Termination::OutputLimit,
                WorkerStatus::Trap => Termination::Trap(resp.message),
                WorkerStatus::Refused => return Err(ExecError::Refused(resp.message)),
            };
            if matches!(outcome.termination, Termination::Exited(_)) {
                outcome.output = resp.output;
                outcome.checkpoint = resp.checkpoint;
            } else {
                // A failed run's checkpoint is still useful for resume; its partial output is not.
                outcome.checkpoint = resp.checkpoint;
            }
        }
        outcome.timings.total = t_start.elapsed();
        Ok(outcome)
    }
}
