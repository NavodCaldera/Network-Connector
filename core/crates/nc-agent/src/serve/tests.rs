use super::*;
use crate::budget::{BudgetGate, NetworkBudget};
use crate::coord::CoordError;
use crate::peer::{fetch, LeasePeer, PeerNet};
use crate::probe::{Clock, DeviceObservation, FakeProbe};
use crate::transfer::Ctx;
use chrono::{DateTime, Utc};
use nc_net::Multiaddr;
use nc_protocol::pb::v1::{
    CompleteRequest, CompleteResponse, HeartbeatRequest, JobLease, JobStatusResponse,
    LeaseFailRequest, LeaseFailResponse, LeaseRenewRequest, LeaseRenewResponse, LeaseResponse,
    PeerHints, PeerRegisterRequest, PeerRegisterResponse, RegisterRequest, SubmitJobRequest,
    SubmitJobResponse, TaskStatus, TransferRef,
};
use nc_tariff::{NetworkObservation, TrafficClass};
use std::io::Read;
use std::sync::atomic::AtomicUsize;
use std::sync::Mutex;

const JOB: &str = "6f1c1f5e-3b7a-4c55-9d2e-1a2b3c4d5e6f";

/// Fake coordinator: scripted lease list and job state, counts HTTP blob gets and lease-list calls.
struct Fake {
    leases: Mutex<JobLeasesResponse>,
    state: Mutex<String>,
    list_status: Option<u16>,
    lists: AtomicUsize,
    gets: AtomicUsize,
    regs: AtomicUsize,
    blob: Vec<u8>,
}

impl Fake {
    fn new(blob: &[u8]) -> Self {
        Self {
            leases: Mutex::new(JobLeasesResponse {
                job_id: JOB.into(),
                leases: Vec::new(),
            }),
            state: Mutex::new("queued".into()),
            list_status: None,
            lists: AtomicUsize::new(0),
            gets: AtomicUsize::new(0),
            regs: AtomicUsize::new(0),
            blob: blob.to_vec(),
        }
    }
    fn set(&self, leases: Vec<JobLease>) {
        self.leases.lock().unwrap().leases = leases;
    }
}

fn nope<T>() -> Result<T, CoordError> {
    Err(CoordError::Network("fake".into()))
}

impl Coordinator for Fake {
    fn register(&self, _: &RegisterRequest) -> Result<(), CoordError> {
        Ok(())
    }
    fn heartbeat(&self, _: &HeartbeatRequest) -> Result<(), CoordError> {
        Ok(())
    }
    fn lease(&self) -> Result<Option<LeaseResponse>, CoordError> {
        Ok(None)
    }
    fn fail(&self, _: &LeaseFailRequest) -> Result<LeaseFailResponse, CoordError> {
        nope()
    }
    fn renew(&self, _: &LeaseRenewRequest) -> Result<LeaseRenewResponse, CoordError> {
        nope()
    }
    fn complete(&self, _: &CompleteRequest) -> Result<CompleteResponse, CoordError> {
        nope()
    }
    fn get_blob(&self, _: &str, offset: u64) -> Result<Box<dyn Read + Send>, CoordError> {
        self.gets.fetch_add(1, Ordering::SeqCst);
        let o = usize::try_from(offset).unwrap();
        Ok(Box::new(std::io::Cursor::new(self.blob[o..].to_vec())))
    }
    fn put_blob(&self, _: &str, _: &[u8]) -> Result<(), CoordError> {
        nope()
    }
    fn register_peer(&self, req: &PeerRegisterRequest) -> Result<PeerRegisterResponse, CoordError> {
        self.regs.fetch_add(1, Ordering::SeqCst);
        Ok(PeerRegisterResponse {
            peer_id: req.peer_id.clone(),
            expires_at: 0,
        })
    }
    fn submit(&self, _: &SubmitJobRequest) -> Result<SubmitJobResponse, CoordError> {
        nope()
    }
    fn job_status(&self, _: &str) -> Result<JobStatusResponse, CoordError> {
        Ok(JobStatusResponse {
            tasks: vec![TaskStatus {
                state: self.state.lock().unwrap().clone(),
                ..Default::default()
            }],
            ..Default::default()
        })
    }
    fn job_leases(&self, _: &str) -> Result<JobLeasesResponse, CoordError> {
        self.lists.fetch_add(1, Ordering::SeqCst);
        if let Some(status) = self.list_status {
            return Err(CoordError::Status {
                status,
                code: "not_found".into(),
            });
        }
        Ok(self.leases.lock().unwrap().clone())
    }
}

const NOW: u64 = 1_800_000_000;

