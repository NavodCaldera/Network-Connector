use super::*;
use crate::budget::{BudgetGate, NetworkBudget};
use crate::probe::{Clock, DeviceObservation, FakeProbe, PlatformProbe};
use chrono::{DateTime, Utc};
use nc_protocol::pb::v1::{
    CompleteRequest, CompleteResponse, HeartbeatRequest, JobStatusResponse, LeaseFailRequest,
    LeaseFailResponse, LeaseRenewRequest, LeaseRenewResponse, LeaseResponse, PeerRegisterResponse,
    RegisterRequest, SubmitJobRequest, SubmitJobResponse,
};
use nc_tariff::NetworkObservation;
use std::io::Read;
use std::sync::atomic::AtomicUsize;

/// Coordinator fake: serves one blob over "HTTP" and counts the requests.
struct Http {
    blob: Vec<u8>,
    gets: AtomicUsize,
    peer_regs: Mutex<Vec<PeerRegisterRequest>>,
    peer_status: Option<u16>,
}

impl Http {
    fn new(blob: &[u8]) -> Self {
        Self {
            blob: blob.to_vec(),
            gets: AtomicUsize::new(0),
            peer_regs: Mutex::new(Vec::new()),
            peer_status: None,
        }
    }
    fn gets(&self) -> usize {
        self.gets.load(Ordering::SeqCst)
    }
}

fn nope<T>() -> Result<T, CoordError> {
    Err(CoordError::Network("fake".into()))
}

impl Coordinator for Http {
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
        lock(&self.peer_regs).push(req.clone());
        match self.peer_status {
            Some(status) => Err(CoordError::Status {
                status,
                code: String::new(),
            }),
            None => Ok(PeerRegisterResponse {
                peer_id: req.peer_id.clone(),
                expires_at: 0,
            }),
        }
    }
    fn submit(&self, _: &SubmitJobRequest) -> Result<SubmitJobResponse, CoordError> {
        nope()
    }
    fn job_status(&self, _: &str) -> Result<JobStatusResponse, CoordError> {
        nope()
    }
}

