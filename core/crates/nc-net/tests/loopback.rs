//! Two (or three) in-process peers over loopback: pair, stream data, reconnect, impostors rejected.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use futures::{AsyncReadExt, AsyncWriteExt};
use nc_net::pairing::DEFAULT_OFFER_TTL;
use nc_net::{
    Keypair, Multiaddr, NetError, Node, NodeConfig, PairingOffer, Rung, StreamProtocol, TrustStore,
};

const APP: StreamProtocol = StreamProtocol::new("/nc/test-app/1.0.0");

async fn node_with(key: Keypair, trust: TrustStore, quic: bool) -> Node {
    let mut cfg = NodeConfig::new(key);
    cfg.listen = vec![if quic {
        "/ip4/127.0.0.1/udp/0/quic-v1".parse().unwrap()
    } else {
        "/ip4/127.0.0.1/tcp/0".parse().unwrap()
    }];
    cfg.trust = trust;
    let n = Node::start(cfg).await.unwrap();
    // Wait for the listener.
    for _ in 0..50 {
        if !n.listen_addrs().await.unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    n
}

async fn node(quic: bool) -> Node {
    node_with(Keypair::generate_ed25519(), TrustStore::in_memory(), quic).await
}

/// Responder auto-confirms the SAS it is shown, and returns it.
fn auto_confirm(n: &Node) -> tokio::task::JoinHandle<Option<String>> {
    let mut rx = n.take_pairing_prompts().unwrap();
    tokio::spawn(async move {
        let p = rx.recv().await?;
        let sas = p.sas.clone();
        let _ = p.reply.send(true);
        Some(sas)
    })
}

async fn pair(a: &Node, b: &Node) -> PairingOffer {
    let offer = a.create_pairing_offer(DEFAULT_OFFER_TTL).await.unwrap();
    let qr = offer.to_qr_string();
    let responder = auto_confirm(a);
    let parsed = PairingOffer::parse(&qr).unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let peer = b
        .pair(&parsed, |sas| async move {
            let _ = tx.send(sas);
            true
        })
        .await
        .unwrap();
    assert_eq!(peer, a.peer_id());
    let sas_a = tokio::time::timeout(Duration::from_secs(10), responder)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let sas_b = rx.await.unwrap();
    assert_eq!(sas_a, sas_b, "both screens must show the same SAS");
    assert_eq!(sas_a.len(), 6);
    // Responder adds trust right after sending its confirmation.
    for _ in 0..50 {
        if a.trust().contains(&b.peer_id()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    offer
}

async fn echo_server(n: &Node) {
    let mut inc = n.accept(APP).unwrap();
    tokio::spawn(async move {
        while let Some(mut s) = inc.next().await {
            tokio::spawn(async move {
                let mut buf = vec![0u8; 64 * 1024];
                loop {
                    let k = match s.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(k) => k,
                    };
                    if s.write_all(&buf[..k]).await.is_err() {
                        break;
                    }
                }
                let _ = s.close().await;
            });
        }
    });
}

async fn roundtrip(from: &Node, to: &Node, data: &[u8]) -> Result<Vec<u8>, NetError> {
    let mut s = from.open_stream(to.peer_id(), APP).await?;
    assert_eq!(s.peer(), to.peer_id());
    s.write_all(data).await?;
    let mut got = vec![0u8; data.len()];
    s.read_exact(&mut got).await?;
    Ok(got)
}

#[tokio::test(flavor = "multi_thread")]
async fn pair_and_stream_quic() {
    let a = node(true).await;
    let b = node(true).await;
    pair(&a, &b).await;
    assert!(a.trust().contains(&b.peer_id()));
    assert!(b.trust().contains(&a.peer_id()));

    echo_server(&a).await;
    let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let got = roundtrip(&b, &a, &data).await.unwrap();
    assert_eq!(got, data);

    let conns = b.connections(&a.peer_id());
    assert!(!conns.is_empty());
    assert!(conns.iter().all(|c| c.rung == Rung::LanDirect), "{conns:?}");
    let t = b.traffic();
    assert_eq!(t.relayed_bytes, 0);
    assert!(t.direct_bytes >= 2 * data.len() as u64, "{t:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn pair_and_stream_tcp_fallback() {
    let a = node(false).await;
    let b = node(false).await;
    pair(&a, &b).await;
    echo_server(&a).await;
    assert_eq!(
        roundtrip(&b, &a, b"over tcp/noise/yamux").await.unwrap(),
        b"over tcp/noise/yamux"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn offer_is_single_use() {
    let a = node(true).await;
    let b = node(true).await;
    let c = node(true).await;
    let offer = pair(&a, &b).await;
    // Same QR code again from another device: refused.
    let err = c.pair(&offer, |_| async { true }).await;
    assert!(err.is_err(), "reused offer must fail");
    assert!(!a.trust().contains(&c.peer_id()));
}

#[tokio::test(flavor = "multi_thread")]
async fn unpaired_peer_is_rejected_even_if_it_knows_the_address() {
    let a = node(true).await;
    let b = node(true).await;
    pair(&a, &b).await;
    echo_server(&a).await;

    // Impostor C knows A's address and peer id (as mDNS would tell it) and even marks A as trusted locally.
    let c = node(true).await;
    c.trust().add(a.peer_id()).unwrap();
    let mut a_events = a.events();
    let addr: Multiaddr = a.listen_addrs().await.unwrap()[0].clone();
    c.dial(addr.with(nc_net::Protocol::P2p(a.peer_id())))
        .await
        .unwrap();

    let res = roundtrip(&c, &a, b"let me in").await;
    assert!(res.is_err(), "A must not serve an unpaired peer: {res:?}");

    // A reported the rejection.
    let mut rejected = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), a_events.recv()).await {
            Ok(Ok(nc_net::NodeEvent::Rejected { peer })) if peer == c.peer_id() => {
                rejected = true;
                break;
            }
            _ => {}
        }
    }
    assert!(rejected);

    // A also refuses to open streams to C.
    assert!(matches!(
        a.open_stream(c.peer_id(), APP).await,
        Err(NetError::Untrusted(_))
    ));
    // Paired B still works.
    assert_eq!(roundtrip(&b, &a, b"ok").await.unwrap(), b"ok");
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_secret_fails_and_locks_after_five() {
    let a = node(true).await;
    let mut prompts = a.take_pairing_prompts().unwrap();
    let real = a.create_pairing_offer(DEFAULT_OFFER_TTL).await.unwrap();

    // Forge an offer: correct peer id and addresses, wrong secret.
    let forged = nc_net::pairing::forge_for_test(&real);
    for _ in 0..5 {
        let c = node(true).await;
        assert!(c.pair(&forged, |_| async { true }).await.is_err());
        assert!(!a.trust().contains(&c.peer_id()));
    }
    // Locked: even the real secret no longer works.
    let d = node(true).await;
    assert!(d.pair(&real, |_| async { true }).await.is_err());
    assert!(!a.trust().contains(&d.peer_id()));
    assert!(
        prompts.try_recv().is_err(),
        "no SAS prompt may be shown for a bad proof"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn impostor_at_the_offered_address_is_detected() {
    let a = node(true).await;
    let offer = a.create_pairing_offer(DEFAULT_OFFER_TTL).await.unwrap();
    // C answers at the address in the QR code, but it does not hold A's key.
    let c = node(true).await;
    let c_addrs = c.listen_addrs().await.unwrap();
    let swapped = nc_net::pairing::with_addrs_for_test(&offer, c_addrs);
    let b = node(true).await;
    let r = b.pair(&swapped, |_| async { true }).await;
    assert!(
        r.is_err(),
        "dial by peer id must fail when the key does not match"
    );
    assert!(!b.trust().contains(&c.peer_id()));
    assert!(!b.trust().contains(&a.peer_id()));
}

#[tokio::test(flavor = "multi_thread")]
async fn sas_mismatch_means_no_trust() {
    let a = node(true).await;
    let b = node(true).await;
    let offer = a.create_pairing_offer(DEFAULT_OFFER_TTL).await.unwrap();
    let responder = auto_confirm(&a);
    // User on B says the digits differ.
    assert!(b.pair(&offer, |_| async { false }).await.is_err());
    let _ = tokio::time::timeout(Duration::from_secs(10), responder).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!a.trust().contains(&b.peer_id()));
    assert!(!b.trust().contains(&a.peer_id()));
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnect_after_restart_with_persisted_trust() {
    let dir = std::env::temp_dir().join(format!("nc-net-reconnect-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (fa, fb) = (dir.join("a.json"), dir.join("b.json"));
    let _ = std::fs::remove_file(&fa);
    let _ = std::fs::remove_file(&fb);
    let (ka, kb) = (Keypair::generate_ed25519(), Keypair::generate_ed25519());

    let a = node_with(ka.clone(), TrustStore::open(&fa).unwrap(), true).await;
    {
        let b = node_with(kb.clone(), TrustStore::open(&fb).unwrap(), true).await;
        pair(&a, &b).await;
    }
    echo_server(&a).await;
    // B restarts: same key, trust loaded from disk, no new pairing.
    let b2 = node_with(kb, TrustStore::open(&fb).unwrap(), true).await;
    assert!(b2.trust().contains(&a.peer_id()));
    let addr = a.listen_addrs().await.unwrap()[0].clone();
    b2.dial(addr.with(nc_net::Protocol::P2p(a.peer_id())))
        .await
        .unwrap();
    assert_eq!(roundtrip(&b2, &a, b"again").await.unwrap(), b"again");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn reserved_protocols_are_not_available_to_core() {
    let a = node(true).await;
    assert!(matches!(
        a.accept(StreamProtocol::new("/nc/pair/1.0.0")),
        Err(NetError::ReservedProtocol)
    ));
}
