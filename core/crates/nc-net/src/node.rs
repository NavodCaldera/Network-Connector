//! The libp2p node: transport, discovery, connection gating, pairing and the stream API for Core.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt;
use libp2p::core::ConnectedPoint;
use libp2p::multiaddr::Protocol;
use libp2p::swarm::behaviour::toggle::Toggle;
use libp2p::swarm::{ConnectionId, NetworkBehaviour, SwarmEvent};
use libp2p::{autonat, dcutr, identify, identity, mdns, noise, ping, relay, tcp, yamux};
use libp2p::{Multiaddr, PeerId, StreamProtocol, Swarm};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::ladder::{classify_addr, ConnEntry, ConnectionInfo, Ledger, Rung, TrafficStats};
use crate::pairing::{self, HostOffer, PairingOffer, PairingPrompt, ResponderStage};
use crate::relay_auth::{self, RELAY_AUTH_PROTOCOL};
use crate::stream::PeerStream;
use crate::trust::TrustStore;
use crate::NetError;

/// Pairing protocol id (reserved; Core cannot open it).
pub const PAIR_PROTOCOL: StreamProtocol = StreamProtocol::new("/nc/pair/1.0.0");
/// How long an unpaired peer may stay connected while an offer is open.
const PAIRING_GRACE: Duration = Duration::from_secs(150);
/// Time a user has to compare the SAS.
const SAS_TIMEOUT: Duration = Duration::from_secs(120);
/// A relayed connection is closed this long after a direct one to the same peer exists.
const RELAY_CLOSE_DELAY: Duration = Duration::from_secs(3);

/// Node settings.
pub struct NodeConfig {
    /// Device transport key. Its [`PeerId`] is the identity peers pair with.
    pub keypair: identity::Keypair,
    /// Addresses to listen on, e.g. `/ip4/0.0.0.0/udp/0/quic-v1`. Tests use loopback only.
    pub listen: Vec<Multiaddr>,
    /// Enable mDNS discovery (LAN). Discovery only finds addresses; it never grants trust.
    pub mdns: bool,
    /// Paired peers.
    pub trust: TrustStore,
    /// Infrastructure peers (our relays) that may connect without pairing. They never get app streams.
    pub infra_peers: Vec<PeerId>,
    /// Idle connection timeout.
    pub idle_timeout: Duration,
}

impl NodeConfig {
    /// Config with a fresh key, no listeners, mDNS off, empty in-memory trust store.
    pub fn new(keypair: identity::Keypair) -> Self {
        Self {
            keypair,
            listen: Vec::new(),
            mdns: false,
            trust: TrustStore::in_memory(),
            infra_peers: Vec::new(),
            idle_timeout: Duration::from_secs(300),
        }
    }
}

/// Things the node reports (for UI, logs and the probe).
#[derive(Clone, Debug)]
pub enum NodeEvent {
    /// Listening on a new local address.
    ListenAddr(Multiaddr),
    /// A connection was accepted by the gate.
    Connected {
        peer: PeerId,
        rung: Rung,
        addr: Multiaddr,
    },
    /// The last connection to a peer closed.
    Disconnected { peer: PeerId },
    /// A connection from/to an unpaired, non-infrastructure peer was closed.
    Rejected { peer: PeerId },
    /// mDNS found a peer. `trusted` says whether we will dial it.
    Discovered { peer: PeerId, trusted: bool },
    /// Our relay accepted a reservation; we are reachable via a circuit.
    ReservationAccepted { relay: PeerId },
    /// DCUtR finished.
    HolePunch {
        peer: PeerId,
        ok: bool,
        detail: String,
    },
    /// AutoNAT status changed (e.g. "Public(..)", "Private").
    Nat(String),
    /// Pairing finished successfully.
    Paired { peer: PeerId },
}

#[derive(NetworkBehaviour)]
struct Behaviour {
    relay_client: relay::client::Behaviour,
    ping: ping::Behaviour,
    identify: identify::Behaviour,
    autonat: autonat::Behaviour,
    dcutr: dcutr::Behaviour,
    mdns: Toggle<mdns::tokio::Behaviour>,
    stream: libp2p_stream::Behaviour,
}

