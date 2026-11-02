//! Direct job-byte transfer between devices over nc-net (1.L15, gap G1b).
//!
//! The agent runs one [`PeerNet`]: an nc-net libp2p node with a fresh per-process Ed25519 key, registered with
//! the coordinator through the signed `POST /v1/devices/peer` (refreshed every [`REFRESH_EVERY`], well before
//! the server's 600 s expiry; refreshing stops once the coordinator no longer knows the device).
//!
//! - **Requester side**: [`PeerNet::grant`] lets one helper PeerId fetch exactly the listed blobs. Anyone else,
//!   and any other blob, gets a refusal. The nc-net gate already closes connections from peers that are not in
//!   the node's (in-memory, lease-scoped) trust set, and the grant is checked again per request.
//! - **Helper side**: when a lease carries `LeaseResponse.peer`, [`fetch`] dials the hinted addresses with the
//!   hinted PeerId appended (so the Noise/TLS handshake itself refuses another key), checks the authenticated
//!   PeerId of the stream equals `peer.peer_id` before sending the request, charges every chunk to the budget
//!   gate **before** asking for it, and verifies size and sha256 exactly as the HTTP path does. Any network,
//!   peer or verification failure falls back to the coordinator HTTP path ([`transfer::download`]); a local
//!   refusal (Stop, tariff window, budget) does not, because HTTP would be refused too.
//!
//! Hints are not authorisation (proto/README.md): addresses are only dial candidates, and a hinted peer is
//! trusted only for the lease that named it ([`PeerNet::prepare`] / [`PeerNet::release`]).
//!
//! Wire format of `/nc/blob/1.0.0` (one request per stream; see the hand-back request to move it to proto/):
//! request `sha256[32] || offset u64 BE || len u32 BE`; reply `status u8`, and when status is 0 a
//! `n u32 BE` (1..=len) followed by `n` bytes of the blob from `offset`.
//!
//! Nothing here logs keys, PeerIds of counterparts with job context, addresses, blob hashes or bytes
//! (AGENT.md §1 rule 10).

use crate::budget::Grant;
use crate::coord::{CoordError, Coordinator};
use crate::transfer::{self, refusal, same_network, Ctx, TransferError};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use futures::{AsyncReadExt, AsyncWriteExt};
use nc_net::{Keypair, Multiaddr, Node, NodeConfig, PeerId, PeerStream, Protocol, StreamProtocol};
use nc_protocol::pb::v1::{PeerHints, PeerRegisterRequest, TransferRef};
use nc_tariff::TrafficClass;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Write;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Stream protocol for blob chunks.
pub const BLOB_PROTOCOL: StreamProtocol = StreamProtocol::new("/nc/blob/1.0.0");
/// Bytes asked for (and charged) per direct request.
pub const DIRECT_CHUNK: u64 = 256 * 1024;
/// Wire bytes charged per direct request on top of the chunk (request, status, length, stream setup).
pub const DIRECT_OVERHEAD: u64 = 256;
/// How often the peer registration is refreshed. The server keeps it for 600 s.
pub const REFRESH_EVERY: Duration = Duration::from_secs(240);
/// Retry delay after a failed refresh.
const REFRESH_RETRY: Duration = Duration::from_secs(30);
/// How long a hinted peer may take to connect.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest one chunk request may take.
const CHUNK_TIMEOUT: Duration = Duration::from_secs(30);
/// Requester side: time a helper has to send its request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Largest number of hinted addresses dialled (the server caps at 16 too).
const MAX_HINT_ADDRS: usize = 16;
/// Domain of the binding signature (proto/README.md "Peer hints").
const BINDING_DOMAIN: &[u8] = b"nc.v1.PeerBinding\0";

const STATUS_OK: u8 = 0;
const STATUS_DENIED: u8 = 1;
const STATUS_RANGE: u8 = 2;
const STATUS_BUSY: u8 = 3;