struct Fixed(DateTime<Utc>);
impl Clock for Fixed {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

/// Inside the example profile's night window (23:00 Asia/Colombo).
fn night() -> DateTime<Utc> {
    "2026-01-05T17:30:00Z".parse().unwrap()
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

fn probe(net: NetworkObservation) -> FakeProbe {
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

fn loopback() -> Vec<Multiaddr> {
    vec!["/ip4/127.0.0.1/tcp/0".parse().unwrap()]
}

fn blob(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 7 % 251) as u8).collect()
}

fn tref(b: &[u8]) -> TransferRef {
    TransferRef {
        sha256: hex::encode(Sha256::digest(b)),
        size: b.len() as u64,
        path: "/v1/blobs/x".into(),
    }
}

fn hint(net: &PeerNet, peer_id: String) -> PeerHints {
    PeerHints {
        peer_id,
        addrs: net
            .listen_addrs(Duration::from_secs(5))
            .iter()
            .map(|a| a.to_string())
            .collect(),
        ..Default::default()
    }
}

struct Env {
    gate: BudgetGate,
    probe: FakeProbe,
    clock: Fixed,
    dir: tempfile::TempDir,
}

fn env() -> Env {
    let (budget, net) = metered();
    Env {
        gate: BudgetGate::new(&budget, night()).unwrap(),
        probe: probe(net),
        clock: Fixed(night()),
        dir: tempfile::tempdir().unwrap(),
    }
}

fn run(
    e: &Env,
    coord: &Http,
    direct: Option<(&PeerNet, PeerId)>,
    r: &TransferRef,
) -> Result<Vec<u8>, TransferError> {
    let no = || false;
    let ctx = Ctx {
        coord,
        budget: &e.gate,
        probe: &e.probe,
        clock: &e.clock,
        stopped: &no,
        staging: e.dir.path(),
    };
    fetch(&ctx, direct, r, TrafficClass::Bulk)
}

/// Two in-process nodes: the requester grants the helper; the helper fetches 3 chunks directly, no HTTP.
#[test]
fn direct_fetch_between_two_nodes() {
    let req = PeerNet::start(loopback()).unwrap();
    let helper = PeerNet::start(Vec::new()).unwrap();
    let data = blob(600 * 1024);
    req.grant(helper.peer_id(), vec![data.clone()]);
    let coord = Http::new(&data);
    let e = env();
    let lp = LeasePeer::open(Some(&helper), Some(&hint(&req, req.peer_id().to_string()))).unwrap();
    let got = run(&e, &coord, Some(lp.source()), &tref(&data)).unwrap();
    assert_eq!(got, data);
    assert_eq!(coord.gets(), 0, "no HTTP request");
    assert_eq!(
        helper.stats.direct_in.load(Ordering::SeqCst),
        data.len() as u64
    );
    assert_eq!(req.stats.served.load(Ordering::SeqCst), data.len() as u64);
    assert_eq!(helper.stats.fallbacks.load(Ordering::SeqCst), 0);
    drop(lp);
    assert!(
        !helper.node.trust().contains(&req.peer_id()),
        "lease trust released"
    );
}

/// Budget: each chunk is charged (chunk + overhead) before it is requested.
#[test]
fn direct_budget_charged_per_chunk() {
    let req = PeerNet::start(loopback()).unwrap();
    let helper = PeerNet::start(Vec::new()).unwrap();
    let data = blob(600 * 1024); // 256 + 256 + 88 KiB
    req.grant(helper.peer_id(), vec![data.clone()]);
    let coord = Http::new(&data);
    let e = env();
    let before = e.gate.available_bytes().unwrap();
    let lp = LeasePeer::open(Some(&helper), Some(&hint(&req, req.peer_id().to_string()))).unwrap();
    run(&e, &coord, Some(lp.source()), &tref(&data)).unwrap();
    let spent = before - e.gate.available_bytes().unwrap();
    assert_eq!(spent, data.len() as u64 + 3 * DIRECT_OVERHEAD);

    // On cellular the first chunk is refused before any request reaches the requester, and HTTP is not
    // tried either (it would be refused by the same gate).
    let mut cell = e.probe.network();
    cell.interface = nc_tariff::InterfaceClass::Cellular;
    e.probe.set_network(cell);
    let served = req.stats.served.load(Ordering::SeqCst);
    let r = run(&e, &coord, Some(lp.source()), &tref(&data));
    assert!(matches!(r, Err(TransferError::Window(_))), "{r:?}");
    assert_eq!(
        req.stats.served.load(Ordering::SeqCst),
        served,
        "no byte served"
    );
    assert_eq!(coord.gets(), 0);
}

/// A hint whose PeerId is not the key behind the addresses: the handshake refuses it, HTTP is used.
#[test]
fn wrong_peer_id_is_refused_and_falls_back() {
    let req = PeerNet::start(loopback()).unwrap();
    let helper = PeerNet::start(Vec::new()).unwrap();
    let data = blob(10_000);
    req.grant(helper.peer_id(), vec![data.clone()]);
    let other = Keypair::generate_ed25519().public().to_peer_id();
    let coord = Http::new(&data);
    let e = env();
    let lp = LeasePeer::open(Some(&helper), Some(&hint(&req, other.to_string())));
    assert!(lp.is_none(), "impostor address must not connect");
    let got = run(&e, &coord, lp.as_ref().map(LeasePeer::source), &tref(&data)).unwrap();
    assert_eq!(got, data);
    assert_eq!(coord.gets(), 1);
    assert_eq!(req.stats.served.load(Ordering::SeqCst), 0);
    assert!(!helper.node.trust().contains(&other));
}

/// The requester serves only the helper it granted, only the granted blobs.
#[test]
fn ungranted_peer_or_blob_is_refused() {
    let req = PeerNet::start(loopback()).unwrap();
    let helper = PeerNet::start(Vec::new()).unwrap();
    let granted = blob(5_000);
    let secret = blob(6_000);
    // Helper trusted for another blob only.
    req.grant(helper.peer_id(), vec![granted]);
    let coord = Http::new(&secret);
    let e = env();
    let lp = LeasePeer::open(Some(&helper), Some(&hint(&req, req.peer_id().to_string()))).unwrap();
    let got = run(&e, &coord, Some(lp.source()), &tref(&secret)).unwrap();
    assert_eq!(got, secret);
    assert_eq!(coord.gets(), 1, "fell back to HTTP");
    assert_eq!(req.stats.served.load(Ordering::SeqCst), 0);
    // A third node that was never granted cannot even connect.
    let stranger = PeerNet::start(Vec::new()).unwrap();
    assert!(LeasePeer::open(
        Some(&stranger),
        Some(&hint(&req, req.peer_id().to_string()))
    )
    .is_none());
    // Revoked: nothing more.
    req.revoke(&helper.peer_id());
    let r = run(&e, &coord, Some(lp.source()), &tref(&secret)).unwrap();
    assert_eq!(r, secret);
    assert_eq!(req.stats.served.load(Ordering::SeqCst), 0);
}

/// A requester that serves different bytes under the right hash: refused, HTTP gives the real bytes.
#[test]
fn tampered_chunk_is_refused_and_falls_back() {
    let req = PeerNet::start(loopback()).unwrap();
    let helper = PeerNet::start(Vec::new()).unwrap();
    let data = blob(300 * 1024);
    let mut bad = data.clone();
    bad[280 * 1024] ^= 1;
    let sha: [u8; 32] = Sha256::digest(&data).into();
    req.grant_raw(helper.peer_id(), HashMap::from([(sha, Arc::new(bad))]));
    let coord = Http::new(&data);
    let e = env();
    let lp = LeasePeer::open(Some(&helper), Some(&hint(&req, req.peer_id().to_string()))).unwrap();
    let got = run(&e, &coord, Some(lp.source()), &tref(&data)).unwrap();
    assert_eq!(got, data);
    assert_eq!(coord.gets(), 1);
    assert_eq!(helper.stats.direct_in.load(Ordering::SeqCst), 0);
    assert_eq!(helper.stats.fallbacks.load(Ordering::SeqCst), 1);
    assert!(
        std::fs::read_dir(e.dir.path()).unwrap().next().is_none(),
        "no part file left"
    );
}

/// No hint (or no node): straight to HTTP.
#[test]
fn no_hint_uses_http() {
    let helper = PeerNet::start(Vec::new()).unwrap();
    let data = blob(1_000);
    let coord = Http::new(&data);
    let e = env();
    assert!(LeasePeer::open(Some(&helper), None).is_none());
    assert!(LeasePeer::open(None, Some(&PeerHints::default())).is_none());
    assert_eq!(run(&e, &coord, None, &tref(&data)).unwrap(), data);
    assert_eq!(coord.gets(), 1);
    // Malformed hints never connect anywhere.
    let bad = PeerHints {
        peer_id: "not-a-peer".into(),
        addrs: vec!["/ip4/127.0.0.1/tcp/1".into()],
        ..Default::default()
    };
    assert!(LeasePeer::open(Some(&helper), Some(&bad)).is_none());
}

/// Registration body: PeerId matches the key, the binding verifies, addresses carry no /p2p or 0.0.0.0;
/// refresh is rate-limited and stops on 401.
#[test]
fn registration_binding_and_refresh() {
    use ed25519_dalek::{Signature, VerifyingKey};
    let net = PeerNet::start(vec![
        "/ip4/127.0.0.1/tcp/0".parse().unwrap(),
        "/ip4/0.0.0.0/tcp/0".parse().unwrap(),
    ])
    .unwrap();
    let _ = net.listen_addrs(Duration::from_secs(5));
    std::thread::sleep(Duration::from_millis(200));
    let device = [7u8; 32];
    let r = net.register_request(&device).unwrap();
    assert_eq!(r.peer_id, net.peer_id().to_string());
    let pk: [u8; 32] = URL_SAFE_NO_PAD
        .decode(&r.libp2p_public_key)
        .unwrap()
        .try_into()
        .unwrap();
    let sig: [u8; 64] = URL_SAFE_NO_PAD
        .decode(&r.binding_signature)
        .unwrap()
        .try_into()
        .unwrap();
    let mut msg = b"nc.v1.PeerBinding\0".to_vec();
    msg.extend_from_slice(&device);
    VerifyingKey::from_bytes(&pk)
        .unwrap()
        .verify_strict(&msg, &Signature::from_bytes(&sig))
        .unwrap();
    assert!(!r.addrs.is_empty());
    for a in &r.addrs {
        assert!(!a.contains("/p2p") && !a.contains("0.0.0.0"), "{a}");
    }

    let coord = Http::new(b"");
    net.refresh_if_due(&coord, &device, || true);
    net.refresh_if_due(&coord, &device, || true);
    assert_eq!(lock(&coord.peer_regs).len(), 1, "not due again yet");

    // Tariff says no: nothing sent.
    let fresh = PeerNet::start(Vec::new()).unwrap();
    fresh.refresh_if_due(&coord, &device, || false);
    assert_eq!(lock(&coord.peer_regs).len(), 1);

    // Deregistered device: one 401, then no more attempts.
    let mut gone = Http::new(b"");
    gone.peer_status = Some(401);
    fresh.refresh_if_due(&gone, &device, || true);
    lock(&fresh.refresh).next = None;
    fresh.refresh_if_due(&gone, &device, || true);
    assert_eq!(lock(&gone.peer_regs).len(), 1);
}