fn lease(helper: Option<&PeerId>, expires_at: u64) -> JobLease {
    JobLease {
        lease_id: "l".into(),
        attempt_id: "a".into(),
        helper_peer_id: helper.map(ToString::to_string).unwrap_or_default(),
        expires_at,
    }
}

fn resp(leases: Vec<JobLease>) -> JobLeasesResponse {
    JobLeasesResponse {
        job_id: JOB.into(),
        leases,
    }
}

fn loopback() -> Vec<Multiaddr> {
    vec!["/ip4/127.0.0.1/tcp/0".parse().unwrap()]
}

fn sha(b: &[u8]) -> [u8; 32] {
    Sha256::digest(b).into()
}

fn other_peer() -> PeerId {
    nc_net::Keypair::generate_ed25519().public().to_peer_id()
}

#[test]
fn grants_exactly_the_listed_helper_and_only_this_jobs_blobs() {
    let net = PeerNet::start(loopback()).unwrap();
    let (helper, stranger) = (other_peer(), other_peer());
    // Another job's grant to the same helper must survive this job's revoke.
    net.grant(helper, vec![b"other job".to_vec()]);
    let mut g = JobGrants::new(&net, vec![b"module".to_vec(), b"input".to_vec()]);
    g.apply(JOB, &resp(vec![lease(Some(&helper), NOW + 60)]), NOW);
    assert_eq!(g.granted(), vec![helper]);
    assert_eq!(net.granted_peers(), vec![helper]);
    let mut blobs = net.granted_blobs(&helper);
    blobs.sort_unstable();
    let mut want = vec![sha(b"module"), sha(b"input"), sha(b"other job")];
    want.sort_unstable();
    assert_eq!(blobs, want);
    assert!(net.granted_blobs(&stranger).is_empty());

    // A response for another job id changes nothing; neither does our own PeerId.
    let mut wrong = resp(vec![lease(Some(&stranger), NOW + 60)]);
    wrong.job_id = "00000000-0000-0000-0000-000000000000".into();
    g.apply(JOB, &wrong, NOW);
    let me = net.peer_id();
    g.apply(
        JOB,
        &resp(vec![
            lease(Some(&helper), NOW + 60),
            lease(Some(&me), NOW + 60),
        ]),
        NOW,
    );
    assert_eq!(g.granted(), vec![helper]);

    // The lease leaves the list: this job's blobs are withdrawn, the other job's stay.
    g.apply(JOB, &resp(Vec::new()), NOW);
    assert!(g.granted().is_empty());
    assert_eq!(net.granted_blobs(&helper), vec![sha(b"other job")]);
    net.revoke(&helper);
}

#[test]
fn lease_without_helper_peer_id_grants_nothing() {
    let net = PeerNet::start(loopback()).unwrap();
    let mut g = JobGrants::new(&net, vec![b"input".to_vec()]);
    let mut bad = lease(None, NOW + 60);
    g.apply(JOB, &resp(vec![bad.clone()]), NOW);
    bad.helper_peer_id = "not-a-peer-id".into();
    g.apply(JOB, &resp(vec![bad]), NOW);
    // Already expired by the requester's clock: nothing either.
    g.apply(JOB, &resp(vec![lease(Some(&other_peer()), NOW)]), NOW);
    assert!(g.granted().is_empty());
    assert!(net.granted_peers().is_empty());
}

#[test]
fn revoked_on_expiry_and_on_drop() {
    let net = PeerNet::start(loopback()).unwrap();
    let (a, b) = (other_peer(), other_peer());
    {
        let mut g = JobGrants::new(&net, vec![b"input".to_vec()]);
        g.apply(
            JOB,
            &resp(vec![lease(Some(&a), NOW + 10), lease(Some(&b), NOW + 100)]),
            NOW,
        );
        assert_eq!(net.granted_peers().len(), 2);
        g.expire(NOW + 10);
        assert_eq!(g.granted(), vec![b]);
        assert_eq!(net.granted_peers(), vec![b]);
    }
    assert!(net.granted_peers().is_empty(), "drop revokes the rest");
}

fn ctx(coord: Arc<Fake>, net: Arc<PeerNet>, now: u64) -> ServeCtx {
    ServeCtx {
        coord,
        net,
        device_public: [7; 32],
        control: Arc::new(|| true),
        now_unix: Arc::new(move || now),
        poll: Duration::from_millis(10),
    }
}

