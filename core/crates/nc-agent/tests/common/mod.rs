#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]
//! Test kit: an in-process fake coordinator, fake runners, fixed and stepping clocks, signed test jobs.

use chrono::{DateTime, TimeZone, Utc};
use ed25519_dalek::{Signer as _, SigningKey};
use nc_agent::budget::{BudgetGate, NetworkBudget};
use nc_agent::coord::{CoordError, Coordinator};
use nc_agent::journal::Journal;
use nc_agent::probe::{Clock, DeviceObservation, FakeProbe};
use nc_agent::requester::{self, JobSpec};
use nc_agent::runner::JobRunner;
use nc_agent::transfer::BlobCache;
use nc_agent::{Agent, Backend, Tuning};
use nc_exec::{CancelToken, ExecError, Outcome, Termination};
use nc_policy::{OwnerPolicy, PeerKey, ThermalLevel, WorkingWindow};
use nc_protocol::pb::v1::*;
use nc_tariff::{InterfaceClass, NetworkObservation};
use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct TestKey(pub SigningKey);
impl nc_protocol::Signer for TestKey {
    fn public_key(&self) -> [u8; 32] {
        self.0.verifying_key().to_bytes()
    }
    fn sign(&self, msg: &[u8]) -> Result<[u8; 64], nc_protocol::ProtocolError> {
        Ok(self.0.sign(msg).to_bytes())
    }
}
impl nc_identity::DeviceKeyStore for TestKey {
    fn assurance(&self) -> nc_identity::Assurance {
        nc_identity::Assurance::Software
    }
    fn public_key(&self) -> [u8; 32] {
        self.0.verifying_key().to_bytes()
    }
    fn sign(&self, msg: &[u8]) -> Result<[u8; 64], nc_identity::IdentityError> {
        Ok(self.0.sign(msg).to_bytes())
    }
}

pub fn key(seed: u8) -> TestKey {
    TestKey(SigningKey::from_bytes(&[seed; 32]))
}

/// 2026-11-02 12:00:00 UTC.
pub fn noon() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 2, 12, 0, 0).unwrap()
}

pub struct FixedClock(pub DateTime<Utc>);
impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

/// Advances by `step` on every read.
pub struct StepClock {
    pub t: Mutex<DateTime<Utc>>,
    pub step: chrono::Duration,
}
impl Clock for StepClock {
    fn now(&self) -> DateTime<Utc> {
        let mut t = self.t.lock().unwrap();
        let now = *t;
        *t += self.step;
        now
    }
}

pub fn home() -> NetworkObservation {
    NetworkObservation {
        interface: InterfaceClass::Ethernet,
        connection_id: Some("home".into()),
        bytes_per_second: 10_000_000,
    }
}

pub fn cellular() -> NetworkObservation {
    NetworkObservation {
        interface: InterfaceClass::Cellular,
        connection_id: Some("sim".into()),
        bytes_per_second: 10_000_000,
    }
}

pub fn good_device() -> DeviceObservation {
    DeviceObservation {
        on_ac_power: true,
        battery_percent: None,
        thermal_level: Some(ThermalLevel::Nominal),
        user_idle_seconds: Some(3600),
    }
}

pub fn policy_trusting(publisher: [u8; 32]) -> OwnerPolicy {
    use chrono::{NaiveTime, Weekday::*};
    OwnerPolicy {
        accept_jobs: true,
        charging_only: false,
        min_battery_percent: 0,
        pause_on_user_activity: false,
        max_memory_mb: 512,
        max_disk_mb: 512,
        // Two halves cover the whole day (a single window cannot have equal endpoints).
        time_windows: vec![
            WorkingWindow {
                days: vec![Mon, Tue, Wed, Thu, Fri, Sat, Sun],
                start: NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
                end: NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
            },
            WorkingWindow {
                days: vec![Mon, Tue, Wed, Thu, Fri, Sat, Sun],
                start: NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
                end: NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
            },
        ],
        trusted_peers: vec![PeerKey::from_bytes(publisher)],
        allowed_execution_profiles: vec!["cpu-core-v1".into()],
        ..OwnerPolicy::default()
    }
}