enum Cmd {
    Dial(Multiaddr, oneshot::Sender<Result<(), NetError>>),
    Listen(Multiaddr, oneshot::Sender<Result<(), NetError>>),
    Addrs(oneshot::Sender<Vec<Multiaddr>>),
    AddPeerAddr(PeerId, Multiaddr),
    AllowPeer(PeerId, Duration),
    AddInfra(PeerId),
    AddExternal(Multiaddr),
    Connected(PeerId, oneshot::Sender<bool>),
}

/// Handle to a running node. Cheap to clone. Dropping every handle stops the node.
#[derive(Clone)]
pub struct Node {
    peer_id: PeerId,
    control: libp2p_stream::Control,
    cmd: mpsc::Sender<Cmd>,
    events: broadcast::Sender<NodeEvent>,
    trust: TrustStore,
    ledger: Arc<Ledger>,
    offer: Arc<Mutex<Option<HostOffer>>>,
    prompts: Arc<Mutex<Option<mpsc::Receiver<PairingPrompt>>>>,
}

impl Node {
    /// Builds the swarm (QUIC + TCP/Noise/Yamux + relay client), starts listening and spawns the event loop.
    pub async fn start(config: NodeConfig) -> Result<Node, NetError> {
        let peer_id = config.keypair.public().to_peer_id();
        let mdns_on = config.mdns;
        let mut swarm = libp2p::SwarmBuilder::with_existing_identity(config.keypair)
            .with_tokio()
            .with_tcp(
                tcp::Config::default().nodelay(true),
                noise::Config::new,
                yamux::Config::default,
            )
            .map_err(|e| NetError::Transport(e.to_string()))?
            .with_quic()
            .with_dns()
            .map_err(|e| NetError::Transport(e.to_string()))?
            .with_relay_client(noise::Config::new, yamux::Config::default)
            .map_err(|e| NetError::Transport(e.to_string()))?
            .with_behaviour(|key, relay_client| {
                let pid = key.public().to_peer_id();
                let mdns = if mdns_on {
                    Some(mdns::tokio::Behaviour::new(mdns::Config::default(), pid)?)
                } else {
                    None
                };
                Ok(Behaviour {
                    relay_client,
                    ping: ping::Behaviour::default(),
                    identify: identify::Behaviour::new(identify::Config::new(
                        "/nc/id/1.0.0".into(),
                        key.public(),
                    )),
                    autonat: autonat::Behaviour::new(pid, autonat::Config::default()),
                    dcutr: dcutr::Behaviour::new(pid),
                    mdns: Toggle::from(mdns),
                    stream: libp2p_stream::Behaviour::new(),
                })
            })
            .map_err(|e| NetError::Transport(e.to_string()))?
            .with_swarm_config(|c| c.with_idle_connection_timeout(config.idle_timeout))
            .build();

        for addr in &config.listen {
            swarm
                .listen_on(addr.clone())
                .map_err(|e| NetError::Transport(e.to_string()))?;
        }

        let control = swarm.behaviour().stream.new_control();
        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let (ev_tx, _) = broadcast::channel(256);
        let (prompt_tx, prompt_rx) = mpsc::channel(8);
        let ledger = Arc::new(Ledger::default());
        let offer = Arc::new(Mutex::new(None));

        let node = Node {
            peer_id,
            control: control.clone(),
            cmd: cmd_tx,
            events: ev_tx.clone(),
            trust: config.trust.clone(),
            ledger: ledger.clone(),
            offer: offer.clone(),
            prompts: Arc::new(Mutex::new(Some(prompt_rx))),
        };

        let task = SwarmTask {
            swarm,
            events: ev_tx.clone(),
            trust: config.trust.clone(),
            infra: config.infra_peers.into_iter().collect(),
            allow_until: HashMap::new(),
            untrusted_since: HashMap::new(),
            offer: offer.clone(),
            ledger,
        };
        tokio::spawn(task.run(cmd_rx));
        tokio::spawn(pairing_responder(
            control,
            peer_id,
            offer,
            config.trust,
            prompt_tx,
            ev_tx,
        ));
        Ok(node)
    }

    /// This device's peer key.
    pub fn peer_id(&self) -> PeerId {
        self.peer_id
    }

