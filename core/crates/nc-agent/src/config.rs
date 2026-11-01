//! Daemon configuration file (JSON) and the wiring from it to a real [`crate::Agent`].
//!
//! The enrolment secret is never in the file: it comes from the `NC_ENROLL_SECRET` environment variable.

use crate::budget::{BudgetGate, NetworkBudget};
use crate::coord::HttpCoordinator;
use crate::journal::Journal;
use crate::probe::{ProbeConfig, SystemClock, SystemProbe};
use crate::runner::SandboxRunner;
use crate::session::{Backend, Tuning};
use crate::transfer::BlobCache;
use crate::Agent;
use nc_identity::{DeviceKeyStore, SoftwareKeyStore};
use nc_policy::OwnerPolicy;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Environment variable holding the coordinator enrolment secret.
pub const ENROLL_ENV: &str = "NC_ENROLL_SECRET";
/// Largest config or policy file read.
const MAX_FILE: u64 = 256 * 1024;

/// Config errors (no secrets, no file contents).
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// File missing or unreadable.
    #[error("cannot read {0}")]
    Read(&'static str),
    /// File does not parse or is invalid.
    #[error("invalid {0}")]
    Invalid(&'static str),
    /// Start-up failure of a component.
    #[error("cannot open {0}")]
    Open(&'static str),
}

/// `nc-agent` configuration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    /// Coordinator base URL, e.g. `https://coordinator.example` (HTTP only for the loopback test stack).
    pub coordinator_url: String,
    /// Private folder for the device key, lease journal, blob cache and staging.
    pub data_dir: PathBuf,
    /// Display name sent on register.
    pub device_name: String,
    /// Path to `nc-exec-worker` (on Windows in a folder AppContainers may read).
    pub worker_path: PathBuf,
    /// Owner policy file; default `<data_dir>/policy.json`. Missing file = safe defaults (no consent).
    #[serde(default)]
    pub policy_file: Option<PathBuf>,
    /// Network cost: `"unmetered"` or `{"metered": <nc-tariff profile>}`. Required: the owner must say.
    pub network_budget: NetworkBudget,
    /// Owner-declared facts the platform cannot report.
    pub probe: ProbeConfig,
    /// Loopback port of the local control socket used by `nc`.
    #[serde(default = "default_port")]
    pub control_port: u16,
    /// Poll interval without work, seconds.
    #[serde(default = "default_poll")]
    pub poll_seconds: u64,
    /// Blob cache quota, MiB.
    #[serde(default = "default_quota")]
    pub cache_quota_mb: u64,
}

fn default_port() -> u16 {
    47_611
}
fn default_poll() -> u64 {
    10
}
fn default_quota() -> u64 {
    512
}

fn read_capped(path: &Path, what: &'static str) -> Result<Vec<u8>, ConfigError> {
    let meta = std::fs::metadata(path).map_err(|_| ConfigError::Read(what))?;
    if meta.len() > MAX_FILE {
        return Err(ConfigError::Invalid(what));
    }
    std::fs::read(path).map_err(|_| ConfigError::Read(what))
}

impl AgentConfig {
    /// Load and check a config file.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let c: Self = serde_json::from_slice(&read_capped(path, "config file")?)
            .map_err(|_| ConfigError::Invalid("config file"))?;
        let url_ok = c.coordinator_url.starts_with("https://")
            || c.coordinator_url.starts_with("http://127.0.0.1")
            || c.coordinator_url.starts_with("http://localhost");
        if !url_ok {
            return Err(ConfigError::Invalid(
                "coordinator_url (https required except loopback)",
            ));
        }
        if let NetworkBudget::Metered(p) = &c.network_budget {
            p.validate()
                .map_err(|_| ConfigError::Invalid("network_budget"))?;
        }
        if c.poll_seconds == 0 || c.device_name.is_empty() {
            return Err(ConfigError::Invalid("config file"));
        }
        Ok(c)
    }

    /// Policy file path.
    pub fn policy_path(&self) -> PathBuf {
        self.policy_file
            .clone()
            .unwrap_or_else(|| self.data_dir.join("policy.json"))
    }

    /// Device key path.
    pub fn key_path(&self) -> PathBuf {
        self.data_dir.join("device.key")
    }

    /// Control token path.
    pub fn token_path(&self) -> PathBuf {
        self.data_dir.join("control.token")
    }

    /// Load the owner policy; a missing file gives nc-policy's safe defaults.
    pub fn load_policy(&self) -> Result<OwnerPolicy, ConfigError> {
        let p = self.policy_path();
        if !p.exists() {
            return Ok(OwnerPolicy::default());
        }
        let policy: OwnerPolicy = serde_json::from_slice(&read_capped(&p, "policy file")?)
            .map_err(|_| ConfigError::Invalid("policy file"))?;
        policy
            .validate()
            .map_err(|_| ConfigError::Invalid("policy file"))?;
        Ok(policy)
    }

    /// Save the owner policy atomically.
    pub fn save_policy(&self, policy: &OwnerPolicy) -> Result<(), ConfigError> {
        let p = self.policy_path();
        let bytes =
            serde_json::to_vec_pretty(policy).map_err(|_| ConfigError::Invalid("policy"))?;
        let tmp = p.with_extension("tmp");
        std::fs::write(&tmp, bytes).map_err(|_| ConfigError::Open("policy file"))?;
        std::fs::rename(&tmp, &p).map_err(|_| ConfigError::Open("policy file"))
    }

    /// Open the device key (created on first use; software assurance, DPAPI-sealed on Windows).
    pub fn device_key(&self) -> Result<Arc<dyn DeviceKeyStore>, ConfigError> {
        std::fs::create_dir_all(&self.data_dir).map_err(|_| ConfigError::Open("data_dir"))?;
        let k = SoftwareKeyStore::open_or_create(self.key_path())
            .map_err(|_| ConfigError::Open("device key"))?;
        Ok(Arc::new(k))
    }

    /// Coordinator client signing with `key`.
    pub fn coordinator(&self, key: Arc<dyn DeviceKeyStore>) -> HttpCoordinator {
        HttpCoordinator::new(&self.coordinator_url, key, std::env::var(ENROLL_ENV).ok())
    }

    /// Build the real agent.
    pub fn build_agent(&self) -> Result<Agent, ConfigError> {
        let key = self.device_key()?;
        let runner = SandboxRunner::new(&self.worker_path);
        if let Err(e) = runner.prepare() {
            // Not fatal here: every job will be refused with SANDBOX_UNAVAILABLE instead of running weaker.
            tracing::warn!(error = %e, "sandbox not ready; jobs will be refused");
        }
        let staging = self.data_dir.join("staging");
        std::fs::create_dir_all(&staging).map_err(|_| ConfigError::Open("staging"))?;
        let journal = Journal::open(&self.data_dir.join("journal"))
            .map_err(|_| ConfigError::Open("journal"))?;
        let cache = BlobCache::open(
            &self.data_dir.join("blobs"),
            self.cache_quota_mb.saturating_mul(1024 * 1024),
        )
        .map_err(|_| ConfigError::Open("blob cache"))?;
        let budget = BudgetGate::new(&self.network_budget, chrono::Utc::now())
            .map_err(|_| ConfigError::Invalid("network_budget"))?;
        let backend = Backend {
            coord: Arc::new(self.coordinator(key)),
            probe: Arc::new(SystemProbe::new(self.probe.clone())),
            clock: Arc::new(SystemClock),
            runner: Arc::new(runner),
            journal: Mutex::new(journal),
            cache: Mutex::new(cache),
            budget,
            staging,
            device_name: self.device_name.clone(),
            platform: std::env::consts::OS.to_string(),
            tuning: Tuning {
                poll: Duration::from_secs(self.poll_seconds),
                ..Tuning::default()
            },
        };
        Ok(Agent::with_backend(backend))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses_and_requires_https() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.json");
        let mut v = serde_json::json!({
            "coordinator_url": "https://coordinator.example",
            "data_dir": dir.path(),
            "device_name": "pc",
            "worker_path": "nc-exec-worker.exe",
            "network_budget": "unmetered",
            "probe": {"network": {"interface": "ethernet", "connection_id": "home", "bytes_per_second": 1000000}, "declared_thermal": "nominal"}
        });
        std::fs::write(&path, v.to_string()).unwrap();
        let c = AgentConfig::load(&path).unwrap();
        assert_eq!(c.control_port, 47_611);
        assert_eq!(c.load_policy().unwrap(), OwnerPolicy::default());
        v["coordinator_url"] = "http://coordinator.example".into();
        std::fs::write(&path, v.to_string()).unwrap();
        assert!(AgentConfig::load(&path).is_err());
        v["coordinator_url"] = "https://x".into();
        v["surprise"] = 1.into();
        std::fs::write(&path, v.to_string()).unwrap();
        assert!(AgentConfig::load(&path).is_err(), "unknown fields refused");
    }
}
