//! Daemon configuration file (JSON) and the wiring from it to a real [`crate::Agent`].
//!
//! The enrolment secret is never in the file: it comes from the `NC_ENROLL_SECRET` environment variable.

use crate::budget::{BudgetGate, NetworkBudget};
use crate::coord::{Coordinator, HttpCoordinator};
use crate::journal::Journal;
use crate::parked::{derive_key, ParkedResults};
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
    /// Data budget ledger (nc-store `budget_usage`); default `<data_dir>/budget.sqlite`. Set it to keep the
    /// ledger outside `data_dir`, e.g. so a reset that deletes `data_dir` cannot spend a budget twice. When
    /// set, no ledger is created in `data_dir`.
    #[serde(default)]
    pub budget_ledger: Option<PathBuf>,
}

/// Message the device key signs to derive the parked-result key (never sent anywhere).
const PARKED_KEY_LABEL: &[u8] = b"nc-agent/parked-results/v1";

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

    /// Data budget ledger path.
    pub fn budget_ledger_path(&self) -> PathBuf {
        self.budget_ledger
            .clone()
            .unwrap_or_else(|| self.data_dir.join("budget.sqlite"))
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

    /// This device's public key as base64url without padding (the trusted-publisher key format). Creates the
    /// key on first use. The private key is never returned.
    pub fn public_key_b64(&self) -> Result<String, ConfigError> {
        use base64::Engine;
        let key = self.device_key()?;
        Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key.public_key()))
    }

    /// Coordinator client signing with `key`.
    pub fn coordinator(&self, key: Arc<dyn DeviceKeyStore>) -> HttpCoordinator {
        HttpCoordinator::new(&self.coordinator_url, key, std::env::var(ENROLL_ENV).ok())
    }

    /// Build the real agent (HTTPS coordinator signing with the device key; registers on start).
    pub fn build_agent(&self) -> Result<Agent, ConfigError> {
        let key = self.device_key()?;
        self.build_agent_with(Arc::new(self.coordinator(key)))
    }

    /// Build the real agent with the same wiring as [`Self::build_agent`] (sandbox runner, journal, blob
    /// cache, persisted budget ledger, system probe, tuning) but talking to `coord`. Wrap `coord` in
    /// [`crate::coord::SkipRegister`] on a device that is already enrolled.
    pub fn build_agent_with(&self, coord: Arc<dyn Coordinator>) -> Result<Agent, ConfigError> {
        Ok(Agent::with_backend(self.backend_with(coord)?))
    }

    /// The [`Backend`] [`Self::build_agent_with`] uses, for callers that need to adjust it first.
    pub fn backend_with(&self, coord: Arc<dyn Coordinator>) -> Result<Backend, ConfigError> {
        std::fs::create_dir_all(&self.data_dir).map_err(|_| ConfigError::Open("data_dir"))?;
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
        // Parked results must survive a restart, so their key is derived from the device key: Ed25519
        // signatures are deterministic, and the key file itself is OS-protected (DPAPI on Windows).
        let device_key = self.device_key()?;
        let secret = device_key
            .sign(PARKED_KEY_LABEL)
            .map_err(|_| ConfigError::Open("device key"))?;
        let parked = ParkedResults::open(
            &self.data_dir.join("parked"),
            self.cache_quota_mb.saturating_mul(1024 * 1024),
            derive_key(&secret),
        )
        .map_err(|_| ConfigError::Open("parked results"))?;
        let ledger = self.budget_ledger_path();
        if let Some(parent) = ledger.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|_| ConfigError::Open("budget usage"))?;
        }
        let usage =
            nc_store::Store::open(&ledger).map_err(|_| ConfigError::Open("budget usage"))?;
        let budget = BudgetGate::with_usage_store(&self.network_budget, chrono::Utc::now(), usage)
            .map_err(|_| ConfigError::Invalid("network_budget"))?;
        let backend = Backend {
            coord,
            probe: Arc::new(SystemProbe::new(self.probe.clone())),
            clock: Arc::new(SystemClock),
            runner: Arc::new(runner),
            journal: Mutex::new(journal),
            cache: Mutex::new(cache),
            parked: Mutex::new(parked),
            budget,
            staging,
            device_name: self.device_name.clone(),
            platform: std::env::consts::OS.to_string(),
            tuning: Tuning {
                poll: Duration::from_secs(self.poll_seconds),
                ..Tuning::default()
            },
        };
        Ok(backend)
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

    #[test]
    fn public_key_is_stable_base64url_of_32_bytes() {
        use base64::Engine;
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let a = c.public_key_b64().unwrap();
        assert_eq!(a, c.public_key_b64().unwrap(), "same key on reopen");
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&a)
            .unwrap();
        assert_eq!(raw.len(), 32);
        assert!(!a.contains('=') && !a.contains('+') && !a.contains('/'));
        let file = std::fs::read(c.key_path()).unwrap();
        assert!(!file.windows(a.len()).any(|w| w == a.as_bytes()));
    }

    fn cfg(dir: &Path) -> AgentConfig {
        serde_json::from_value(serde_json::json!({
            "coordinator_url": "https://coordinator.example",
            "data_dir": dir,
            "device_name": "pc",
            "worker_path": "nc-exec-worker-missing.exe",
            "network_budget": "unmetered",
            "probe": {"network": {"interface": "ethernet", "connection_id": "home", "bytes_per_second": 1000000}, "declared_thermal": "nominal"}
        }))
        .unwrap()
    }

    struct Counting(std::sync::atomic::AtomicUsize);
    impl Coordinator for Counting {
        fn register(
            &self,
            _: &nc_protocol::pb::v1::RegisterRequest,
        ) -> Result<(), crate::coord::CoordError> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
        fn heartbeat(
            &self,
            _: &nc_protocol::pb::v1::HeartbeatRequest,
        ) -> Result<(), crate::coord::CoordError> {
            self.0.fetch_add(100, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
        fn lease(
            &self,
        ) -> Result<Option<nc_protocol::pb::v1::LeaseResponse>, crate::coord::CoordError> {
            unimplemented!()
        }
        fn fail(
            &self,
            _: &nc_protocol::pb::v1::LeaseFailRequest,
        ) -> Result<nc_protocol::pb::v1::LeaseFailResponse, crate::coord::CoordError> {
            unimplemented!()
        }
        fn renew(
            &self,
            _: &nc_protocol::pb::v1::LeaseRenewRequest,
        ) -> Result<nc_protocol::pb::v1::LeaseRenewResponse, crate::coord::CoordError> {
            unimplemented!()
        }
        fn complete(
            &self,
            _: &nc_protocol::pb::v1::CompleteRequest,
        ) -> Result<nc_protocol::pb::v1::CompleteResponse, crate::coord::CoordError> {
            unimplemented!()
        }
        fn get_blob(
            &self,
            _: &str,
            _: u64,
        ) -> Result<Box<dyn std::io::Read + Send>, crate::coord::CoordError> {
            unimplemented!()
        }
        fn put_blob(&self, _: &str, _: &[u8]) -> Result<(), crate::coord::CoordError> {
            unimplemented!()
        }
        fn submit(
            &self,
            _: &nc_protocol::pb::v1::SubmitJobRequest,
        ) -> Result<nc_protocol::pb::v1::SubmitJobResponse, crate::coord::CoordError> {
            unimplemented!()
        }
        fn job_status(
            &self,
            _: &str,
        ) -> Result<nc_protocol::pb::v1::JobStatusResponse, crate::coord::CoordError> {
            unimplemented!()
        }
    }

    #[test]
    fn build_agent_with_uses_given_coordinator_and_skip_register_skips() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let inner = Arc::new(Counting(std::sync::atomic::AtomicUsize::new(0)));
        let backend = c.backend_with(inner.clone()).unwrap();
        assert!(dir.path().join("budget.sqlite").exists());
        assert!(dir.path().join("staging").is_dir());
        backend.coord.register(&Default::default()).unwrap();
        assert_eq!(inner.0.load(std::sync::atomic::Ordering::SeqCst), 1);
        let skip = crate::coord::SkipRegister(inner.clone());
        skip.register(&Default::default()).unwrap();
        assert_eq!(
            inner.0.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "register skipped"
        );
        skip.heartbeat(&Default::default()).unwrap();
        assert_eq!(
            inner.0.load(std::sync::atomic::Ordering::SeqCst),
            101,
            "heartbeat forwarded"
        );
        drop(backend);
        let _agent = c.build_agent_with(Arc::new(skip)).unwrap();
    }

    #[test]
    fn budget_ledger_option_moves_the_ledger_out_of_data_dir() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("agent");
        let ledger = root.path().join("kept").join("budget.sqlite");
        let mut c = cfg(&data);
        assert_eq!(c.budget_ledger_path(), data.join("budget.sqlite"));
        c.budget_ledger = Some(ledger.clone());
        assert_eq!(c.budget_ledger_path(), ledger);
        let inner = Arc::new(Counting(std::sync::atomic::AtomicUsize::new(0)));
        let backend = c.backend_with(inner).unwrap();
        assert!(ledger.exists(), "ledger at the configured path");
        assert!(
            !data.join("budget.sqlite").exists(),
            "no default ledger in data_dir"
        );
        drop(backend);
        // Also accepted from the JSON config file.
        let path = root.path().join("agent.json");
        let mut v = serde_json::to_value(&c).unwrap();
        v["budget_ledger"] = serde_json::json!(ledger);
        std::fs::write(&path, v.to_string()).unwrap();
        assert_eq!(
            AgentConfig::load(&path).unwrap().budget_ledger,
            Some(ledger)
        );
    }
}
