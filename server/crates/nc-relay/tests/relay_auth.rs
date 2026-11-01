//! In-process relay + client over loopback: reservations need a valid token for the client's own key.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use futures::{AsyncReadExt, AsyncWriteExt, StreamExt};
use libp2p::multiaddr::Protocol;
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{identity, noise, relay, tcp, yamux, Multiaddr, PeerId, Swarm};
use nc_protocol::pb::v1::{relay_auth_frame::Payload, RelayAuthFrame};
use nc_protocol::wire::{decode_strict, relay_token_from_text};
use nc_relay::server::RELAY_AUTH_PROTOCOL;
use nc_relay::{unix_now, Limits, Quotas, RelayConfig, TokenKey};
use prost::Message;

#[derive(NetworkBehaviour)]
struct Client {
    relay: relay::client::Behaviour,
    stream: libp2p_stream::Behaviour,
}

fn client(key: identity::Keypair) -> Swarm<Client> {
    libp2p::SwarmBuilder::with_existing_identity(key)
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )
        .unwrap()
        .with_quic()
        .with_relay_client(noise::Config::new, yamux::Config::default)
        .unwrap()
        .with_behaviour(|_, relay| Client {
            relay,
            stream: libp2p_stream::Behaviour::new(),
        })
        .unwrap()
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(30)))
        .build()
}

async fn start_relay(key: TokenKey) -> Multiaddr {
    let rk = identity::Keypair::generate_ed25519();
    let rp = rk.public().to_peer_id();
    let cfg = RelayConfig {
        listen: vec!["/ip4/127.0.0.1/tcp/0".parse().unwrap()],
        external: vec![],
        token_key: key,
        max_reservations: 8,
        max_circuits: 8,
        max_circuits_per_peer: 2,
        max_circuit_duration: Duration::from_secs(60),
    };
    let quotas = Quotas::new(Limits::default());
    let mut swarm = nc_relay::build_swarm(rk, &cfg, &quotas).unwrap();
    // Bind here to learn the port, then hand the swarm to `run` with no extra listeners.
    swarm.listen_on(cfg.listen[0].clone()).unwrap();
    let addr = loop {
        if let SwarmEvent::NewListenAddr { address, .. } = swarm.select_next_some().await {
            break address;
        }
    };
    let run_cfg = RelayConfig {
        listen: vec![],
        external: vec![addr.clone()],
        ..cfg
    };
    tokio::spawn(nc_relay::run(swarm, run_cfg, quotas));
    addr.with(Protocol::P2p(rp))
}

/// Connects, optionally presents `token`, then asks for a reservation. Returns true if accepted.
async fn try_reserve(
    relay_addr: &Multiaddr,
    key: identity::Keypair,
    token: Option<String>,
) -> (bool, Option<bool>) {
    let relay_peer = match relay_addr.iter().last() {
        Some(Protocol::P2p(p)) => p,
        _ => panic!(),
    };
    let mut sw = client(key);
    sw.dial(relay_addr.clone()).unwrap();
    loop {
        if let SwarmEvent::ConnectionEstablished { peer_id, .. } = sw.select_next_some().await {
            if peer_id == relay_peer {
                break;
            }
        }
    }
    let mut auth_ok = None;
    if let Some(t) = token {
        let mut control = sw.behaviour().stream.new_control();
        let h = tokio::spawn(async move {
            let mut s = control
                .open_stream(relay_peer, RELAY_AUTH_PROTOCOL)
                .await
                .unwrap();
            let frame = RelayAuthFrame {
                payload: Some(Payload::Token(relay_token_from_text(&t).unwrap())),
            }
            .encode_to_vec();
            s.write_all(&(frame.len() as u32).to_be_bytes())
                .await
                .unwrap();
            s.write_all(&frame).await.unwrap();
            s.flush().await.unwrap();
            let mut len = [0u8; 4];
            s.read_exact(&mut len).await.unwrap();
            let mut r = vec![0u8; u32::from_be_bytes(len) as usize];
            s.read_exact(&mut r).await.unwrap();
            let reply: RelayAuthFrame = decode_strict(&r).unwrap();
            matches!(reply.payload, Some(Payload::Accepted(true)))
        });
        // Drive the swarm while the auth stream runs.
        let ok = loop {
            tokio::select! {
                _ = sw.select_next_some() => {}
                _ = async { while !h.is_finished() { tokio::time::sleep(Duration::from_millis(20)).await } } => {
                    break h.await.unwrap();
                }
            }
        };
        auth_ok = Some(ok);
    }
    sw.listen_on(relay_addr.clone().with(Protocol::P2pCircuit))
        .unwrap();
    let accepted = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match sw.select_next_some().await {
                SwarmEvent::Behaviour(ClientEvent::Relay(
                    relay::client::Event::ReservationReqAccepted { .. },
                )) => return true,
                SwarmEvent::ListenerClosed { .. } | SwarmEvent::ListenerError { .. } => {
                    return false
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap_or(false);
    (accepted, auth_ok)
}

#[tokio::test(flavor = "multi_thread")]
async fn reservation_requires_valid_token_for_own_key() {
    let tk = TokenKey::new([42; 32]);
    let relay = start_relay(tk.clone()).await;

    // 1. No token: denied.
    let (ok, _) = try_reserve(&relay, identity::Keypair::generate_ed25519(), None).await;
    assert!(!ok, "relay must not serve a peer without a token");

    // 2. Valid token for our own peer id: accepted.
    let k = identity::Keypair::generate_ed25519();
    let me = k.public().to_peer_id();
    let (t, _) = tk.issue(&me.to_string(), "acct-1", "dev-1", unix_now(), 300);
    let (ok, auth) = try_reserve(&relay, k, Some(t)).await;
    assert_eq!(auth, Some(true));
    assert!(ok, "token holder must get a reservation");

    // 3. Someone else's token: refused at auth and at reservation.
    let other = PeerId::random();
    let (t, _) = tk.issue(&other.to_string(), "acct-1", "dev-2", unix_now(), 300);
    let (ok, auth) = try_reserve(&relay, identity::Keypair::generate_ed25519(), Some(t)).await;
    assert_eq!(auth, Some(false));
    assert!(!ok);

    // 4. Token signed with the wrong key: refused.
    let k = identity::Keypair::generate_ed25519();
    let (t, _) = TokenKey::new([1; 32]).issue(
        &k.public().to_peer_id().to_string(),
        "a",
        "d",
        unix_now(),
        300,
    );
    let (ok, auth) = try_reserve(&relay, k, Some(t)).await;
    assert_eq!(auth, Some(false));
    assert!(!ok);
}