/// A metered profile: window 10:00-14:00 UTC every day, `remaining` bytes per month, no reserve.
pub fn metered(remaining: u64) -> nc_tariff::TariffProfile {
    use chrono::{NaiveTime, Weekday::*};
    nc_tariff::TariffProfile {
        profile_id: "test".into(),
        version: 1,
        timezone: chrono_tz::UTC,
        binding: nc_tariff::ConnectionBinding {
            interface: InterfaceClass::Ethernet,
            connection_id: "home".into(),
        },
        windows: vec![nc_tariff::Window {
            days: vec![Mon, Tue, Wed, Thu, Fri, Sat, Sun],
            start: NaiveTime::from_hms_opt(10, 0, 0).unwrap(),
            end: NaiveTime::from_hms_opt(14, 0, 0).unwrap(),
            marginal_price_per_gb: 0.0,
            max_bytes: u64::MAX / 4,
        }],
        outside_window: nc_tariff::OutsideWindowRule::Block,
        billing: nc_tariff::Billing {
            reset_rule: nc_tariff::ResetRule { day_of_month: 1 },
            remaining_bytes: remaining,
            safety_reserve_percent: 0,
        },
        policy: nc_tariff::Policy {
            allow_cellular_fallback: false,
            boundary_guard_seconds: 0,
            strict_zero_outside_window: false,
        },
    }
}

#[derive(Default)]
pub struct FakeState {
    pub leases: VecDeque<LeaseResponse>,
    pub blobs: HashMap<String, Vec<u8>>,
    pub calls: Vec<String>,
    pub fails: Vec<LeaseFailRequest>,
    pub completes: Vec<CompleteRequest>,
    pub puts: HashMap<String, Vec<u8>>,
    /// Paths whose first GET is cut after this many bytes.
    pub cut_once: HashMap<String, usize>,
    /// Bodies served for a path regardless of the stored blob (to test overrun).
    pub override_body: HashMap<String, Vec<u8>>,
    /// Number of upcoming Complete calls that are recorded and then answered with a network error.
    pub complete_lost: usize,
    pub committed: HashMap<String, CompleteRequest>,
}

#[derive(Default)]
pub struct FakeCoord {
    pub s: Mutex<FakeState>,
}

impl FakeCoord {
    pub fn calls(&self) -> Vec<String> {
        self.s.lock().unwrap().calls.clone()
    }
    pub fn count(&self, prefix: &str) -> usize {
        self.calls()
            .iter()
            .filter(|c| c.starts_with(prefix))
            .count()
    }
    pub fn fails(&self) -> Vec<LeaseFailRequest> {
        self.s.lock().unwrap().fails.clone()
    }
}

/// Reader that yields `data` then fails (a dropped connection).
struct Cut {
    data: std::io::Cursor<Vec<u8>>,
}
impl Read for Cut {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.data.read(buf)?;
        if n == 0 {
            return Err(std::io::Error::other("connection reset"));
        }
        Ok(n)
    }
}

