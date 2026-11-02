//! Coordinator enrolment and the coordinator-backed agent.
//!
//! Enrolment follows the server contract: one signed `POST /v1/devices/register` carrying the owner's
//! enrolment secret in `x-nc-enroll`. The secret is used for that one call and then dropped: it is never
//! written to disk, logged or returned to Dart. The device key lives in nc-identity's key store under
//! `<app dir>/agent/device.key` and never crosses the bridge.
//!
//! The saved file `<app dir>/coordinator.json` holds only the URL, the device name and the owner's network
//! declaration. On the next start [`build_agent`] makes `Agent::with_backend` from it.

use crate::api::{EnrolRequest, Enrolment, NetworkKind, NightDataSettings};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use nc_agent::budget::BudgetGate;
use nc_agent::coord::{CoordError, Coordinator, HttpCoordinator};
use nc_agent::journal::Journal;
use nc_agent::probe::{DeclaredNetwork, ProbeConfig, SystemClock, SystemProbe};
use nc_agent::runner::SandboxRunner;
use nc_agent::transfer::BlobCache;
use nc_agent::{Agent, Backend, NetworkBudget, Tuning};
use nc_identity::{DeviceKeyStore, SoftwareKeyStore};
use nc_policy::{OwnerPolicy, PeerKey, ThermalLevel};
use nc_protocol::pb::v1::{
    CompleteRequest, CompleteResponse, HeartbeatRequest, JobStatusResponse, LeaseFailRequest,
    LeaseFailResponse, LeaseRenewRequest, LeaseRenewResponse, LeaseResponse, RegisterRequest,
    SubmitJobRequest, SubmitJobResponse,
};
use nc_protocol::PROFILE_CPU_CORE_V1;
use nc_tariff::{ConnectionBinding, InterfaceClass};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{Read, Write};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub(crate) const CONFIG_NAME: &str = "coordinator.json";
const CONFIG_TMP: &str = "coordinator.json.tmp";
const AGENT_DIR: &str = "agent";
const FORMAT: u32 = 1;
const MAX_FILE_BYTES: u64 = 64 * 1024;
const CACHE_QUOTA_BYTES: u64 = 512 * 1024 * 1024;
const POLL: Duration = Duration::from_secs(10);
/// Set to `1` to allow `http://` to a loopback or private-LAN test coordinator. Never set in releases.
pub(crate) const DEV_HTTP_ENV: &str = "NC_DEV_ALLOW_HTTP";
/// Optional override for the sandbox worker path (default: next to the app executable).
const WORKER_ENV: &str = "NC_EXEC_WORKER";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EnrolFile {
    format: u32,
    pub(crate) coordinator_url: String,
    pub(crate) device_name: String,
    pub(crate) network: DeclaredNetwork,
    pub(crate) unlimited_data: bool,
    pub(crate) declared_thermal: Option<ThermalLevel>,
    /// Owner-trusted job publisher keys (32-byte Ed25519, base64url without padding).
    pub(crate) trusted_publishers: Vec<String>,
}

impl EnrolFile {
    pub(crate) fn summary(&self, dir: &Path, active: bool) -> Enrolment {
        Enrolment {
            coordinator_url: self.coordinator_url.clone(),
            device_name: self.device_name.clone(),
            unlimited_data: self.unlimited_data,
            trusted_publishers: u32::try_from(self.trusted_publishers.len()).unwrap_or(u32::MAX),
            device_key: public_key(dir).unwrap_or_default(),
            active,
        }
    }

    /// Adds the enrolment's trust to the owner's limits: only the listed publishers, only cpu-core-v1.
    pub(crate) fn apply_trust(&self, mut p: OwnerPolicy) -> OwnerPolicy {
        for k in &self.trusted_publishers {
            if let Ok(key) = parse_key(k) {
                let key = PeerKey::from_bytes(key);
                if !p.trusted_peers.contains(&key) {
                    p.trusted_peers.push(key);
                }
            }
        }
        if !p.trusted_peers.is_empty()
            && !p
                .allowed_execution_profiles
                .iter()
                .any(|x| x == PROFILE_CPU_CORE_V1)
        {
            p.allowed_execution_profiles
                .push(PROFILE_CPU_CORE_V1.to_string());
        }
        p
    }

    /// The binding a metered night-data profile must use for transfers on this connection.
    pub(crate) fn binding(&self) -> ConnectionBinding {
        ConnectionBinding {
            interface: self.network.interface,
            connection_id: self.network.connection_id.clone(),
        }
    }
}