    /// Trust store in use.
    pub fn trust(&self) -> &TrustStore {
        &self.trust
    }

    /// Subscribe to node events.
    pub fn events(&self) -> broadcast::Receiver<NodeEvent> {
        self.events.subscribe()
    }

    async fn send(&self, c: Cmd) -> Result<(), NetError> {
        self.cmd.send(c).await.map_err(|_| NetError::Stopped)
    }

    /// Current listen and confirmed external addresses.
    pub async fn listen_addrs(&self) -> Result<Vec<Multiaddr>, NetError> {
        let (tx, rx) = oneshot::channel();
        self.send(Cmd::Addrs(tx)).await?;
        rx.await.map_err(|_| NetError::Stopped)
    }

    /// Dials an address. The connection is still gated by trust once established.
    pub async fn dial(&self, addr: Multiaddr) -> Result<(), NetError> {
        let (tx, rx) = oneshot::channel();
        self.send(Cmd::Dial(addr, tx)).await?;
        rx.await.map_err(|_| NetError::Stopped)?
    }

    /// Listens on an extra address, e.g. `<relay>/p2p-circuit` for a relay reservation.
    pub async fn listen_on(&self, addr: Multiaddr) -> Result<(), NetError> {
        let (tx, rx) = oneshot::channel();
        self.send(Cmd::Listen(addr, tx)).await?;
        rx.await.map_err(|_| NetError::Stopped)?
    }

    /// Declares an address as externally reachable (e.g. a port-forwarded public address).
    pub async fn add_external_addr(&self, addr: Multiaddr) -> Result<(), NetError> {
        self.send(Cmd::AddExternal(addr)).await
    }

    /// Adds an infrastructure (relay) peer that may connect without pairing.
    pub async fn add_infra_peer(&self, peer: PeerId) -> Result<(), NetError> {
        self.send(Cmd::AddInfra(peer)).await
    }

    /// True if at least one connection to `peer` is open.
    pub async fn is_connected(&self, peer: PeerId) -> Result<bool, NetError> {
        let (tx, rx) = oneshot::channel();
        self.send(Cmd::Connected(peer, tx)).await?;
        rx.await.map_err(|_| NetError::Stopped)
    }

    /// Presents a relay token (from the coordinator) to relay `relay`. Must be connected to it.
    pub async fn authorize_relay(&self, relay: PeerId, token: &[u8]) -> Result<(), NetError> {
        let mut c = self.control.clone();
        tokio::time::timeout(
            Duration::from_secs(15),
            relay_auth::present_token(&mut c, relay, token),
        )
        .await
        .map_err(|_| NetError::Timeout)?
    }

    /// Live connections to `peer` with their ladder rung.
    pub fn connections(&self, peer: &PeerId) -> Vec<ConnectionInfo> {
        self.ledger.connections(peer)
    }

    /// Direct vs relayed stream bytes so far.
    pub fn traffic(&self) -> TrafficStats {
        self.ledger.traffic()
    }

    // ---- pairing ----

    /// Creates a new single-use offer (replacing any previous one) for the QR code.
    pub async fn create_pairing_offer(&self, ttl: Duration) -> Result<PairingOffer, NetError> {
        let mut addrs = self.listen_addrs().await?;
        addrs.retain(|a| {
            !a.iter()
                .any(|p| matches!(p, Protocol::Ip4(ip) if ip.is_unspecified()))
        });
        let offer = PairingOffer::new(self.peer_id, addrs, ttl);
        let mut g = self
            .offer
            .lock()
            .map_err(|_| NetError::Pairing("state poisoned".into()))?;
        *g = Some(HostOffer {
            offer: offer.clone(),
            failed: 0,
            used: false,
            busy: false,
        });
        Ok(offer)
    }

    /// Cancels the active offer.
    pub fn cancel_pairing_offer(&self) {
        if let Ok(mut g) = self.offer.lock() {
            *g = None;
        }
    }

    /// Takes the receiver of SAS prompts for incoming pairings (responder side). Only one taker.
    pub fn take_pairing_prompts(&self) -> Option<mpsc::Receiver<PairingPrompt>> {
        self.prompts.lock().ok().and_then(|mut g| g.take())
    }