/// Why a direct transfer did not happen. Carries no addresses, keys or data.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PeerError {
    /// The node could not start.
    #[error("peer node failed to start")]
    Start,
    /// The hint was malformed.
    #[error("peer hint invalid")]
    BadHint,
    /// The hinted peer could not be reached.
    #[error("peer unreachable")]
    Unreachable,
    /// The authenticated peer key was not the hinted one.
    #[error("peer key mismatch")]
    WrongPeer,
    /// The peer refused the request (not granted, budget, range).
    #[error("peer refused")]
    Refused,
    /// The peer broke the protocol (oversized or empty reply).
    #[error("peer protocol error")]
    Protocol,
    /// Timed out.
    #[error("peer timed out")]
    Timeout,
    /// Stop requested.
    #[error("stopped")]
    Stopped,
}

/// Byte counters for the two paths (1.L16 surfaces them to the owner).
#[derive(Debug, Default)]
pub struct PathStats {
    /// Verified blob bytes received over nc-net.
    pub direct_in: AtomicU64,
    /// Blob bytes received over the coordinator HTTP path while a node was running.
    pub http_in: AtomicU64,
    /// Blob bytes served to granted helpers.
    pub served: AtomicU64,
    /// Direct attempts that fell back to HTTP.
    pub fallbacks: AtomicU64,
}

/// Requester-side check before serving a chunk: `true` lets `bytes` go out (the caller charged them).
pub type ServeGate = Arc<dyn Fn(u64) -> bool + Send + Sync>;

type Grants = Arc<Mutex<HashMap<PeerId, HashMap<[u8; 32], Arc<Vec<u8>>>>>>;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Refresh {
    next: Option<Instant>,
    disabled: bool,
}

/// The agent's nc-net node plus the blob server and the registration state.
pub struct PeerNet {
    rt: tokio::runtime::Runtime,
    node: Node,
    keypair: Keypair,
    grants: Grants,
    gate: Arc<Mutex<Option<ServeGate>>>,
    refresh: Mutex<Refresh>,
    /// Byte counters.
    pub stats: Arc<PathStats>,
}

impl std::fmt::Debug for PeerNet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PeerNet").finish_non_exhaustive()
    }
}

fn hex32(s: &str) -> Option<[u8; 32]> {
    let v = hex::decode(s).ok()?;
    v.try_into().ok()
}

/// `addr` without any `/p2p/..` or `/p2p-circuit` part.
fn transport_only(addr: &Multiaddr) -> Multiaddr {
    addr.iter()
        .filter(|p| !matches!(p, Protocol::P2p(_) | Protocol::P2pCircuit))
        .collect()
}

fn unspecified(addr: &Multiaddr) -> bool {
    addr.iter().any(|p| match p {
        Protocol::Ip4(ip) => ip.is_unspecified(),
        Protocol::Ip6(ip) => ip.is_unspecified(),
        _ => false,
    })
}