/// A publisher key: 32 bytes as base64url (no padding, as in `x-nc-key`) or 64 hex characters.
pub(crate) fn parse_key(s: &str) -> Result<[u8; 32], String> {
    let s = s.trim();
    let bytes = if s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        (0..32)
            .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16))
            .collect::<Result<Vec<u8>, _>>()
            .ok()
    } else {
        URL_SAFE_NO_PAD.decode(s).ok()
    };
    bytes
        .and_then(|b| <[u8; 32]>::try_from(b).ok())
        .ok_or_else(|| "A trusted publisher key is not valid.".to_string())
}

/// This device's public key, base64url. Public only; the private key never leaves the key store.
fn public_key(dir: &Path) -> Option<String> {
    let p = agent_dir(dir).join("device.key");
    if !p.exists() {
        return None;
    }
    let k = SoftwareKeyStore::open_or_create(p).ok()?;
    Some(URL_SAFE_NO_PAD.encode(k.public_key()))
}

pub(crate) fn dev_http_allowed() -> bool {
    std::env::var(DEV_HTTP_ENV).is_ok_and(|v| v == "1")
}

fn dev_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
    {
        Ok(IpAddr::V4(a)) => a.is_loopback() || a.is_private() || a.is_link_local(),
        Ok(IpAddr::V6(a)) => a.is_loopback() || (a.segments()[0] & 0xfe00) == 0xfc00,
        Err(_) => false,
    }
}

/// Checks a coordinator base URL. HTTPS always; plain HTTP only for loopback / private-LAN hosts and only
/// when `dev` is set. Returns the normalised URL (no trailing slash).
pub(crate) fn check_url(url: &str, dev: bool) -> Result<String, String> {
    let url = url.trim();
    let bad = || "Enter the coordinator address, like https://coordinator.example.".to_string();
    let (scheme, rest) = url.split_once("://").ok_or_else(bad)?;
    let scheme = scheme.to_ascii_lowercase();
    let authority = rest.strip_suffix('/').unwrap_or(rest);
    if authority.is_empty()
        || authority.chars().any(|c| {
            matches!(c, '/' | '?' | '#' | '@' | '\\') || c.is_whitespace() || c.is_control()
        })
    {
        return Err(bad());
    }
    // Host without the port (IPv6 literals are bracketed).
    let host = if let Some(end) = authority.find(']') {
        &authority[..=end]
    } else {
        authority.split(':').next().unwrap_or_default()
    };
    if host.is_empty() {
        return Err(bad());
    }
    match scheme.as_str() {
        "https" => Ok(format!("https://{authority}")),
        "http" if dev && dev_host(host) => Ok(format!("http://{authority}")),
        "http" => Err(
            "Use an https:// address. Plain http:// is only for a local test coordinator in developer mode."
                .into(),
        ),
        _ => Err(bad()),
    }
}

fn agent_dir(dir: &Path) -> PathBuf {
    dir.join(AGENT_DIR)
}

fn open_key(dir: &Path) -> Result<Arc<dyn DeviceKeyStore>, String> {
    let d = agent_dir(dir);
    fs::create_dir_all(&d).map_err(|_| "Could not create the app data folder.".to_string())?;
    let k = SoftwareKeyStore::open_or_create(d.join("device.key"))
        .map_err(|_| "Could not open this device's key.".to_string())?;
    Ok(Arc::new(k))
}

fn plain(e: &CoordError) -> String {
    match e {
        CoordError::Network(_) => {
            "Cannot reach the coordinator. Check the address and your connection.".into()
        }
        CoordError::Status { status: 401, .. } => {
            "The coordinator did not accept the enrolment secret.".into()
        }
        CoordError::Status { status: 400, .. } => {
            "The coordinator did not accept this device name.".into()
        }
        CoordError::Status { status, .. } => {
            format!("The coordinator refused enrolment (error {status}).")
        }
        CoordError::Decode => "The coordinator sent an answer this app does not understand.".into(),
        CoordError::Sign => "Could not sign the enrolment request.".into(),
    }
}