impl Coordinator for FakeCoord {
    fn register(&self, _r: &RegisterRequest) -> Result<(), CoordError> {
        self.s.lock().unwrap().calls.push("register".into());
        Ok(())
    }
    fn heartbeat(&self, _r: &HeartbeatRequest) -> Result<(), CoordError> {
        self.s.lock().unwrap().calls.push("heartbeat".into());
        Ok(())
    }
    fn lease(&self) -> Result<Option<LeaseResponse>, CoordError> {
        let mut s = self.s.lock().unwrap();
        s.calls.push("lease".into());
        Ok(s.leases.pop_front())
    }
    fn fail(&self, r: &LeaseFailRequest) -> Result<LeaseFailResponse, CoordError> {
        let mut s = self.s.lock().unwrap();
        s.calls.push(format!(
            "fail:{}:{}",
            r.reason,
            r.code.clone().unwrap_or_default()
        ));
        s.fails.push(r.clone());
        Ok(LeaseFailResponse {
            task_id: String::new(),
            state: "queued".into(),
            duplicate: false,
        })
    }
    fn complete(&self, r: &CompleteRequest) -> Result<CompleteResponse, CoordError> {
        let mut s = self.s.lock().unwrap();
        s.calls.push("complete".into());
        s.completes.push(r.clone());
        let duplicate = s.committed.contains_key(&r.attempt_id);
        s.committed.insert(r.attempt_id.clone(), r.clone());
        if s.complete_lost > 0 {
            s.complete_lost -= 1;
            return Err(CoordError::Network("reset".into()));
        }
        Ok(CompleteResponse {
            task_id: uuid::Uuid::nil().to_string(),
            state: "committed".into(),
            duplicate,
        })
    }
    fn get_blob(&self, path: &str, offset: u64) -> Result<Box<dyn Read + Send>, CoordError> {
        let mut s = self.s.lock().unwrap();
        s.calls.push(format!("get:{path}@{offset}"));
        let body = match s.override_body.get(path) {
            Some(b) => b.clone(),
            None => s.blobs.get(path).cloned().ok_or(CoordError::Status {
                status: 404,
                code: "not_found".into(),
            })?,
        };
        let rest = body[usize::try_from(offset).unwrap().min(body.len())..].to_vec();
        if let Some(n) = s.cut_once.remove(path) {
            return Ok(Box::new(Cut {
                data: std::io::Cursor::new(rest[..n.min(rest.len())].to_vec()),
            }));
        }
        Ok(Box::new(std::io::Cursor::new(rest)))
    }
    fn put_blob(&self, h: &str, bytes: &[u8]) -> Result<(), CoordError> {
        let mut s = self.s.lock().unwrap();
        s.calls.push(format!("put:{h}"));
        s.puts.insert(h.into(), bytes.to_vec());
        Ok(())
    }
    fn submit(&self, _r: &SubmitJobRequest) -> Result<SubmitJobResponse, CoordError> {
        Err(CoordError::Decode)
    }
    fn job_status(&self, _id: &str) -> Result<JobStatusResponse, CoordError> {
        Err(CoordError::Decode)
    }
}

/// Runner modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Output = input reversed.
    Reverse,
    /// Spin until cancelled (an infinite-loop job).
    UntilCancel,
}

pub struct FakeRunner {
    pub mode: Mode,
    pub runs: AtomicUsize,
    pub cancelled_at: Mutex<Option<Instant>>,
    pub seen_input: Mutex<Vec<u8>>,
}

impl FakeRunner {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            runs: AtomicUsize::new(0),
            cancelled_at: Mutex::new(None),
            seen_input: Mutex::new(Vec::new()),
        }
    }
}

impl JobRunner for FakeRunner {
    fn run(
        &self,
        task: &TaskManifest,
        publishers: &[[u8; 32]],
        module: Vec<u8>,
        input: Vec<u8>,
        cancel: &CancelToken,
    ) -> Result<Outcome, ExecError> {
        // Same refusals the real executor makes before running anything.
        if nc_exec::sha256(&module).as_slice() != task.module_sha256.as_slice() {
            return Err(ExecError::HashMismatch);
        }
        let pk: [u8; 32] = task.publisher_key.as_slice().try_into().unwrap();
        if !publishers.contains(&pk) {
            return Err(ExecError::SignatureInvalid("untrusted".into()));
        }
        self.runs.fetch_add(1, Ordering::SeqCst);
        *self.seen_input.lock().unwrap() = input.clone();
        let termination = match self.mode {
            Mode::Reverse => Termination::Exited(0),
            Mode::UntilCancel => {
                while !cancel.is_cancelled() {
                    std::thread::sleep(Duration::from_millis(1));
                }
                *self.cancelled_at.lock().unwrap() = Some(Instant::now());
                Termination::Cancelled
            }
        };
        let mut output = input;
        output.reverse();
        Ok(Outcome {
            termination,
            output,
            checkpoint: Vec::new(),
            fuel_used: 1,
            peak_memory_bytes: 0,
            timings: Default::default(),
        })
    }
}

