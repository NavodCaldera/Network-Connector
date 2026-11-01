//! Resource limits for one job run (PLAN.md §5, AGENT.md §1 rule 3).

use std::time::Duration;

/// Hard upper bounds that no job may ask to exceed. Requests above these are refused, not clamped.
pub mod ceiling {
    /// Largest module accepted (bytes).
    pub const MODULE_BYTES: u64 = 64 * 1024 * 1024;
    /// Largest input accepted (bytes).
    pub const INPUT_BYTES: u64 = 256 * 1024 * 1024;
    /// Largest linear memory a job may ask for (bytes).
    pub const MEMORY_BYTES: u64 = 4 * 1024 * 1024 * 1024;
    /// Largest Wasm stack a job may ask for (bytes).
    pub const STACK_BYTES: u64 = 8 * 1024 * 1024;
    /// Largest output a job may produce (bytes).
    pub const OUTPUT_BYTES: u64 = 256 * 1024 * 1024;
    /// Largest checkpoint a job may write (bytes).
    pub const CHECKPOINT_BYTES: u64 = 64 * 1024 * 1024;
    /// Longest wall-clock time for one run.
    pub const WALL_MS: u64 = 24 * 60 * 60 * 1000;
}

/// Limits applied to one run. Every field is enforced by at least one layer; most by two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Wasmtime fuel (instruction budget). Enforced in the worker.
    pub fuel: u64,
    /// Wall-clock budget. Enforced by epoch interruption in the worker **and** by the parent watchdog.
    pub wall: Duration,
    /// Maximum linear memory. Enforced by the store limiter **and** (plus overhead) by the Job Object.
    pub memory_bytes: u64,
    /// Maximum Wasm stack (`Config::max_wasm_stack`).
    pub stack_bytes: u64,
    /// Maximum output bytes. Enforced by the host API **and** by the parent pipe reader.
    pub output_bytes: u64,
    /// Maximum checkpoint bytes.
    pub checkpoint_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            fuel: 10_000_000_000,
            wall: Duration::from_secs(60),
            memory_bytes: 256 * 1024 * 1024,
            stack_bytes: 512 * 1024,
            output_bytes: 16 * 1024 * 1024,
            checkpoint_bytes: 4 * 1024 * 1024,
        }
    }
}

impl Limits {
    /// Check that every limit is non-zero and below the hard ceilings.
    pub fn validate(&self) -> Result<(), &'static str> {
        let wall_ms = u64::try_from(self.wall.as_millis()).unwrap_or(u64::MAX);
        if self.fuel == 0 {
            return Err("fuel must be > 0");
        }
        if wall_ms == 0 || wall_ms > ceiling::WALL_MS {
            return Err("wall time out of range");
        }
        if self.memory_bytes == 0 || self.memory_bytes > ceiling::MEMORY_BYTES {
            return Err("memory limit out of range");
        }
        if self.stack_bytes < 16 * 1024 || self.stack_bytes > ceiling::STACK_BYTES {
            return Err("stack limit out of range");
        }
        if self.output_bytes > ceiling::OUTPUT_BYTES {
            return Err("output limit out of range");
        }
        if self.checkpoint_bytes > ceiling::CHECKPOINT_BYTES {
            return Err("checkpoint limit out of range");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_valid() {
        assert!(Limits::default().validate().is_ok());
    }

    #[test]
    fn over_ceiling_is_refused() {
        let l = Limits {
            memory_bytes: ceiling::MEMORY_BYTES + 1,
            ..Limits::default()
        };
        assert!(l.validate().is_err());
        let l = Limits {
            fuel: 0,
            ..Limits::default()
        };
        assert!(l.validate().is_err());
    }
}