/// Validates the request, registers the device key with the coordinator using the enrolment secret, then
/// saves the config (without the secret). `dev` allows plain HTTP to a local test coordinator.
pub(crate) fn enrol(dir: &Path, req: EnrolRequest, dev: bool) -> Result<Enrolment, String> {
    let url = check_url(&req.coordinator_url, dev)?;
    let name = req.device_name.trim().to_string();
    let n = name.chars().count();
    if n == 0 || n > 64 || name.chars().any(char::is_control) {
        return Err("Device name must be 1 to 64 characters.".into());
    }
    let secret = req.enrol_secret;
    if secret.trim().is_empty() {
        return Err("Enter the enrolment secret from the coordinator owner.".into());
    }
    let connection = req.connection_name.trim().to_string();
    if connection.is_empty() || connection.chars().count() > 64 {
        return Err("Give this connection a short name, like home.".into());
    }
    if req.speed_mbps == 0 {
        return Err("Connection speed must be more than 0 Mbit/s.".into());
    }
    if req.trusted_publishers.is_empty() || req.trusted_publishers.len() > 16 {
        return Err("Add 1 to 16 trusted publisher keys, one per line.".into());
    }
    let mut trusted = Vec::new();
    for k in &req.trusted_publishers {
        let key = URL_SAFE_NO_PAD.encode(parse_key(k)?);
        if !trusted.contains(&key) {
            trusted.push(key);
        }
    }
    let interface = match req.network {
        NetworkKind::Ethernet => InterfaceClass::Ethernet,
        NetworkKind::Wifi => InterfaceClass::Wifi,
    };
    let file = EnrolFile {
        format: FORMAT,
        coordinator_url: url.clone(),
        device_name: name.clone(),
        network: DeclaredNetwork {
            interface,
            connection_id: connection,
            // Mbit/s -> bytes/s, conservative (no rounding up).
            bytes_per_second: u64::from(req.speed_mbps) * 125_000,
        },
        unlimited_data: req.unlimited_data,
        declared_thermal: req.cool_device.then_some(ThermalLevel::Nominal),
        trusted_publishers: trusted,
    };
    let key = open_key(dir)?;
    // The secret goes to the coordinator client for this one register call and is dropped with it.
    let coord = HttpCoordinator::new(&url, key, Some(secret));
    coord
        .register(&RegisterRequest {
            name,
            platform: std::env::consts::OS.into(),
        })
        .map_err(|e| plain(&e))?;
    drop(coord);
    save(dir, &file)?;
    Ok(file.summary(dir, false))
}

fn save(dir: &Path, file: &EnrolFile) -> Result<(), String> {
    let fail = || "Could not save the coordinator settings.".to_string();
    let bytes = serde_json::to_vec_pretty(file).map_err(|_| fail())?;
    let tmp = dir.join(CONFIG_TMP);
    let mut f = fs::File::create(&tmp).map_err(|_| fail())?;
    f.write_all(&bytes).map_err(|_| fail())?;
    f.sync_all().map_err(|_| fail())?;
    drop(f);
    fs::rename(&tmp, dir.join(CONFIG_NAME)).map_err(|_| fail())
}

/// Reads `coordinator.json`. `Ok(None)` when not enrolled. The URL is checked again on every load.
pub(crate) fn load(dir: &Path, dev: bool) -> Result<Option<EnrolFile>, String> {
    let p = dir.join(CONFIG_NAME);
    let bad = || "The saved coordinator settings could not be used. Enrol again.".to_string();
    let meta = match fs::metadata(&p) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(bad()),
    };
    if meta.len() > MAX_FILE_BYTES {
        return Err(bad());
    }
    let mut bytes = Vec::new();
    fs::File::open(&p)
        .and_then(|mut f| f.read_to_end(&mut bytes))
        .map_err(|_| bad())?;
    let f: EnrolFile = serde_json::from_slice(&bytes).map_err(|_| bad())?;
    if f.format != FORMAT {
        return Err(bad());
    }
    check_url(&f.coordinator_url, dev)?;
    for k in &f.trusted_publishers {
        parse_key(k).map_err(|_| bad())?;
    }
    Ok(Some(f))
}

/// Removes the coordinator config and the device identity, journal and blob cache. The data budget ledger
/// (`budget.sqlite`) is kept so a reset cannot be used to spend the night budget twice.
pub(crate) fn unenrol(dir: &Path) -> Result<(), String> {
    let fail = || "Could not remove the coordinator settings.".to_string();
    for name in [CONFIG_NAME, CONFIG_TMP] {
        match fs::remove_file(dir.join(name)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(fail()),
        }
    }
    match fs::remove_dir_all(agent_dir(dir)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(fail()),
    }
}

