//! Platforms without an implemented OS sandbox. Every job is refused (AGENT.md §1 rule 3: no weaker fallback).
//!
//! TODO(core, Phase 1): Linux namespaces + seccomp + cgroups.

use std::time::Duration;

use super::{ChildPipes, SandboxError, SandboxEvent, SandboxSpec, SetupTimings};

/// Profile name prefix (unused here; kept for API parity).
pub const PROFILE_PREFIX: &str = "nc-exec-";

/// Never constructed on this platform.
#[derive(Debug)]
pub struct SandboxedChild {
    _never: std::convert::Infallible,
}

impl SandboxedChild {
    pub(super) fn spawn(_spec: &SandboxSpec) -> Result<(Self, ChildPipes), SandboxError> {
        Err(SandboxError {
            step: "platform",
            detail: "no OS sandbox implemented for this platform yet".into(),
        })
    }

    /// Unreachable on this platform.
    pub fn resume(&mut self) -> Result<(), SandboxError> {
        match self._never {}
    }

    /// Unreachable on this platform.
    pub fn wait(&mut self, _timeout: Duration) -> Option<u32> {
        match self._never {}
    }

    /// Unreachable on this platform.
    pub fn events(&mut self) -> Vec<SandboxEvent> {
        match self._never {}
    }

    /// Unreachable on this platform.
    pub fn kill(&mut self) {
        match self._never {}
    }

    /// Unreachable on this platform.
    pub fn timings(&self) -> SetupTimings {
        match self._never {}
    }

    /// Unreachable on this platform.
    pub fn teardown(self) -> Duration {
        match self._never {}
    }

    /// Unreachable on this platform.
    pub fn peak_memory_bytes(&self) -> u64 {
        match self._never {}
    }
}

/// Nothing to sweep on this platform.
pub fn sweep_leftovers() -> Result<usize, SandboxError> {
    Ok(0)
}
