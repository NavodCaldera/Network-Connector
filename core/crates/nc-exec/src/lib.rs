//! `nc-exec`: executor for other people's code (AGENT.md §1 rules 3–5).
//!
//! Layers, all required (if one cannot be set up the job is refused, never run with less):
//! 1. module SHA-256 checked against the manifest, publisher signature checked by a [`ModuleVerifier`] ([`Ed25519Publishers`]);
//! 2. a separate, single-use `nc-exec-worker` process;
//! 3. inside it, Wasmtime with profile `cpu-core-v1` ([`runtime`]): narrow `nc.*` host API only (no WASI),
//!    fuel, epoch deadline, memory/table/instance limits, stack limit, output and checkpoint caps;
//! 4. around it the OS sandbox ([`sandbox`]): on Windows a no-capability AppContainer plus a Job Object
//!    (memory cap, one active process, kill-on-close, UI limits), child-process creation blocked;
//! 5. in the parent an external wall-clock watchdog, a capped pipe reader and a cancel token (Stop < 1 s).
//!
//! Teardown runs on every path through `Drop`; [`sandbox::sweep_leftovers`] removes anything a crash left behind.

pub mod executor;
pub mod ipc;
pub mod limits;
pub mod runtime;
pub mod sandbox;
pub mod verify;

pub use executor::{CancelToken, ExecError, Executor, ExecutorConfig, Job, Outcome, Termination};
pub use limits::Limits;
pub use verify::{sha256, Ed25519Publishers, ModuleVerifier, PublisherSignature};