/// The coordinator client used after enrolment. The server only accepts `register` with the enrolment
/// secret, which is not stored, so `register` is a no-op here: the device is already registered, and the
/// signed heartbeat that follows fails with 401 if the coordinator no longer knows this key.
struct Enrolled(HttpCoordinator);

impl Coordinator for Enrolled {
    fn register(&self, _req: &RegisterRequest) -> Result<(), CoordError> {
        Ok(())
    }
    fn heartbeat(&self, req: &HeartbeatRequest) -> Result<(), CoordError> {
        self.0.heartbeat(req)
    }
    fn lease(&self) -> Result<Option<LeaseResponse>, CoordError> {
        self.0.lease()
    }
    fn fail(&self, req: &LeaseFailRequest) -> Result<LeaseFailResponse, CoordError> {
        self.0.fail(req)
    }
    fn renew(&self, req: &LeaseRenewRequest) -> Result<LeaseRenewResponse, CoordError> {
        self.0.renew(req)
    }
    fn complete(&self, req: &CompleteRequest) -> Result<CompleteResponse, CoordError> {
        self.0.complete(req)
    }
    fn get_blob(&self, path: &str, offset: u64) -> Result<Box<dyn Read + Send>, CoordError> {
        self.0.get_blob(path, offset)
    }
    fn put_blob(&self, sha256_hex: &str, bytes: &[u8]) -> Result<(), CoordError> {
        self.0.put_blob(sha256_hex, bytes)
    }
    fn submit(&self, req: &SubmitJobRequest) -> Result<SubmitJobResponse, CoordError> {
        self.0.submit(req)
    }
    fn job_status(&self, job_id: &str) -> Result<JobStatusResponse, CoordError> {
        self.0.job_status(job_id)
    }
}

fn worker_path() -> PathBuf {
    if let Some(p) = std::env::var_os(WORKER_ENV).filter(|v| !v.is_empty()) {
        return PathBuf::from(p);
    }
    let exe = if cfg!(windows) {
        "nc-exec-worker.exe"
    } else {
        "nc-exec-worker"
    };
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(exe)))
        .unwrap_or_else(|| PathBuf::from(exe))
}

/// The network budget for this connection: unmetered, or the night-data profile bound to it.
pub(crate) fn budget(file: &EnrolFile, night: &NightDataSettings) -> Result<NetworkBudget, String> {
    if file.unlimited_data {
        return Ok(NetworkBudget::Unmetered);
    }
    crate::night::to_profile_bound(night, file.binding()).map(NetworkBudget::Metered)
}

/// Builds `Agent::with_backend` the way `nc_agent::config::AgentConfig::build_agent` does, with the app's
/// paths and the [`Enrolled`] coordinator client.
pub(crate) fn build_agent(
    dir: &Path,
    file: &EnrolFile,
    budget: &NetworkBudget,
) -> Result<Agent, String> {
    let fail = |what: &str| format!("Could not open the {what}.");
    let key = open_key(dir)?;
    let data = agent_dir(dir);
    let runner = SandboxRunner::new(worker_path());
    // Not fatal: without the sandbox every job is refused (SANDBOX_UNAVAILABLE), never run weaker.
    let _ = runner.prepare();
    let staging = data.join("staging");
    fs::create_dir_all(&staging).map_err(|_| fail("staging folder"))?;
    let journal = Journal::open(&data.join("journal")).map_err(|_| fail("job journal"))?;
    let cache =
        BlobCache::open(&data.join("blobs"), CACHE_QUOTA_BYTES).map_err(|_| fail("blob cache"))?;
    let usage =
        nc_store::Store::open(&dir.join("budget.sqlite")).map_err(|_| fail("data budget"))?;
    let budget = BudgetGate::with_usage_store(budget, chrono::Utc::now(), usage)
        .map_err(|_| fail("data budget"))?;
    let probe = ProbeConfig {
        network: Some(file.network.clone()),
        declared_thermal: file.declared_thermal,
    };
    let backend = Backend {
        coord: Arc::new(Enrolled(HttpCoordinator::new(
            &file.coordinator_url,
            key,
            None,
        ))),
        probe: Arc::new(SystemProbe::new(probe)),
        clock: Arc::new(SystemClock),
        runner: Arc::new(runner),
        journal: Mutex::new(journal),
        cache: Mutex::new(cache),
        budget,
        staging,
        device_name: file.device_name.clone(),
        platform: std::env::consts::OS.to_string(),
        tuning: Tuning {
            poll: POLL,
            ..Tuning::default()
        },
    };
    Ok(Agent::with_backend(backend))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