pub struct Kit {
    pub coord: Arc<FakeCoord>,
    pub runner: Arc<FakeRunner>,
    pub probe: Arc<FakeProbe>,
    pub publisher: TestKey,
    pub dir: tempfile::TempDir,
}

impl Kit {
    pub fn new(mode: Mode) -> Self {
        Self {
            coord: Arc::new(FakeCoord::default()),
            runner: Arc::new(FakeRunner::new(mode)),
            probe: Arc::new(FakeProbe::new(good_device(), home())),
            publisher: key(42),
            dir: tempfile::tempdir().unwrap(),
        }
    }

    pub fn journal(&self) -> Journal {
        Journal::open(&self.dir.path().join("journal")).unwrap()
    }

    pub fn agent_with(&self, budget: NetworkBudget, clock: Arc<dyn Clock>) -> Agent {
        let staging = self.dir.path().join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        Agent::with_backend(Backend {
            coord: self.coord.clone(),
            probe: self.probe.clone(),
            clock: clock.clone(),
            runner: self.runner.clone(),
            journal: Mutex::new(self.journal()),
            cache: Mutex::new(BlobCache::open(&self.dir.path().join("blobs"), 64 << 20).unwrap()),
            budget: BudgetGate::new(&budget, clock.now()).unwrap(),
            staging,
            device_name: "test".into(),
            platform: "windows".into(),
            tuning: Tuning {
                poll: Duration::from_millis(20),
                max_backoff: Duration::from_millis(100),
                monitor_tick: Duration::from_millis(10),
            },
        })
    }

    pub fn agent(&self) -> Agent {
        self.agent_with(NetworkBudget::Unmetered, Arc::new(FixedClock(noon())))
    }

    /// Build a signed job and queue its lease; blobs are served by the fake. Returns the lease.
    pub fn queue_job(&self, module: &[u8], input: &[u8]) -> LeaseResponse {
        let built = requester::build(
            &JobSpec::new(module.to_vec(), input.to_vec()),
            &self.publisher,
            noon().timestamp_millis(),
        )
        .unwrap();
        let mut transfers = Vec::new();
        let mut s = self.coord.s.lock().unwrap();
        for (hash, bytes) in &built.blobs {
            let path = format!("/v1/blobs/{hash}");
            s.blobs.insert(path.clone(), bytes.clone());
            transfers.push(TransferRef {
                sha256: hash.clone(),
                size: bytes.len() as u64,
                path,
            });
        }
        #[allow(deprecated)]
        let lease = LeaseResponse {
            lease_id: random_uuid(),
            task_id: built.job_id.clone(),
            attempt_id: random_uuid(),
            expires_at: 0,
            input: String::new(),
            manifest: Some(built.signed),
            transfers,
            version: 2,
        };
        s.leases.push_back(lease.clone());
        lease
    }

    pub fn policy(&self) -> OwnerPolicy {
        policy_trusting(nc_protocol::Signer::public_key(&self.publisher))
    }
}

/// Wait until `f` holds (5 s max).
pub fn until(mut f: impl FnMut() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(
            t.elapsed() < Duration::from_secs(5),
            "condition not reached"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

pub fn staging_parts(dir: &Path) -> usize {
    std::fs::read_dir(dir.join("staging"))
        .map(|r| r.count())
        .unwrap_or(0)
}

pub fn random_uuid() -> String {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).unwrap();
    uuid::Builder::from_random_bytes(b).into_uuid().to_string()
}