impl PeerNet {
    /// Start a node with a fresh Ed25519 key listening on `listen` (empty: outbound only, i.e. helper only).
    pub fn start(listen: Vec<Multiaddr>) -> Result<Arc<Self>, PeerError> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("nc-peer")
            .enable_all()
            .build()
            .map_err(|_| PeerError::Start)?;
        let keypair = Keypair::generate_ed25519();
        let mut cfg = NodeConfig::new(keypair.clone());
        cfg.listen = listen;
        let node = rt
            .block_on(Node::start(cfg))
            .map_err(|_| PeerError::Start)?;
        let incoming = {
            let _g = rt.enter();
            node.accept(BLOB_PROTOCOL).map_err(|_| PeerError::Start)?
        };
        let net = Arc::new(Self {
            rt,
            node,
            keypair,
            grants: Arc::default(),
            gate: Arc::default(),
            refresh: Mutex::new(Refresh {
                next: None,
                disabled: false,
            }),
            stats: Arc::default(),
        });
        let (grants, gate, stats) = (net.grants.clone(), net.gate.clone(), net.stats.clone());
        net.rt.spawn(serve_loop(incoming, grants, gate, stats));
        Ok(net)
    }

    /// This node's PeerId.
    pub fn peer_id(&self) -> PeerId {
        self.node.peer_id()
    }

    /// Current listen/external addresses (waits up to `wait` for the first one).
    pub fn listen_addrs(&self, wait: Duration) -> Vec<Multiaddr> {
        let deadline = Instant::now() + wait;
        loop {
            let a = self
                .rt
                .block_on(self.node.listen_addrs())
                .unwrap_or_default();
            if !a.is_empty() || Instant::now() >= deadline {
                return a;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Set the requester-side budget check run before each served chunk.
    pub fn set_serve_gate(&self, gate: Option<ServeGate>) {
        *lock(&self.gate) = gate;
    }

    // ---- requester side ----

    /// Let `helper` fetch exactly these blobs (keyed by their own sha256) until [`Self::revoke`].
    pub fn grant(&self, helper: PeerId, blobs: Vec<Vec<u8>>) {
        let set = blobs
            .into_iter()
            .map(|b| {
                let sha: [u8; 32] = Sha256::digest(&b).into();
                (sha, Arc::new(b))
            })
            .collect();
        self.grant_raw(helper, set);
    }

    fn grant_raw(&self, helper: PeerId, set: HashMap<[u8; 32], Arc<Vec<u8>>>) {
        let _ = self.node.trust().add(helper);
        lock(&self.grants).entry(helper).or_default().extend(set);
    }

    /// Withdraw everything granted to `helper`.
    pub fn revoke(&self, helper: &PeerId) {
        lock(&self.grants).remove(helper);
        let _ = self.node.trust().remove(helper);
    }

    // ---- helper side ----

    /// Trust the hinted requester for this lease and connect to it. Returns its PeerId.
    pub fn prepare(&self, hint: &PeerHints) -> Result<PeerId, PeerError> {
        let peer = PeerId::from_str(&hint.peer_id).map_err(|_| PeerError::BadHint)?;
        if peer == self.peer_id() {
            return Err(PeerError::BadHint);
        }
        let addrs: Vec<Multiaddr> = hint
            .addrs
            .iter()
            .take(MAX_HINT_ADDRS)
            .filter_map(|a| a.parse::<Multiaddr>().ok())
            .map(|a| transport_only(&a).with(Protocol::P2p(peer)))
            .collect();
        if addrs.is_empty() {
            return Err(PeerError::BadHint);
        }
        let _ = self.node.trust().add(peer);
        let connected = self.rt.block_on(async {
            if self.node.is_connected(peer).await.unwrap_or(false) {
                return true;
            }
            for a in addrs {
                // The /p2p suffix makes the transport handshake reject any other key.
                let _ = self.node.dial(a).await;
            }
            let deadline = tokio::time::Instant::now() + CONNECT_TIMEOUT;
            while tokio::time::Instant::now() < deadline {
                if self.node.is_connected(peer).await.unwrap_or(false) {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            false
        });
        if !connected {
            self.release(&peer);
            return Err(PeerError::Unreachable);
        }
        Ok(peer)
    }

    /// End the lease-scoped trust in `peer` (unless we also granted it blobs).
    pub fn release(&self, peer: &PeerId) {
        if !lock(&self.grants).contains_key(peer) {
            let _ = self.node.trust().remove(peer);
        }
    }

    /// Ask `peer` for `len` bytes of blob `sha` from `offset`. Polls `stopped` every 20 ms.
    fn fetch_chunk(
        &self,
        peer: PeerId,
        sha: [u8; 32],
        offset: u64,
        len: u32,
        stopped: &dyn Fn() -> bool,
    ) -> Result<Vec<u8>, PeerError> {
        self.rt.block_on(async {
            let req = request_chunk(&self.node, peer, sha, offset, len);
            tokio::pin!(req);
            let deadline = tokio::time::sleep(CHUNK_TIMEOUT);
            tokio::pin!(deadline);
            let mut tick = tokio::time::interval(Duration::from_millis(20));
            loop {
                tokio::select! {
                    r = &mut req => return r,
                    _ = &mut deadline => return Err(PeerError::Timeout),
                    _ = tick.tick() => if stopped() { return Err(PeerError::Stopped) },
                }
            }
        })
    }

    // ---- registration ----

    /// Signed-binding registration body for a device with public key `device_public`.
    pub fn register_request(
        &self,
        device_public: &[u8; 32],
    ) -> Result<PeerRegisterRequest, PeerError> {
        let mut msg = BINDING_DOMAIN.to_vec();
        msg.extend_from_slice(device_public);
        let sig = self.keypair.sign(&msg).map_err(|_| PeerError::Start)?;
        let public = self
            .keypair
            .public()
            .try_into_ed25519()
            .map_err(|_| PeerError::Start)?
            .to_bytes();
        let mut addrs: Vec<String> = Vec::new();
        for a in self.listen_addrs(Duration::ZERO) {
            let a = transport_only(&a);
            let s = a.to_string();
            if unspecified(&a) || s.len() > 256 || addrs.contains(&s) {
                continue;
            }
            addrs.push(s);
            if addrs.len() == MAX_HINT_ADDRS {
                break;
            }
        }
        Ok(PeerRegisterRequest {
            peer_id: self.peer_id().to_string(),
            libp2p_public_key: URL_SAFE_NO_PAD.encode(public),
            binding_signature: URL_SAFE_NO_PAD.encode(sig),
            addrs,
        })
    }

    /// Register or refresh if due. `allowed` charges the control call to the tariff (false: not now).
    /// A 401/403 (device unknown or deregistered) or 404 (old coordinator) stops refreshing.
    pub fn refresh_if_due(
        &self,
        coord: &dyn Coordinator,
        device_public: &[u8; 32],
        allowed: impl FnOnce() -> bool,
    ) {
        {
            let r = lock(&self.refresh);
            if r.disabled || r.next.is_some_and(|n| Instant::now() < n) {
                return;
            }
        }
        if !allowed() {
            return;
        }
        let res = self
            .register_request(device_public)
            .map_err(|_| CoordError::Sign)
            .and_then(|req| coord.register_peer(&req));
        let mut r = lock(&self.refresh);
        match res {
            Ok(_) => r.next = Some(Instant::now() + REFRESH_EVERY),
            Err(CoordError::Status {
                status: 401 | 403 | 404,
                ..
            }) => {
                tracing::info!("peer registration not available; direct transfers off");
                r.disabled = true;
            }
            Err(e) => {
                tracing::warn!(error = %e, "peer registration failed; retrying");
                r.next = Some(Instant::now() + REFRESH_RETRY);
            }
        }
    }
}

async fn request_chunk(
    node: &Node,
    peer: PeerId,
    sha: [u8; 32],
    offset: u64,
    len: u32,
) -> Result<Vec<u8>, PeerError> {
    let mut s = node
        .open_stream(peer, BLOB_PROTOCOL)
        .await
        .map_err(|_| PeerError::Unreachable)?;
    // Before any job byte is sent or accepted: the authenticated key must be the hinted one.
    if s.peer() != peer {
        return Err(PeerError::WrongPeer);
    }
    let io = |_| PeerError::Unreachable;
    let mut req = Vec::with_capacity(44);
    req.extend_from_slice(&sha);
    req.extend_from_slice(&offset.to_be_bytes());
    req.extend_from_slice(&len.to_be_bytes());
    s.write_all(&req).await.map_err(io)?;
    s.flush().await.map_err(io)?;
    let mut st = [0u8; 1];
    s.read_exact(&mut st).await.map_err(io)?;
    if st[0] != STATUS_OK {
        return Err(PeerError::Refused);
    }
    let mut n = [0u8; 4];
    s.read_exact(&mut n).await.map_err(io)?;
    let n = u32::from_be_bytes(n);
    if n == 0 || n > len {
        return Err(PeerError::Protocol);
    }
    let mut buf = vec![0u8; n as usize];
    s.read_exact(&mut buf).await.map_err(io)?;
    let _ = s.close().await;
    Ok(buf)
}

async fn serve_loop(
    mut incoming: nc_net::IncomingPeerStreams,
    grants: Grants,
    gate: Arc<Mutex<Option<ServeGate>>>,
    stats: Arc<PathStats>,
) {
    while let Some(s) = incoming.next().await {
        tokio::spawn(serve_one(s, grants.clone(), gate.clone(), stats.clone()));
    }
}

async fn reply(s: &mut PeerStream, status: u8) {
    let _ = s.write_all(&[status]).await;
    let _ = s.close().await;
}

async fn serve_one(
    mut s: PeerStream,
    grants: Grants,
    gate: Arc<Mutex<Option<ServeGate>>>,
    stats: Arc<PathStats>,
) {
    let peer = s.peer();
    let mut req = [0u8; 44];
    match tokio::time::timeout(REQUEST_TIMEOUT, s.read_exact(&mut req)).await {
        Ok(Ok(())) => {}
        _ => return,
    }
    let mut sha = [0u8; 32];
    sha.copy_from_slice(&req[..32]);
    let offset = u64::from_be_bytes(req[32..40].try_into().unwrap_or([0xff; 8]));
    let len = u32::from_be_bytes(req[40..44].try_into().unwrap_or([0; 4]));
    // Only the peer the lease named, only its blobs.
    let blob = lock(&grants).get(&peer).and_then(|m| m.get(&sha)).cloned();
    let Some(blob) = blob else {
        tracing::debug!("refused an ungranted blob request");
        return reply(&mut s, STATUS_DENIED).await;
    };
    let size = blob.len() as u64;
    if offset >= size || len == 0 {
        return reply(&mut s, STATUS_RANGE).await;
    }
    let n = u64::from(len).min(DIRECT_CHUNK).min(size - offset);
    let g = lock(&gate).clone();
    if let Some(g) = g {
        if !g(n + DIRECT_OVERHEAD) {
            return reply(&mut s, STATUS_BUSY).await;
        }
    }
    let (a, b) = (offset as usize, (offset + n) as usize);
    let mut out = Vec::with_capacity(5 + (b - a));
    out.push(STATUS_OK);
    out.extend_from_slice(&(n as u32).to_be_bytes());
    out.extend_from_slice(&blob[a..b]);
    if s.write_all(&out).await.is_ok() && s.flush().await.is_ok() {
        stats.served.fetch_add(n, Ordering::Relaxed);
    }
    let _ = s.close().await;
}

/// Outcome of the direct attempt.
enum Direct {
    /// Verified bytes.
    Ok(Vec<u8>),
    /// A local refusal; HTTP would be refused too.
    Local(TransferError),
    /// Peer or verification failure; use HTTP.
    Fallback,
}

fn direct_part(staging: &std::path::Path, sha_hex: &str) -> std::path::PathBuf {
    staging.join(format!("{sha_hex}.direct.part"))
}

/// Download `r` from `peer` over nc-net, chunk by chunk, each chunk charged before it is requested.
fn download_direct(
    ctx: &Ctx<'_>,
    net: &PeerNet,
    peer: PeerId,
    r: &TransferRef,
    class: TrafficClass,
) -> Direct {
    let Some(sha) = hex32(&r.sha256) else {
        return Direct::Fallback;
    };
    let part = direct_part(ctx.staging, &r.sha256);
    let _ = std::fs::remove_file(&part);
    let out = (|| {
        let mut file = std::fs::File::create(&part).map_err(|_| Direct::Fallback)?;
        let mut have = 0u64;
        while have < r.size {
            if (ctx.stopped)() {
                return Err(Direct::Local(TransferError::Stopped));
            }
            let want = (r.size - have).min(DIRECT_CHUNK);
            let grant: Grant = ctx
                .budget
                .authorize(
                    class,
                    want + DIRECT_OVERHEAD,
                    ctx.clock.now(),
                    &ctx.probe.network(),
                )
                .map_err(|e| Direct::Local(refusal(e)))?;
            if grant.until.is_some_and(|u| ctx.clock.now() >= u) {
                ctx.budget.settle(grant, 0);
                return Err(Direct::Local(TransferError::Window("WINDOW_ENDED")));
            }
            if !same_network(&grant, ctx) {
                ctx.budget.settle(grant, 0);
                return Err(Direct::Local(TransferError::Window("NETWORK_CHANGED")));
            }
            let len = u32::try_from(want).unwrap_or(u32::MAX);
            match net.fetch_chunk(peer, sha, have, len, ctx.stopped) {
                Ok(bytes) => {
                    ctx.budget
                        .settle(grant, bytes.len() as u64 + DIRECT_OVERHEAD);
                    file.write_all(&bytes).map_err(|_| Direct::Fallback)?;
                    have += bytes.len() as u64;
                }
                Err(PeerError::Stopped) => {
                    ctx.budget.settle(grant, DIRECT_OVERHEAD);
                    return Err(Direct::Local(TransferError::Stopped));
                }
                Err(_) => {
                    // The request may have gone out; charge the overhead, not the unreceived chunk.
                    ctx.budget.settle(grant, DIRECT_OVERHEAD);
                    return Err(Direct::Fallback);
                }
            }
        }
        file.flush().map_err(|_| Direct::Fallback)?;
        drop(file);
        let bytes = std::fs::read(&part).map_err(|_| Direct::Fallback)?;
        // Exactly the HTTP path's check.
        if bytes.len() as u64 != r.size || hex::encode(Sha256::digest(&bytes)) != r.sha256 {
            return Err(Direct::Fallback);
        }
        Ok(bytes)
    })();
    let _ = std::fs::remove_file(&part);
    match out {
        Ok(b) => Direct::Ok(b),
        Err(d) => d,
    }
}

/// Fetch one transfer: directly from `direct` when given, else (or on a peer/verification failure) over the
/// coordinator HTTP path.
pub fn fetch(
    ctx: &Ctx<'_>,
    direct: Option<(&PeerNet, PeerId)>,
    r: &TransferRef,
    class: TrafficClass,
) -> Result<Vec<u8>, TransferError> {
    if let Some((net, peer)) = direct {
        match download_direct(ctx, net, peer, r, class) {
            Direct::Ok(b) => {
                net.stats
                    .direct_in
                    .fetch_add(b.len() as u64, Ordering::Relaxed);
                return Ok(b);
            }
            Direct::Local(e) => return Err(e),
            Direct::Fallback => {
                net.stats.fallbacks.fetch_add(1, Ordering::Relaxed);
                tracing::info!("direct transfer failed; using the coordinator");
            }
        }
    }
    let b = transfer::download(ctx, r, class)?;
    if let Some((net, _)) = direct {
        net.stats
            .http_in
            .fetch_add(b.len() as u64, Ordering::Relaxed);
    }
    Ok(b)
}

/// Lease-scoped direct source: prepares the hinted peer and releases it on drop.
pub struct LeasePeer<'a> {
    net: &'a PeerNet,
    peer: PeerId,
}

impl<'a> LeasePeer<'a> {
    /// `None` when there is no node, no hint, or the hinted peer cannot be reached.
    pub fn open(net: Option<&'a PeerNet>, hint: Option<&PeerHints>) -> Option<Self> {
        let (net, hint) = (net?, hint?);
        match net.prepare(hint) {
            Ok(peer) => Some(Self { net, peer }),
            Err(e) => {
                net.stats.fallbacks.fetch_add(1, Ordering::Relaxed);
                tracing::info!(error = %e, "direct path unavailable; using the coordinator");
                None
            }
        }
    }

    /// Argument for [`fetch`].
    pub fn source(&self) -> (&'a PeerNet, PeerId) {
        (self.net, self.peer)
    }
}

impl Drop for LeasePeer<'_> {
    fn drop(&mut self) {
        self.net.release(&self.peer);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;

/// The agent's node plus the device key it is bound to (for [`PeerNet::refresh_if_due`]).
#[derive(Debug, Clone)]
pub struct AgentPeer {
    /// Running node.
    pub net: Arc<PeerNet>,
    /// Device public key the PeerId is bound to.
    pub device_public: [u8; 32],
}
