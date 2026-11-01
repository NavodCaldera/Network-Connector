//! `nc-netprobe`: Phase 0 test kit (task 0.5) for direct / hole-punched / relayed connections between two
//! machines on different ISPs. See `spikes/phase0/net/RUN.md` for exact steps.
//!
//! Modes:
//! - `id`     print this machine's probe peer id (creates the key file if needed)
//! - `relay`  run a dev relay (circuit relay v2) that serves **only** the peer ids given with `--allow`
//! - `listen` wait for a peer: print a pairing offer, serve the throughput test
//! - `dial`   pair using the offer printed by `listen`, wait for a direct upgrade, run the throughput test
//!
//! Output: human-readable lines plus one JSON block between `=== NC-NETPROBE RESULT ===` markers to paste
//! back. The offer text contains a one-time secret: paste it only into the other probe, not into reports.

use std::collections::HashSet;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use futures::{AsyncReadExt, AsyncWriteExt, StreamExt};
use libp2p::multiaddr::Protocol;
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{autonat, identify, identity, noise, ping, relay, tcp, yamux, Multiaddr, PeerId};
use nc_net::pairing::DEFAULT_OFFER_TTL;
use nc_net::{Node, NodeConfig, NodeEvent, PairingOffer, PeerStream, StreamProtocol, TrustStore};
use serde::Serialize;

const PROBE: StreamProtocol = StreamProtocol::new("/nc/probe/1.0.0");

#[derive(Parser)]
#[command(version, about = "Network Connector Phase 0 connectivity probe")]
struct Cli {
    /// Key file for this machine's probe identity (created if missing). Not your device key.
    #[arg(long, default_value = "nc-probe.key", global = true)]
    key: PathBuf,
    /// IPv4 address to bind. Default all interfaces (needed for LAN/internet tests);
    /// use 127.0.0.1 for a single-PC self-test.
    #[arg(long, default_value = "0.0.0.0", global = true)]
    bind: std::net::Ipv4Addr,
    #[command(subcommand)]
    mode: Mode,
}

static BIND: std::sync::OnceLock<std::net::Ipv4Addr> = std::sync::OnceLock::new();