fn wait_until(what: &str, f: impl Fn() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < Duration::from_secs(10), "timed out: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn serve_loop_follows_the_list_and_ends_with_the_job() {
    let net = PeerNet::start(loopback()).unwrap();
    let coord = Arc::new(Fake::new(b""));
    let helper = other_peer();
    let s = Serving::spawn(
        ctx(coord.clone(), net.clone(), NOW),
        JOB.into(),
        vec![b"input".to_vec()],
        Duration::from_secs(30),
    );
    wait_until("first list", || coord.lists.load(Ordering::SeqCst) > 0);
    assert!(net.granted_peers().is_empty(), "queued, no lease: nothing");
    assert!(coord.regs.load(Ordering::SeqCst) >= 1, "peer registered");
    coord.set(vec![lease(Some(&helper), NOW + 60)]);
    wait_until("granted", || net.granted_peers() == vec![helper]);
    coord.set(Vec::new());
    wait_until("revoked", || net.granted_peers().is_empty());
    coord.set(vec![lease(Some(&helper), NOW + 60)]);
    wait_until("granted again", || net.granted_peers() == vec![helper]);
    // The job commits; the server drops the lease and the loop ends.
    *coord.state.lock().unwrap() = "committed".into();
    coord.set(Vec::new());
    wait_until("ended", || s.is_finished());
    assert_eq!(s.finish(), Some(ServeEnd::Ended("committed".into())));
    assert!(net.granted_peers().is_empty());
}

#[test]
fn serve_loop_stops_within_a_poll_and_revokes() {
    let net = PeerNet::start(loopback()).unwrap();
    let coord = Arc::new(Fake::new(b""));
    let helper = other_peer();
    coord.set(vec![lease(Some(&helper), NOW + 60)]);
    let s = Serving::spawn(
        ctx(coord.clone(), net.clone(), NOW),
        JOB.into(),
        vec![b"input".to_vec()],
        Duration::from_secs(30),
    );
    wait_until("granted", || net.granted_peers() == vec![helper]);
    let t = Instant::now();
    assert_eq!(s.finish(), Some(ServeEnd::Stopped));
    assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
    assert!(net.granted_peers().is_empty());
}

#[test]
fn serve_loop_off_when_the_coordinator_has_no_list_or_control_is_refused() {
    let net = PeerNet::start(loopback()).unwrap();
    let mut f = Fake::new(b"");
    f.list_status = Some(404);
    let coord = Arc::new(f);
    let end = serve_job(
        &ctx(coord.clone(), net.clone(), NOW),
        JOB,
        vec![b"x".to_vec()],
        Instant::now() + Duration::from_secs(5),
        &AtomicBool::new(false),
    );
    assert_eq!(end, ServeEnd::Unavailable);

    // Tariff refuses control traffic: no lease list call at all, and no grant.
    let coord = Arc::new(Fake::new(b""));
    coord.set(vec![lease(Some(&other_peer()), NOW + 60)]);
    let mut c = ctx(coord.clone(), net.clone(), NOW);
    c.control = Arc::new(|| false);
    let end = serve_job(
        &c,
        JOB,
        vec![b"x".to_vec()],
        Instant::now() + Duration::from_millis(100),
        &AtomicBool::new(false),
    );
    assert_eq!(end, ServeEnd::Deadline);
    assert_eq!(coord.lists.load(Ordering::SeqCst), 0);
    assert_eq!(coord.regs.load(Ordering::SeqCst), 0);
    assert!(net.granted_peers().is_empty());
}

// ---- served bytes and the end-to-end path ----

struct At(DateTime<Utc>);
impl Clock for At {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

/// Inside the example profile's night window (23:00 Asia/Colombo).
fn night() -> DateTime<Utc> {
    "2026-01-05T17:30:00Z".parse().unwrap()
}
/// Outside it (15:30 Asia/Colombo).
fn day() -> DateTime<Utc> {
    "2026-01-05T10:00:00Z".parse().unwrap()
}

fn metered() -> (NetworkBudget, NetworkObservation) {
    let p = nc_tariff::sim::example_profile().unwrap();
    let net = NetworkObservation {
        interface: p.binding.interface,
        connection_id: Some(p.binding.connection_id.clone()),
        bytes_per_second: 10_000_000,
    };
    (NetworkBudget::Metered(p), net)
}

fn fake_probe(net: NetworkObservation) -> FakeProbe {
    FakeProbe::new(
        DeviceObservation {
            on_ac_power: true,
            battery_percent: None,
            thermal_level: None,
            user_idle_seconds: None,
        },
        net,
    )
}

fn data(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 13 % 251) as u8).collect()
}

fn tref(b: &[u8]) -> TransferRef {
    TransferRef {
        sha256: hex::encode(Sha256::digest(b)),
        size: b.len() as u64,
        path: "/v1/blobs/x".into(),
    }
}

