//! OS sandbox around the worker process.
//!
//! [`spawn`] either returns a child with **every** layer applied or an error; there is no partial mode.

use std::fs::File;
use std::path::PathBuf;
use std::time::Duration;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::{
    create_named_profile_for_test, grant_appcontainer_read, list_profiles, sweep_leftovers,
    SandboxedChild, PROFILE_PREFIX,
};

#[cfg(not(windows))]
mod unsupported;
#[cfg(not(windows))]
pub use self::unsupported::{sweep_leftovers, SandboxedChild, PROFILE_PREFIX};

/// What the sandbox needs to start the worker.
#[derive(Debug, Clone)]
pub struct SandboxSpec {
    /// Path of the `nc-exec-worker` binary. Its folder must be readable by AppContainers
    /// (see `grant_appcontainer_read` on Windows).
    pub worker: PathBuf,
    /// Committed-memory cap for the whole worker process (Job Object).
    pub process_memory_bytes: u64,
}

/// A sandbox layer could not be applied. The job must be refused.
#[derive(Debug, thiserror::Error)]
#[error("sandbox unavailable: {step}: {detail}")]
pub struct SandboxError {
    /// Setup step that failed.
    pub step: &'static str,
    /// OS error text (never contains job data).
    pub detail: String,
}

/// Something the OS reported about the sandboxed process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxEvent {
    /// The process hit the Job Object memory cap.
    MemoryLimit,
    /// The process tried to start another process (escape signal).
    ProcessLimit,
}

/// Pipes to the worker.
pub struct ChildPipes {
    /// Worker stdin (request).
    pub stdin: File,
    /// Worker stdout (response).
    pub stdout: File,
    /// Worker stderr (drained and discarded).
    pub stderr: File,
}

/// Time spent in the sandbox setup steps.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SetupTimings {
    /// Profile / container creation.
    pub profile: Duration,
    /// Job Object creation and process launch.
    pub launch: Duration,
}

/// Start the worker with every layer applied.
pub fn spawn(spec: &SandboxSpec) -> Result<(SandboxedChild, ChildPipes), SandboxError> {
    SandboxedChild::spawn(spec)
}