#[derive(Subcommand)]
enum Mode {
    /// Print this machine's probe peer id.
    Id,
    /// Run a dev relay. Only `--allow`ed peer ids may reserve or open circuits.
    Relay {
        /// UDP (QUIC) and TCP port to listen on, on all interfaces (a relay must be reachable from outside).
        #[arg(long, default_value_t = 4001)]
        port: u16,
        /// Peer ids allowed to use the relay (from `nc-netprobe id` on each PC). Required.
        #[arg(long, required = true)]
        allow: Vec<PeerId>,
        /// Max bytes per circuit.
        #[arg(long, default_value_t = 256 * 1024 * 1024)]
        max_circuit_bytes: u64,
        /// Max circuit duration in seconds.
        #[arg(long, default_value_t = 600)]
        max_circuit_secs: u64,
    },
    /// Wait for the other PC: print an offer and serve the throughput test.
    Listen {
        /// UDP (QUIC) and TCP port to listen on. 0 = random.
        #[arg(long, default_value_t = 4002)]
        port: u16,
        /// Relay address incl. `/p2p/<relay id>`, to be reachable behind NAT.
        #[arg(long)]
        relay: Option<Multiaddr>,
        /// Relay token from the coordinator (for nc-relay; the dev relay uses --allow instead).
        #[arg(long)]
        relay_token: Option<String>,
        /// Accept the SAS without asking (only for unattended runs where you compare by eye).
        #[arg(long)]
        yes: bool,
        /// Stop after this many seconds.
        #[arg(long, default_value_t = 1800)]
        max_secs: u64,
    },
    /// Pair with the offer printed by `listen` and run the test.
    Dial {
        /// The `nc-pair:v1:...` text printed by `listen`.
        offer: String,
        /// Relay address incl. `/p2p/<relay id>` (same as the listener used).
        #[arg(long)]
        relay: Option<Multiaddr>,
        /// Relay token from the coordinator (for nc-relay).
        #[arg(long)]
        relay_token: Option<String>,
        /// Seconds per direction of the throughput test.
        #[arg(long, default_value_t = 10)]
        test_secs: u64,
        /// Seconds to wait for a direct/hole-punched connection before testing over whatever exists.
        #[arg(long, default_value_t = 30)]
        upgrade_wait_secs: u64,
        /// Accept the SAS without asking.
        #[arg(long)]
        yes: bool,
        /// Port to listen on locally (helps hole punching). 0 = random.
        #[arg(long, default_value_t = 4003)]
        port: u16,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,libp2p=warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let _ = BIND.set(cli.bind);
    let key = load_or_create_key(&cli.key)?;
    match cli.mode {
        Mode::Id => {
            println!("{}", key.public().to_peer_id());
            Ok(())
        }
        Mode::Relay {
            port,
            allow,
            max_circuit_bytes,
            max_circuit_secs,
        } => run_relay(key, port, allow, max_circuit_bytes, max_circuit_secs).await,
        Mode::Listen {
            port,
            relay,
            relay_token,
            yes,
            max_secs,
        } => run_listen(key, port, relay, relay_token, yes, max_secs).await,
        Mode::Dial {
            offer,
            relay,
            relay_token,
            test_secs,
            upgrade_wait_secs,
            yes,
            port,
        } => {
            run_dial(
                key,
                offer,
                relay,
                relay_token,
                test_secs,
                upgrade_wait_secs,
                yes,
                port,
            )
            .await
        }
    }
}

fn load_or_create_key(path: &PathBuf) -> Result<identity::Keypair> {
    if let Ok(bytes) = std::fs::read(path) {
        return identity::Keypair::from_protobuf_encoding(&bytes).context("bad key file");
    }
    let k = identity::Keypair::generate_ed25519();
    std::fs::write(path, k.to_protobuf_encoding().context("encode key")?)?;
    eprintln!("created probe key {}", path.display());
    Ok(k)
}

fn listen_addrs(port: u16) -> Vec<Multiaddr> {
    // Probe default: all interfaces, so LAN and public paths can be tested (`--bind` narrows it).
    let ip = BIND
        .get()
        .copied()
        .unwrap_or(std::net::Ipv4Addr::UNSPECIFIED);
    vec![
        format!("/ip4/{ip}/udp/{port}/quic-v1")
            .parse()
            .unwrap_or_else(|_| Multiaddr::empty()),
        format!("/ip4/{ip}/tcp/{port}")
            .parse()
            .unwrap_or_else(|_| Multiaddr::empty()),
    ]
}

fn relay_peer(addr: &Multiaddr) -> Result<PeerId> {
    match addr.iter().last() {
        Some(Protocol::P2p(p)) => Ok(p),
        _ => bail!("relay address must end with /p2p/<relay peer id>"),
    }
}

fn ts() -> String {
    format!(
        "{:?}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
    )
}

fn spawn_event_printer(node: &Node, log: Arc<Mutex<Vec<String>>>) {
    let mut ev = node.events();
    tokio::spawn(async move {
        while let Ok(e) = ev.recv().await {
            let line = match &e {
                NodeEvent::ListenAddr(a) => format!("listen {a}"),
                NodeEvent::Connected { peer, rung, addr } => {
                    format!("connected {peer} rung={rung:?} via {addr}")
                }
                NodeEvent::Disconnected { peer } => format!("disconnected {peer}"),
                NodeEvent::Rejected { peer } => format!("REJECTED unpaired peer {peer}"),
                NodeEvent::Discovered { peer, trusted } => {
                    format!("mdns discovered {peer} trusted={trusted}")
                }
                NodeEvent::ReservationAccepted { relay } => {
                    format!("relay reservation accepted by {relay}")
                }
                NodeEvent::HolePunch { peer, ok, detail } => {
                    format!("dcutr {peer} ok={ok} {detail}")
                }
                NodeEvent::Nat(s) => format!("autonat {s}"),
                NodeEvent::Paired { peer } => format!("paired {peer}"),
            };
            println!("[{}] {line}", ts());
            if let Ok(mut l) = log.lock() {
                l.push(line);
            }
        }
    });
}

async fn start_node(key: identity::Keypair, port: u16, relay: Option<&Multiaddr>) -> Result<Node> {
    let mut cfg = NodeConfig::new(key);
    cfg.listen = listen_addrs(port);
    // mDNS binds UDP 5353 on all interfaces; skip it for a loopback self-test.
    cfg.mdns = !BIND.get().is_some_and(|ip| ip.is_loopback());
    cfg.trust = TrustStore::in_memory();
    if let Some(r) = relay {
        cfg.infra_peers.push(relay_peer(r)?);
    }
    Ok(Node::start(cfg).await?)
}

async fn connect_relay(
    node: &Node,
    relay: &Multiaddr,
    token: Option<&str>,
    reserve: bool,
) -> Result<()> {
    let rp = relay_peer(relay)?;
    node.dial(relay.clone()).await?;
    let deadline = Instant::now() + Duration::from_secs(20);
    while !node.is_connected(rp).await? {
        if Instant::now() > deadline {
            bail!("could not connect to relay {relay}");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    if let Some(t) = token {
        node.authorize_relay(rp, t.as_bytes()).await?;
        println!("relay token accepted");
    }
    if reserve {
        node.listen_on(relay.clone().with(Protocol::P2pCircuit))
            .await?;
    }
    Ok(())
}

async fn ask_sas(sas: &str, yes: bool) -> bool {
    println!("\n  SHORT AUTHENTICATION STRING: {sas}\n  Check that the other PC shows the same 6 digits.");
    if yes {
        println!("  (--yes given: accepting)");
        return true;
    }
    println!("  Type y and Enter if they match:");
    tokio::task::spawn_blocking(|| {
        let mut s = String::new();
        std::io::stdin().read_line(&mut s).ok();
        s.trim().eq_ignore_ascii_case("y")
    })
    .await
    .unwrap_or(false)
}

async fn run_listen(
    key: identity::Keypair,
    port: u16,
    relay: Option<Multiaddr>,
    token: Option<String>,
    yes: bool,
    max_secs: u64,
) -> Result<()> {
    let node = start_node(key, port, relay.as_ref()).await?;
    let log = Arc::new(Mutex::new(Vec::new()));
    spawn_event_printer(&node, log.clone());
    println!("probe peer id: {}", node.peer_id());
    if let Some(r) = &relay {
        connect_relay(&node, r, token.as_deref(), true).await?;
        // Wait for the reservation so the offer contains the circuit address.
        let mut ev = node.events();
        let _ = tokio::time::timeout(Duration::from_secs(20), async {
            while let Ok(e) = ev.recv().await {
                if matches!(e, NodeEvent::ReservationAccepted { .. }) {
                    break;
                }
            }
        })
        .await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    let offer = node.create_pairing_offer(DEFAULT_OFFER_TTL).await?;
    println!("\nOffer addresses: {:?}", offer.addrs);
    println!("\nCopy this ONE line to the other PC (it is a one-time secret, valid 10 min; do not paste it in reports):\n");
    println!("{}\n", offer.to_qr_string());

    let mut prompts = node.take_pairing_prompts().context("prompts")?;
    tokio::spawn(async move {
        while let Some(p) = prompts.recv().await {
            println!("pairing request from {}", p.peer);
            let ok = ask_sas(&p.sas, yes).await;
            let _ = p.reply.send(ok);
        }
    });

    let mut inc = node.accept(PROBE)?;
    let n2 = node.clone();
    tokio::spawn(async move {
        while let Some(s) = inc.next().await {
            let n3 = n2.clone();
            tokio::spawn(async move {
                if let Err(e) = serve_probe(s).await {
                    println!("probe stream ended: {e}");
                }
                println!(
                    "traffic so far: {}",
                    serde_json::to_string(&n3.traffic()).unwrap_or_default()
                );
            });
        }
    });
    println!("waiting (Ctrl+C to stop, max {max_secs}s)...");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        _ = tokio::time::sleep(Duration::from_secs(max_secs)) => {},
    }
    println!("final traffic: {}", serde_json::to_string(&node.traffic())?);
    Ok(())
}

/// Server side: first byte `U` (client uploads for N secs) or `D` (we send for N secs), then u32 seconds.
async fn serve_probe(mut s: PeerStream) -> Result<()> {
    let mut hdr = [0u8; 5];
    s.read_exact(&mut hdr).await?;
    let secs = u64::from(u32::from_be_bytes([hdr[1], hdr[2], hdr[3], hdr[4]])).min(120);
    match hdr[0] {
        b'U' => {
            let mut buf = vec![0u8; 64 * 1024];
            let mut total: u64 = 0;
            loop {
                let n = s.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                total += n as u64;
            }
            s.write_all(&total.to_be_bytes()).await?;
            s.close().await?;
        }
        b'D' => {
            let buf = vec![0xA5u8; 64 * 1024];
            let end = Instant::now() + Duration::from_secs(secs);
            while Instant::now() < end {
                s.write_all(&buf).await?;
            }
            s.close().await?;
        }
        _ => bail!("bad probe header"),
    }
    Ok(())
}

#[derive(Serialize)]
struct Result_ {
    local_peer: String,
    remote_peer: String,
    connections_at_test: Vec<nc_net::ConnectionInfo>,
    upload_mbit_s: f64,
    upload_bytes: u64,
    download_mbit_s: f64,
    download_bytes: u64,
    traffic: nc_net::TrafficStats,
    stream_relayed: bool,
    pair_secs: f64,
    events: Vec<String>,
}

#[allow(clippy::too_many_arguments)]
async fn run_dial(
    key: identity::Keypair,
    offer: String,
    relay: Option<Multiaddr>,
    token: Option<String>,
    test_secs: u64,
    upgrade_wait_secs: u64,
    yes: bool,
    port: u16,
) -> Result<()> {
    let offer = PairingOffer::parse(&offer)?;
    let node = start_node(key, port, relay.as_ref()).await?;
    let log = Arc::new(Mutex::new(Vec::new()));
    spawn_event_printer(&node, log.clone());
    println!("probe peer id: {}", node.peer_id());
    if let Some(r) = &relay {
        // Not reserving: we only dial through the relay.
        connect_relay(&node, r, token.as_deref(), false).await?;
    }
    let t0 = Instant::now();
    let remote = node
        .pair(&offer, |sas| async move { ask_sas(&sas, yes).await })
        .await?;
    let pair_secs = t0.elapsed().as_secs_f64();
    println!("paired with {remote} in {pair_secs:.2}s");

    // Wait for a direct (LAN / internet / hole-punched) connection, if one is possible.
    let deadline = Instant::now() + Duration::from_secs(upgrade_wait_secs);
    loop {
        let c = node.connections(&remote);
        let direct = c.iter().any(|c| c.rung.is_direct());
        let relayed = c.iter().any(|c| !c.rung.is_direct());
        if direct && !relayed {
            break;
        }
        if Instant::now() > deadline {
            println!("no direct-only path after {upgrade_wait_secs}s; testing over what exists");
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let connections_at_test = node.connections(&remote);
    println!(
        "connections: {}",
        serde_json::to_string(&connections_at_test)?
    );

    // Upload.
    let mut s = node.open_stream(remote, PROBE).await?;
    let stream_relayed = s.is_relayed();
    let mut hdr = vec![b'U'];
    hdr.extend_from_slice(&(test_secs as u32).to_be_bytes());
    s.write_all(&hdr).await?;
    let buf = vec![0x5Au8; 64 * 1024];
    let start = Instant::now();
    let end = start + Duration::from_secs(test_secs);
    while Instant::now() < end {
        s.write_all(&buf).await?;
    }
    s.close().await?;
    let mut total = [0u8; 8];
    s.read_exact(&mut total).await?;
    let up_bytes = u64::from_be_bytes(total);
    let up_secs = start.elapsed().as_secs_f64();

    // Download.
    let mut s = node.open_stream(remote, PROBE).await?;
    let mut hdr = vec![b'D'];
    hdr.extend_from_slice(&(test_secs as u32).to_be_bytes());
    s.write_all(&hdr).await?;
    let start = Instant::now();
    let mut down_bytes: u64 = 0;
    let mut rb = vec![0u8; 64 * 1024];
    loop {
        let n = s.read(&mut rb).await?;
        if n == 0 {
            break;
        }
        down_bytes += n as u64;
    }
    let down_secs = start.elapsed().as_secs_f64();

    let mbit = |b: u64, s: f64| {
        if s > 0.0 {
            (b as f64 * 8.0 / 1e6 / s * 100.0).round() / 100.0
        } else {
            0.0
        }
    };
    let result = Result_ {
        local_peer: node.peer_id().to_string(),
        remote_peer: remote.to_string(),
        connections_at_test,
        upload_mbit_s: mbit(up_bytes, up_secs),
        upload_bytes: up_bytes,
        download_mbit_s: mbit(down_bytes, down_secs),
        download_bytes: down_bytes,
        traffic: node.traffic(),
        stream_relayed,
        pair_secs,
        events: log.lock().map(|l| l.clone()).unwrap_or_default(),
    };
    println!(
        "\n=== NC-NETPROBE RESULT ===\n{}\n=== END ===",
        serde_json::to_string_pretty(&result)?
    );
    Ok(())
}

// ---- dev relay ----

#[derive(NetworkBehaviour)]
struct RelayBehaviour {
    relay: relay::Behaviour,
    ping: ping::Behaviour,
    identify: identify::Behaviour,
    autonat: autonat::Behaviour,
}

async fn run_relay(
    key: identity::Keypair,
    port: u16,
    allow: Vec<PeerId>,
    max_circuit_bytes: u64,
    max_circuit_secs: u64,
) -> Result<()> {
    let allow: Arc<HashSet<PeerId>> = Arc::new(allow.into_iter().collect());
    let a1 = allow.clone();
    let a2 = allow.clone();
    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(key)
        .with_tokio()
        .with_tcp(
            tcp::Config::default().nodelay(true),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_quic()
        .with_behaviour(move |k| {
            let pid = k.public().to_peer_id();
            let mut cfg = relay::Config {
                max_reservations: 16,
                max_reservations_per_peer: 2,
                max_circuits: 16,
                max_circuits_per_peer: 4,
                max_circuit_duration: Duration::from_secs(max_circuit_secs),
                max_circuit_bytes,
                ..Default::default()
            };
            // Only allow-listed peer keys may reserve or open circuits.
            cfg.reservation_rate_limiters
                .push(Box::new(move |p: PeerId, _: &Multiaddr, _| a1.contains(&p)));
            cfg.circuit_src_rate_limiters
                .push(Box::new(move |p: PeerId, _: &Multiaddr, _| a2.contains(&p)));
            let cfg = cfg
                .reservation_rate_per_peer(
                    NonZeroU32::new(30).unwrap_or(NonZeroU32::MIN),
                    Duration::from_secs(60),
                )
                .circuit_src_per_peer(
                    NonZeroU32::new(30).unwrap_or(NonZeroU32::MIN),
                    Duration::from_secs(60),
                );
            RelayBehaviour {
                relay: relay::Behaviour::new(pid, cfg),
                ping: ping::Behaviour::default(),
                identify: identify::Behaviour::new(identify::Config::new(
                    "/nc/id/1.0.0".into(),
                    k.public(),
                )),
                autonat: autonat::Behaviour::new(pid, autonat::Config::default()),
            }
        })?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(600)))
        .build();
    for a in listen_addrs(port) {
        swarm.listen_on(a)?;
    }
    println!("dev relay peer id: {}", swarm.local_peer_id());
    println!("allowed peers: {:?}", allow);
    loop {
        match swarm.select_next_some().await {
            SwarmEvent::NewListenAddr { address, .. } => {
                println!(
                    "[{}] relay listening on {address}/p2p/{}",
                    ts(),
                    swarm.local_peer_id()
                )
            }
            SwarmEvent::Behaviour(RelayBehaviourEvent::Relay(e)) => {
                println!("[{}] relay {e:?}", ts())
            }
            SwarmEvent::Behaviour(RelayBehaviourEvent::Identify(identify::Event::Received {
                peer_id,
                info,
                ..
            })) if allow.contains(&peer_id) => {
                // Learn our public address from what allowed peers observe, so circuit addresses are dialable.
                swarm.add_external_address(info.observed_addr);
            }
            SwarmEvent::ConnectionEstablished {
                peer_id, endpoint, ..
            } => {
                println!(
                    "[{}] conn {peer_id} {:?}",
                    ts(),
                    endpoint.get_remote_address()
                )
            }
            _ => {}
        }
    }
}