fn hint(req: &PeerNet) -> PeerHints {
    PeerHints {
        peer_id: req.peer_id().to_string(),
        addrs: req
            .listen_addrs(Duration::from_secs(5))
            .iter()
            .map(ToString::to_string)
            .collect(),
        ..Default::default()
    }
}

/// Helper-side fetch with an unmetered budget (the helper's own gate is not under test here).
fn helper_fetch(helper: &PeerNet, coord: &Fake, req: &PeerNet, r: &TransferRef) -> Vec<u8> {
    let gate = BudgetGate::new(&NetworkBudget::Unmetered, night()).unwrap();
    let (_, obs) = metered();
    let probe = fake_probe(obs);
    let clock = At(night());
    let dir = tempfile::tempdir().unwrap();
    let no = || false;
    let ctx = Ctx {
        coord,
        budget: &gate,
        probe: &probe,
        clock: &clock,
        stopped: &no,
        staging: dir.path(),
    };
    let lp = LeasePeer::open(Some(helper), Some(&hint(req)));
    fetch(
        &ctx,
        lp.as_ref().map(LeasePeer::source),
        r,
        TrafficClass::Bulk,
    )
    .unwrap()
}

/// Requester's serve gate exactly as production wires it, against its own metered budget at `at`.
fn requester_gate(req: &PeerNet, at: DateTime<Utc>) -> Arc<BudgetGate> {
    let (budget, obs) = metered();
    let gate = Arc::new(BudgetGate::new(&budget, at).unwrap());
    let (g, probe) = (gate.clone(), Arc::new(fake_probe(obs)));
    req.set_serve_gate(Some(Arc::new(move |bytes| {
        charge_served(&g, probe.as_ref(), &At(at), bytes)
    })));
    gate
}

/// End to end in-process: the requester listens and grants from the (fake) coordinator's list; the helper
/// dials the hinted address and fetches directly. No HTTP blob get; served bytes charged to the requester.
#[test]
fn end_to_end_direct_fetch_from_a_listed_lease() {
    let input = data(600 * 1024);
    let req = PeerNet::start(loopback()).unwrap();
    let helper = PeerNet::start(Vec::new()).unwrap();
    let gate = requester_gate(&req, night());
    let before = gate.available_bytes().unwrap();
    let coord = Arc::new(Fake::new(&input));
    coord.set(vec![lease(Some(&helper.peer_id()), NOW + 60)]);
    let s = Serving::spawn(
        ctx(coord.clone(), req.clone(), NOW),
        JOB.into(),
        vec![input.clone()],
        Duration::from_secs(30),
    );
    // No waiting for the grant: the helper's re-dial covers the gap between its lease and the requester's poll.
    let got = helper_fetch(&helper, &coord, &req, &tref(&input));
    assert_eq!(got, input);
    assert_eq!(coord.gets.load(Ordering::SeqCst), 0, "0 HTTP blob gets");
    assert_eq!(
        helper.stats.direct_in.load(Ordering::SeqCst),
        input.len() as u64
    );
    assert_eq!(req.stats.served.load(Ordering::SeqCst), input.len() as u64);
    let charged = before - gate.available_bytes().unwrap();
    // 3 chunks, each charged n + 256 by the requester.
    assert_eq!(
        charged,
        input.len() as u64 + 3 * crate::peer::DIRECT_OVERHEAD
    );
    eprintln!(
        "end-to-end: {} bytes direct, 0 HTTP gets, requester charged {charged} bytes",
        input.len()
    );
    drop(s);
    assert!(req.granted_peers().is_empty());
}

/// Outside the requester's tariff window the blob server refuses (status 3): nothing served or charged,
/// the helper falls back to HTTP.
#[test]
fn served_bytes_refused_outside_the_requesters_window() {
    let input = data(100 * 1024);
    let req = PeerNet::start(loopback()).unwrap();
    let helper = PeerNet::start(Vec::new()).unwrap();
    let gate = requester_gate(&req, day());
    let before = gate.available_bytes();
    req.grant(helper.peer_id(), vec![input.clone()]);
    let coord = Fake::new(&input);
    let got = helper_fetch(&helper, &coord, &req, &tref(&input));
    assert_eq!(got, input, "HTTP fallback still delivers");
    assert_eq!(req.stats.served.load(Ordering::SeqCst), 0);
    assert_eq!(helper.stats.fallbacks.load(Ordering::SeqCst), 1);
    assert_eq!(coord.gets.load(Ordering::SeqCst), 1);
    assert_eq!(gate.available_bytes(), before, "nothing charged");
    req.revoke(&helper.peer_id());
}
