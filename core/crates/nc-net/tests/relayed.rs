//! Pairing and streaming through a relay circuit on loopback (in-process relay v2 server).
//! Checks that the connection ladder records the relayed rung and that bytes land in the right bucket.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use futures::{AsyncReadExt, AsyncWriteExt, StreamExt};
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{identify, identity, noise, ping, relay, tcp, yamux};
use nc_net::pairing::{with_addrs_for_test, DEFAULT_OFFER_TTL};
use nc_net::{
    Keypair, Multiaddr, Node, NodeConfig, NodeEvent, PeerId, Protocol, Rung, StreamProtocol,
};

const APP: StreamProtocol = StreamProtocol::new("/nc/test-app/1.0.0");

#[derive(NetworkBehaviour)]
struct RelayB {
    relay: relay::Behaviour,
    ping: ping::Behaviour,
    identify: identify::Behaviour,
}

async fn start_relay() -> Multiaddr {
    let key = identity::Keypair::generate_ed25519();
    let mut sw = libp2p::SwarmBuilder::with_existing_identity(key)
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )
        .unwrap()
        .with_behaviour(|k| RelayB {
            relay: relay::Behaviour::new(k.public().to_peer_id(), relay::Config::default()),
            ping: ping::Behaviour::default(),
            identify: identify::Behaviour::new(identify::Config::new(
                "/nc/id/1.0.0".into(),
                k.public(),
            )),
        })
        .unwrap()
        .build();
    sw.listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .unwrap();
    let addr = loop {
        if let SwarmEvent::NewListenAddr { address, .. } = sw.select_next_some().await {
            break address;
        }
    };
    sw.add_external_address(addr.clone());
    let id = *sw.local_peer_id();
    tokio::spawn(async move {
        loop {
            sw.select_next_some().await;
        }
    });
    addr.with(Protocol::P2p(id))
}

async fn node(relay: PeerId) -> Node {
    let mut cfg = NodeConfig::new(Keypair::generate_ed25519());
    cfg.listen = vec!["/ip4/127.0.0.1/tcp/0".parse().unwrap()];
    cfg.infra_peers = vec![relay];
    Node::start(cfg).await.unwrap()
}

async fn wait_connected(n: &Node, p: PeerId) {
    for _ in 0..200 {
        if n.is_connected(p).await.unwrap() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("not connected");
}

#[tokio::test(flavor = "multi_thread")]
async fn pair_and_stream_over_relay_circuit() {
    let relay_addr = start_relay().await;
    let relay_id = match relay_addr.iter().last() {
        Some(Protocol::P2p(p)) => p,
        _ => unreachable!(),
    };
    let a = node(relay_id).await;
    let b = node(relay_id).await;

    // A reserves a slot on the relay.
    let mut ev = a.events();
    a.dial(relay_addr.clone()).await.unwrap();
    wait_connected(&a, relay_id).await;
    a.listen_on(relay_addr.clone().with(Protocol::P2pCircuit))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Ok(e) = ev.recv().await {
            if matches!(e, NodeEvent::ReservationAccepted { .. }) {
                break;
            }
        }
    })
    .await
    .expect("reservation");

    // Offer that only contains the circuit address, so B must come through the relay.
    let offer = a.create_pairing_offer(DEFAULT_OFFER_TTL).await.unwrap();
    let circuit = relay_addr
        .clone()
        .with(Protocol::P2pCircuit)
        .with(Protocol::P2p(a.peer_id()));
    let offer = with_addrs_for_test(&offer, vec![circuit]);

    let mut prompts = a.take_pairing_prompts().unwrap();
    tokio::spawn(async move {
        if let Some(p) = prompts.recv().await {
            let _ = p.reply.send(true);
        }
    });
    b.dial(relay_addr.clone()).await.unwrap();
    wait_connected(&b, relay_id).await;
    let mut b_ev = b.events();
    b.pair(&offer, |_| async { true }).await.unwrap();
    println!(
        "B->A connections after pairing: {:?}",
        b.connections(&a.peer_id())
    );

    // The first connection B made to A went through the circuit.
    let mut rungs = Vec::new();
    while let Ok(Ok(e)) = tokio::time::timeout(Duration::from_millis(1500), b_ev.recv()).await {
        match e {
            NodeEvent::Connected { peer, rung, .. } if peer == a.peer_id() => rungs.push(rung),
            NodeEvent::HolePunch { peer, ok, detail } if peer == a.peer_id() => {
                println!("dcutr ok={ok} {detail}")
            }
            _ => {}
        }
    }
    println!("B->A rungs seen: {rungs:?}");
    assert_eq!(rungs.first(), Some(&Rung::Relayed), "{rungs:?}");

    // Echo server on A; stream from B.
    for _ in 0..50 {
        if a.trust().contains(&b.peer_id()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let mut inc = a.accept(APP).unwrap();
    tokio::spawn(async move {
        while let Some(mut s) = inc.next().await {
            let mut buf = vec![0u8; 8192];
            while let Ok(k) = s.read(&mut buf).await {
                if k == 0 || s.write_all(&buf[..k]).await.is_err() {
                    break;
                }
            }
        }
    });
    let data = vec![7u8; 50_000];
    let mut s = b.open_stream(a.peer_id(), APP).await.unwrap();
    let relayed = s.is_relayed();
    s.write_all(&data).await.unwrap();
    let mut got = vec![0u8; data.len()];
    s.read_exact(&mut got).await.unwrap();
    assert_eq!(got, data);
    let t = b.traffic();
    println!(
        "stream relayed={relayed} traffic={t:?} connections={:?}",
        b.connections(&a.peer_id())
    );
    if relayed {
        assert!(
            t.relayed_bytes >= 2 * data.len() as u64 && t.direct_bytes == 0,
            "{t:?}"
        );
    } else {
        assert!(
            t.direct_bytes >= 2 * data.len() as u64 && t.relayed_bytes == 0,
            "{t:?}"
        );
    }
}
