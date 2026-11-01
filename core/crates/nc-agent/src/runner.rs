//! Job execution behind a trait. The real runner is nc-exec (separate worker process, AppContainer + Job
//! Object on Windows, profile `cpu-core-v1`, fuel + epoch + external watchdog). If the sandbox cannot be set
//! up nc-exec refuses the job; there is no fallback runner in this crate.

use nc_exec::{CancelToken, Ed25519Publishers, ExecError, Executor, ExecutorConfig, Job, Outcome};
use nc_protocol::pb::v1::TaskManifest;
use std::path::PathBuf;

/// Runs one verified job.
pub trait JobRunner: Send + Sync {
    /// Run `task` with `module` and `input`, trusting only `publishers` for the module signature. Must honour
    /// `cancel` within well under a second.
    fn run(
        &self,
        task: &TaskManifest,
        publishers: &[[u8; 32]],
        module: Vec<u8>,
        input: Vec<u8>,
        cancel: &CancelToken,
    ) -> Result<Outcome, ExecError>;
}

/// nc-exec runner.
#[derive(Debug, Clone)]
pub struct SandboxRunner {
    worker: PathBuf,
}

impl SandboxRunner {
    /// Runner using the `nc-exec-worker` binary at `worker` (on Windows in a folder AppContainers can read).
    pub fn new(worker: impl Into<PathBuf>) -> Self {
        Self {
            worker: worker.into(),
        }
    }

    /// Start-up: sweep sandbox leftovers from a crash (AGENT.md §1 rule 4) and warm the worker so the first job
    /// starts (and stops) fast.
    pub fn prepare(&self) -> Result<(), ExecError> {
        nc_exec::sandbox::sweep_leftovers()?;
        let exec = Executor::new(
            ExecutorConfig::new(&self.worker),
            Box::new(Ed25519Publishers::new([])),
        );
        exec.warm_up().map(|_| ())
    }
}

impl JobRunner for SandboxRunner {
    fn run(
        &self,
        task: &TaskManifest,
        publishers: &[[u8; 32]],
        module: Vec<u8>,
        input: Vec<u8>,
        cancel: &CancelToken,
    ) -> Result<Outcome, ExecError> {
        let job = Job::from_manifest(task, module, input)?;
        let exec = Executor::new(
            ExecutorConfig::new(&self.worker),
            Box::new(Ed25519Publishers::new(publishers.iter().copied())),
        );
        exec.run(&job, cancel)
    }
}