    /// Pairs with the device that showed `offer` (initiator side). `confirm` receives the SAS and must
    /// return `true` only if the user saw the same digits on the other device. On success the peer is trusted.
    pub async fn pair<F, Fut>(&self, offer: &PairingOffer, confirm: F) -> Result<PeerId, NetError>
    where
        F: FnOnce(String) -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let peer = offer.peer_id;
        self.send(Cmd::AllowPeer(peer, PAIRING_GRACE)).await?;
        for a in &offer.addrs {
            let a = with_peer(a.clone(), peer);
            self.send(Cmd::AddPeerAddr(peer, a.clone())).await?;
            // Dial each offered address by peer id; the transport handshake rejects a different key.
            let _ = self.dial(a).await;
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        while !self.is_connected(peer).await? {
            if tokio::time::Instant::now() > deadline {
                return Err(NetError::Pairing(
                    "could not reach the device in the offer".into(),
                ));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let mut control = self.control.clone();
        let mut s = tokio::time::timeout(
            Duration::from_secs(30),
            control.open_stream(peer, PAIR_PROTOCOL),
        )
        .await
        .map_err(|_| NetError::Timeout)?
        .map_err(|e| NetError::Stream(e.to_string()))?;
        let res = tokio::time::timeout(
            SAS_TIMEOUT + Duration::from_secs(30),
            pairing::run_initiator(&mut s, offer, self.peer_id, peer, confirm),
        )
        .await
        .map_err(|_| NetError::Timeout)?;
        let _ = futures::AsyncWriteExt::close(&mut s).await;
        let trusted = match res {
            Ok(()) => self.trust.add(peer),
            Err(e) => Err(e),
        };
        // End the pairing window either way: on failure the gate now closes the connection.
        self.send(Cmd::AllowPeer(peer, Duration::ZERO)).await?;
        trusted?;
        let _ = self.events.send(NodeEvent::Paired { peer });
        tracing::info!(%peer, "paired (initiator)");
        Ok(peer)
    }

    // ---- stream API for Core ----

    /// Opens a stream to a **trusted** peer on `protocol`. Untrusted peers are refused before any I/O.
    pub async fn open_stream(
        &self,
        peer: PeerId,
        protocol: StreamProtocol,
    ) -> Result<PeerStream, NetError> {
        check_protocol(&protocol)?;
        if !self.trust.contains(&peer) {
            return Err(NetError::Untrusted(peer));
        }
        // While a direct and a relayed connection coexist, libp2p-stream picks one at random, so the
        // byte accounting would be a guess. The relayed one is closed shortly; wait for that.
        let settle = tokio::time::Instant::now() + RELAY_CLOSE_DELAY + Duration::from_secs(2);
        while self.ledger.peer_is_mixed(&peer) && tokio::time::Instant::now() < settle {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let mut c = self.control.clone();
        let s = tokio::time::timeout(Duration::from_secs(30), c.open_stream(peer, protocol))
            .await
            .map_err(|_| NetError::Timeout)?
            .map_err(|e| NetError::Stream(e.to_string()))?;
        let relayed = !self.ledger.peer_has_direct(&peer);
        Ok(PeerStream::new(
            peer,
            s,
            self.ledger.counter_for(&peer),
            relayed,
        ))
    }

    /// Accepts incoming streams on `protocol`. Streams from untrusted peers are dropped and never surface.
    pub fn accept(&self, protocol: StreamProtocol) -> Result<IncomingPeerStreams, NetError> {
        check_protocol(&protocol)?;
        let mut c = self.control.clone();
        let mut inc = c
            .accept(protocol)
            .map_err(|_| NetError::Stream("protocol already accepted".into()))?;
        let (tx, rx) = mpsc::channel(16);
        let trust = self.trust.clone();
        let ledger = self.ledger.clone();
        let events = self.events.clone();
        tokio::spawn(async move {
            while let Some((peer, s)) = inc.next().await {
                if !trust.contains(&peer) {
                    tracing::warn!(%peer, "dropped stream from untrusted peer");
                    let _ = events.send(NodeEvent::Rejected { peer });
                    drop(s);
                    continue;
                }
                let relayed = !ledger.peer_has_direct(&peer);
                let ps = PeerStream::new(peer, s, ledger.counter_for(&peer), relayed);
                if tx.send(ps).await.is_err() {
                    break;
                }
            }
        });
        Ok(IncomingPeerStreams { rx })
    }
}

/// Incoming streams from trusted peers only.
pub struct IncomingPeerStreams {
    rx: mpsc::Receiver<PeerStream>,
}

impl IncomingPeerStreams {
    /// Next stream; `None` when the node stops.
    pub async fn next(&mut self) -> Option<PeerStream> {
        self.rx.recv().await
    }
}

fn check_protocol(p: &StreamProtocol) -> Result<(), NetError> {
    if *p == PAIR_PROTOCOL || *p == RELAY_AUTH_PROTOCOL {
        return Err(NetError::ReservedProtocol);
    }
    Ok(())
}

fn with_peer(mut a: Multiaddr, peer: PeerId) -> Multiaddr {
    if !matches!(a.iter().last(), Some(Protocol::P2p(_))) {
        a.push(Protocol::P2p(peer));
    }
    a
}

async fn pairing_responder(
    mut control: libp2p_stream::Control,
    local: PeerId,
    offer: Arc<Mutex<Option<HostOffer>>>,
    trust: TrustStore,
    prompts: mpsc::Sender<PairingPrompt>,
    events: broadcast::Sender<NodeEvent>,
) {
    let Ok(mut inc) = control.accept(PAIR_PROTOCOL) else {
        return;
    };
    while let Some((peer, mut s)) = inc.next().await {
        let offer = offer.clone();
        let trust = trust.clone();
        let prompts = prompts.clone();
        let events = events.clone();
        tokio::spawn(async move {
            // Claim the offer (one pairing at a time).
            let secret = {
                let Ok(mut g) = offer.lock() else { return };
                match g.as_mut() {
                    Some(h) if h.usable() => {
                        h.busy = true;
                        Some(h.secret())
                    }
                    _ => None,
                }
            };
            let Some(secret) = secret else {
                pairing::responder_refuse(&mut s).await;
                return;
            };
            let release = |failed: bool, used: bool| {
                if let Ok(mut g) = offer.lock() {
                    if let Some(h) = g.as_mut() {
                        h.busy = false;
                        if failed {
                            h.failed += 1;
                        }
                        if used {
                            h.used = true;
                        }
                    }
                }
            };
            let stage = tokio::time::timeout(
                Duration::from_secs(15),
                pairing::responder_verify(&mut s, &secret, local, peer),
            )
            .await;
            let sas = match stage {
                Ok(Ok(ResponderStage::Verified { sas })) => sas,
                Ok(Ok(ResponderStage::BadProof)) => {
                    tracing::warn!(%peer, "pairing proof rejected");
                    release(true, false);
                    let _ = events.send(NodeEvent::Rejected { peer });
                    return;
                }
                _ => {
                    release(true, false);
                    return;
                }
            };
            let (tx, rx) = oneshot::channel();
            let local_ok = if prompts
                .send(PairingPrompt {
                    peer,
                    sas,
                    reply: tx,
                })
                .await
                .is_ok()
            {
                matches!(tokio::time::timeout(SAS_TIMEOUT, rx).await, Ok(Ok(true)))
            } else {
                false
            };
            let both = matches!(
                tokio::time::timeout(
                    Duration::from_secs(SAS_TIMEOUT.as_secs() + 30),
                    pairing::responder_confirm(&mut s, local_ok)
                )
                .await,
                Ok(Ok(true))
            );
            if both && trust.add(peer).is_ok() {
                release(false, true);
                tracing::info!(%peer, "paired (responder)");
                let _ = events.send(NodeEvent::Paired { peer });
            } else {
                release(false, false);
            }
        });
    }
}

struct SwarmTask {
    swarm: Swarm<Behaviour>,
    events: broadcast::Sender<NodeEvent>,
    trust: TrustStore,
    infra: HashSet<PeerId>,
    allow_until: HashMap<PeerId, Instant>,
    untrusted_since: HashMap<PeerId, Instant>,
    offer: Arc<Mutex<Option<HostOffer>>>,
    ledger: Arc<Ledger>,
}

impl SwarmTask {
    async fn run(mut self, mut cmds: mpsc::Receiver<Cmd>) {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                ev = self.swarm.select_next_some() => self.on_event(ev),
                c = cmds.recv() => match c {
                    Some(c) => self.on_cmd(c),
                    None => break,
                },
                _ = tick.tick() => self.sweep(),
            }
        }
    }

    fn emit(&self, e: NodeEvent) {
        let _ = self.events.send(e);
    }

    fn offer_open(&self) -> bool {
        self.offer
            .lock()
            .map(|g| g.as_ref().is_some_and(|h| h.usable() || h.busy))
            .unwrap_or(false)
    }

    /// Gate: may this peer keep a connection?
    fn allowed(&self, peer: &PeerId, now: Instant) -> bool {
        if self.trust.contains(peer) || self.infra.contains(peer) {
            return true;
        }
        if self.allow_until.get(peer).is_some_and(|t| *t > now) {
            return true;
        }
        self.offer_open()
            && self
                .untrusted_since
                .get(peer)
                .is_none_or(|t| now.duration_since(*t) < PAIRING_GRACE)
    }

    fn on_cmd(&mut self, c: Cmd) {
        match c {
            Cmd::Dial(a, tx) => {
                let r = self
                    .swarm
                    .dial(a)
                    .map_err(|e| NetError::Transport(e.to_string()));
                let _ = tx.send(r);
            }
            Cmd::Listen(a, tx) => {
                let r = self
                    .swarm
                    .listen_on(a)
                    .map(|_| ())
                    .map_err(|e| NetError::Transport(e.to_string()));
                let _ = tx.send(r);
            }
            Cmd::Addrs(tx) => {
                let mut v: Vec<Multiaddr> = self.swarm.listeners().cloned().collect();
                v.extend(self.swarm.external_addresses().cloned());
                v.dedup();
                let _ = tx.send(v);
            }
            Cmd::AddPeerAddr(p, a) => {
                self.swarm.add_peer_address(p, a);
            }
            Cmd::AllowPeer(p, d) => {
                self.allow_until.insert(p, Instant::now() + d);
            }
            Cmd::AddInfra(p) => {
                self.infra.insert(p);
            }
            Cmd::AddExternal(a) => self.swarm.add_external_address(a),
            Cmd::Connected(p, tx) => {
                let _ = tx.send(self.swarm.is_connected(&p));
            }
        }
    }

    fn on_event(&mut self, ev: SwarmEvent<BehaviourEvent>) {
        let now = Instant::now();
        match ev {
            SwarmEvent::NewListenAddr { address, .. } => {
                tracing::info!(%address, "listening");
                self.emit(NodeEvent::ListenAddr(address));
            }
            SwarmEvent::ConnectionEstablished {
                peer_id,
                connection_id,
                endpoint,
                ..
            } => {
                let addr = match &endpoint {
                    ConnectedPoint::Dialer { address, .. } => address.clone(),
                    ConnectedPoint::Listener { send_back_addr, .. } => send_back_addr.clone(),
                };
                if !self.trust.contains(&peer_id) && !self.infra.contains(&peer_id) {
                    self.untrusted_since.entry(peer_id).or_insert(now);
                }
                if !self.allowed(&peer_id, now) {
                    tracing::warn!(peer = %peer_id, "closing connection from unpaired peer");
                    self.swarm.close_connection(connection_id);
                    self.emit(NodeEvent::Rejected { peer: peer_id });
                    return;
                }
                let rung = classify_addr(&addr);
                tracing::info!(peer = %peer_id, ?rung, "connection established");
                self.ledger.insert(
                    connection_id,
                    ConnEntry {
                        peer: peer_id,
                        rung,
                        remote_addr: addr.clone(),
                        since: now,
                    },
                );
                self.emit(NodeEvent::Connected {
                    peer: peer_id,
                    rung,
                    addr,
                });
            }
            SwarmEvent::ConnectionClosed {
                peer_id,
                connection_id,
                num_established,
                ..
            } => {
                self.ledger.remove(&connection_id);
                if num_established == 0 {
                    self.untrusted_since.remove(&peer_id);
                    self.emit(NodeEvent::Disconnected { peer: peer_id });
                }
            }
            SwarmEvent::Behaviour(BehaviourEvent::Mdns(mdns::Event::Discovered(list))) => {
                for (peer, addr) in list {
                    let trusted = self.trust.contains(&peer);
                    self.emit(NodeEvent::Discovered { peer, trusted });
                    // Discovery is not authorisation: only dial peers we already paired with.
                    if trusted {
                        self.swarm.add_peer_address(peer, addr.clone());
                        if !self.swarm.is_connected(&peer) {
                            let _ = self.swarm.dial(with_peer(addr, peer));
                        }
                    }
                }
            }
            SwarmEvent::Behaviour(BehaviourEvent::RelayClient(
                relay::client::Event::ReservationReqAccepted { relay_peer_id, .. },
            )) => {
                tracing::info!(relay = %relay_peer_id, "relay reservation accepted");
                self.emit(NodeEvent::ReservationAccepted {
                    relay: relay_peer_id,
                });
            }
            SwarmEvent::Behaviour(BehaviourEvent::Dcutr(dcutr::Event {
                remote_peer_id,
                result,
            })) => {
                match result {
                    Ok(conn) => {
                        // The relay circuit is closed later by `sweep`, not here: it may still carry a
                        // pairing exchange that is waiting for the user.
                        self.ledger.mark_hole_punched(&conn);
                        self.emit(NodeEvent::HolePunch {
                            peer: remote_peer_id,
                            ok: true,
                            detail: String::new(),
                        });
                    }
                    Err(e) => {
                        self.emit(NodeEvent::HolePunch {
                            peer: remote_peer_id,
                            ok: false,
                            detail: e.to_string(),
                        });
                    }
                }
            }
            SwarmEvent::Behaviour(BehaviourEvent::Autonat(autonat::Event::StatusChanged {
                new,
                ..
            })) => {
                self.emit(NodeEvent::Nat(format!("{new:?}")));
            }
            SwarmEvent::Behaviour(BehaviourEvent::Identify(identify::Event::Received {
                peer_id,
                info,
                ..
            })) if self.trust.contains(&peer_id) || self.infra.contains(&peer_id) => {
                // Learn addresses of trusted/infra peers only.
                for a in info.listen_addrs {
                    self.swarm.add_peer_address(peer_id, a);
                }
            }
            _ => {}
        }
    }

    fn sweep(&mut self) {
        let now = Instant::now();
        self.allow_until.retain(|_, t| *t > now);
        // Close unpaired connections whose pairing window ended.
        let stale: Vec<PeerId> = self
            .untrusted_since
            .keys()
            .copied()
            .filter(|p| !self.allowed(p, now))
            .collect();
        for p in stale {
            if self.swarm.is_connected(&p) {
                tracing::warn!(peer = %p, "closing unpaired connection (pairing window over)");
                let _ = self.swarm.disconnect_peer_id(p);
                self.emit(NodeEvent::Rejected { peer: p });
            }
            self.untrusted_since.remove(&p);
        }
        // Prefer direct: drop relayed connections once a direct one has been up for a moment.
        let to_close: Vec<ConnectionId> = match self.ledger.conns.lock() {
            Ok(c) => {
                let direct_peers: HashSet<PeerId> = c
                    .values()
                    .filter(|e| {
                        e.rung.is_direct() && now.duration_since(e.since) >= RELAY_CLOSE_DELAY
                    })
                    .map(|e| e.peer)
                    .collect();
                // Never while pairing: the pairing stream may be on the circuit, waiting for the user.
                // (Initiator: peer in `allow_until`; responder: peer not yet trusted.)
                c.iter()
                    .filter(|(_, e)| {
                        e.rung == Rung::Relayed
                            && direct_peers.contains(&e.peer)
                            && self.trust.contains(&e.peer)
                            && !self.allow_until.contains_key(&e.peer)
                    })
                    .map(|(id, _)| *id)
                    .collect()
            }
            Err(_) => Vec::new(),
        };
        for id in to_close {
            self.swarm.close_connection(id);
        }
    }
}
