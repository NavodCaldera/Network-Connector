//! Relay swarm: relay v2 behaviour gated by [`Quotas`], plus the token presentation protocol.

use std::time::{Duration, Instant};

use futures::{AsyncReadExt, AsyncWriteExt, StreamExt};
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{
    identify, identity, noise, ping, relay, tcp, yamux, Multiaddr, PeerId, StreamProtocol, Swarm,
};

use nc_protocol::pb::v1::{relay_auth_frame::Payload, RelayAuthFrame};
use nc_protocol::wire::decode_strict;
use prost::Message;

use crate::quota::Quotas;
use crate::token::{TokenKey, MAX_TOKEN_LEN};
use crate::unix_now;

/// Token presentation protocol (same id as `nc-net`'s client).
pub const RELAY_AUTH_PROTOCOL: StreamProtocol = StreamProtocol::new("/nc/relay-auth/1.0.0");

/// Relay settings.
#[derive(Clone, Debug)]
pub struct RelayConfig {
    /// Listen addresses. Default binds loopback only; set public addresses explicitly when deploying.
    pub listen: Vec<Multiaddr>,
    /// Public addresses clients dial (e.g. `/ip4/<public ip>/udp/4001/quic-v1`). libp2p's relay v2 serves
    /// reservations only once the server has an external address, so this must be set for a usable relay.
    pub external: Vec<Multiaddr>,
    /// Shared token key.
    pub token_key: TokenKey,
    /// Max simultaneous reservations / circuits overall.
    pub max_reservations: usize,
    /// Max simultaneous circuits overall.
    pub max_circuits: usize,
    /// Max circuits per peer at once.
    pub max_circuits_per_peer: usize,
    /// Max lifetime of one circuit.
    pub max_circuit_duration: Duration,
}

#[derive(NetworkBehaviour)]
pub struct Behaviour {
    relay: relay::Behaviour,
    ping: ping::Behaviour,
    identify: identify::Behaviour,
    stream: libp2p_stream::Behaviour,
}

/// Builds the relay swarm. `quotas.limits().circuit_bytes` becomes the per-circuit byte cap.
pub fn build_swarm(
    key: identity::Keypair,
    cfg: &RelayConfig,
    quotas: &Quotas,
) -> Result<Swarm<Behaviour>, Box<dyn std::error::Error + Send + Sync>> {
    let q1 = quotas.clone();
    let q2 = quotas.clone();
    let circuit_bytes = quotas.limits().circuit_bytes;
    let cfg = cfg.clone();
    let swarm = libp2p::SwarmBuilder::with_existing_identity(key)
        .with_tokio()
        .with_tcp(
            tcp::Config::default().nodelay(true),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_quic()
        .with_behaviour(move |k| {
            let pid = k.public().to_peer_id();
            let mut rc = relay::Config {
                max_reservations: cfg.max_reservations,
                max_reservations_per_peer: 1,
                reservation_duration: Duration::from_secs(30 * 60),
                max_circuits: cfg.max_circuits,
                max_circuits_per_peer: cfg.max_circuits_per_peer,
                max_circuit_duration: cfg.max_circuit_duration,
                max_circuit_bytes: circuit_bytes,
                reservation_rate_limiters: Vec::new(),
                circuit_src_rate_limiters: Vec::new(),
            };
            rc.reservation_rate_limiters
                .push(Box::new(move |p: PeerId, _: &Multiaddr, _| {
                    q1.try_reserve(p, Instant::now()).is_ok()
                }));
            rc.circuit_src_rate_limiters
                .push(Box::new(move |p: PeerId, _: &Multiaddr, _| {
                    q2.try_circuit(p, Instant::now()).is_ok()
                }));
            Behaviour {
                relay: relay::Behaviour::new(pid, rc),
                ping: ping::Behaviour::default(),
                identify: identify::Behaviour::new(identify::Config::new(
                    "/nc/id/1.0.0".into(),
                    k.public(),
                )),
                stream: libp2p_stream::Behaviour::new(),
            }
        })?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(120)))
        .build();
    // Listeners are started by `run`, so tests can build a swarm without side effects.
    Ok(swarm)
}

/// Handles capped, u32-BE-length-prefixed protobuf RelayAuthFrame messages.
async fn handle_auth(peer: PeerId, mut s: libp2p::Stream, key: TokenKey, quotas: Quotas) {
    let verdict = async {
        let mut len = [0u8; 4];
        s.read_exact(&mut len).await.ok()?;
        let len = u32::from_be_bytes(len) as usize;
        if len > MAX_TOKEN_LEN + 8 {
            return None;
        }
        let mut buf = vec![0u8; len];
        s.read_exact(&mut buf).await.ok()?;
        let frame: RelayAuthFrame = decode_strict(&buf).ok()?;
        let Some(Payload::Token(token)) = frame.payload else {
            return None;
        };
        let now = unix_now();
        let claims = key.verify_binary(&token, now).ok()?;
        // The token must be for the key that is actually on this connection.
        if claims.peer.as_deref() != Some(peer.to_string().as_str()) {
            return None;
        }
        let ttl = Duration::from_secs(claims.exp?.saturating_sub(now));
        quotas.grant(peer, claims.account?, ttl, Instant::now());
        Some(())
    }
    .await;
    let reply = RelayAuthFrame {
        payload: Some(Payload::Accepted(verdict.is_some())),
    }
    .encode_to_vec();
    if verdict.is_some() {
        tracing::info!(%peer, "relay token accepted");
    } else {
        tracing::warn!(%peer, "relay token refused");
    }
    let _ = s.write_all(&(reply.len() as u32).to_be_bytes()).await;
    let _ = s.write_all(&reply).await;
    let _ = s.flush().await;
    let _ = s.close().await;
}

/// Runs the relay until the task is dropped.
pub async fn run(
    mut swarm: Swarm<Behaviour>,
    cfg: RelayConfig,
    quotas: Quotas,
) -> Result<(), String> {
    for a in &cfg.listen {
        swarm.listen_on(a.clone()).map_err(|e| e.to_string())?;
    }
    if cfg.external.is_empty() {
        tracing::warn!("no external address configured: relay v2 will refuse reservations");
    }
    for a in &cfg.external {
        swarm.add_external_address(a.clone());
    }
    let mut control = swarm.behaviour().stream.new_control();
    let mut incoming = control
        .accept(RELAY_AUTH_PROTOCOL)
        .map_err(|e| e.to_string())?;
    let key = cfg.token_key.clone();
    let q = quotas.clone();
    tokio::spawn(async move {
        while let Some((peer, s)) = incoming.next().await {
            tokio::spawn(handle_auth(peer, s, key.clone(), q.clone()));
        }
    });
    loop {
        match swarm.select_next_some().await {
            SwarmEvent::NewListenAddr { address, .. } => {
                tracing::info!("relay listening on {address}/p2p/{}", swarm.local_peer_id());
            }
            SwarmEvent::Behaviour(BehaviourEvent::Relay(e)) => {
                tracing::info!(event = ?e, "relay");
            }
            _ => {}
        }
    }
}
