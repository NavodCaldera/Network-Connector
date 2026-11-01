//! Connection ladder bookkeeping: which rung each connection uses, and direct vs relayed bytes.

use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use libp2p::multiaddr::Protocol;
use libp2p::swarm::ConnectionId;
use libp2p::{Multiaddr, PeerId};
use serde::Serialize;

/// Rung of the connection ladder (PLAN.md §2, net-agent rules).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Rung {
    /// Direct connection to a private/loopback/link-local address (same LAN).
    LanDirect,
    /// Direct connection to a public address.
    InternetDirect,
    /// Direct connection created by DCUtR hole punching.
    HolePunched,
    /// Connection carried through a relay circuit (still end-to-end encrypted).
    Relayed,
}

impl Rung {
    /// True for every rung that does not go through a relay.
    pub fn is_direct(self) -> bool {
        !matches!(self, Rung::Relayed)
    }
}

/// Classifies a remote address. DCUtR upgrades are marked separately by the node.
pub fn classify_addr(addr: &Multiaddr) -> Rung {
    let mut rung = Rung::InternetDirect;
    for p in addr.iter() {
        match p {
            Protocol::P2pCircuit => return Rung::Relayed,
            Protocol::Ip4(ip) if is_local_v4(ip) => rung = Rung::LanDirect,
            Protocol::Ip6(ip) if is_local_v6(ip) => rung = Rung::LanDirect,
            _ => {}
        }
    }
    rung
}

fn is_local_v4(ip: Ipv4Addr) -> bool {
    ip.is_private() || ip.is_loopback() || ip.is_link_local()
}

fn is_local_v6(ip: Ipv6Addr) -> bool {
    let seg0 = ip.segments()[0];
    ip.is_loopback() || (seg0 & 0xfe00) == 0xfc00 || (seg0 & 0xffc0) == 0xfe80
}

/// One live connection, as reported to callers.
#[derive(Clone, Debug, Serialize)]
pub struct ConnectionInfo {
    /// Ladder rung.
    pub rung: Rung,
    /// Remote address (may contain `/p2p-circuit`).
    pub remote_addr: String,
    /// Seconds since the connection was established.
    pub age_secs: u64,
}

/// Bytes moved over [`crate::PeerStream`]s, split by direct vs relayed path.
///
/// Counted at the stream layer (payload bytes, excluding transport framing). A stream is attributed to the
/// path in use when it was opened. `libp2p-stream` picks a connection at random when several exist, so:
/// the node closes a trusted peer's relayed connections 3 s after a direct one exists (never during
/// pairing), and `open_stream` waits up to 5 s for that before opening. Limits: an inbound stream accepted
/// in that window may be attributed to the wrong path, and a stream still running on a circuit when it is
/// closed is cut (Core must retry; transfers are resumable per PLAN.md §6).
#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
pub struct TrafficStats {
    /// Bytes over direct connections.
    pub direct_bytes: u64,
    /// Bytes over relay circuits.
    pub relayed_bytes: u64,
}

#[derive(Debug)]
pub(crate) struct ConnEntry {
    pub peer: PeerId,
    pub rung: Rung,
    pub remote_addr: Multiaddr,
    pub since: Instant,
}

/// Shared between the swarm task and stream handles.
#[derive(Debug, Default)]
pub(crate) struct Ledger {
    pub conns: Mutex<HashMap<ConnectionId, ConnEntry>>,
    pub direct_bytes: Arc<AtomicU64>,
    pub relayed_bytes: Arc<AtomicU64>,
}

impl Ledger {
    pub fn insert(&self, id: ConnectionId, entry: ConnEntry) {
        if let Ok(mut c) = self.conns.lock() {
            c.insert(id, entry);
        }
    }

    pub fn remove(&self, id: &ConnectionId) {
        if let Ok(mut c) = self.conns.lock() {
            c.remove(id);
        }
    }

    pub fn mark_hole_punched(&self, id: &ConnectionId) {
        if let Ok(mut c) = self.conns.lock() {
            if let Some(e) = c.get_mut(id) {
                e.rung = Rung::HolePunched;
            }
        }
    }

    pub fn peer_has_direct(&self, peer: &PeerId) -> bool {
        self.conns
            .lock()
            .map(|c| c.values().any(|e| e.peer == *peer && e.rung.is_direct()))
            .unwrap_or(false)
    }

    /// True if the peer has both a direct and a relayed connection (stream path would be ambiguous).
    pub fn peer_is_mixed(&self, peer: &PeerId) -> bool {
        self.conns
            .lock()
            .map(|c| {
                let mine = c.values().filter(|e| e.peer == *peer);
                let (mut d, mut r) = (false, false);
                for e in mine {
                    if e.rung.is_direct() {
                        d = true;
                    } else {
                        r = true;
                    }
                }
                d && r
            })
            .unwrap_or(false)
    }

    pub fn connections(&self, peer: &PeerId) -> Vec<ConnectionInfo> {
        self.conns
            .lock()
            .map(|c| {
                c.values()
                    .filter(|e| e.peer == *peer)
                    .map(|e| ConnectionInfo {
                        rung: e.rung,
                        remote_addr: e.remote_addr.to_string(),
                        age_secs: e.since.elapsed().as_secs(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn counter_for(&self, peer: &PeerId) -> Arc<AtomicU64> {
        if self.peer_has_direct(peer) {
            self.direct_bytes.clone()
        } else {
            self.relayed_bytes.clone()
        }
    }

    pub fn traffic(&self) -> TrafficStats {
        TrafficStats {
            direct_bytes: self.direct_bytes.load(Ordering::Relaxed),
            relayed_bytes: self.relayed_bytes.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn rung(s: &str) -> Rung {
        classify_addr(&s.parse().unwrap())
    }

    #[test]
    fn classifies_addresses() {
        assert_eq!(rung("/ip4/192.168.1.4/udp/4001/quic-v1"), Rung::LanDirect);
        assert_eq!(rung("/ip4/127.0.0.1/tcp/4001"), Rung::LanDirect);
        assert_eq!(
            rung("/ip4/203.0.113.9/udp/4001/quic-v1"),
            Rung::InternetDirect
        );
        assert_eq!(rung("/ip6/fe80::1/tcp/1"), Rung::LanDirect);
        assert_eq!(rung("/ip6/2001:db8::1/tcp/1"), Rung::InternetDirect);
        let relayed = format!(
            "/ip4/203.0.113.9/udp/4001/quic-v1/p2p/{}/p2p-circuit",
            PeerId::random()
        );
        assert_eq!(rung(&relayed), Rung::Relayed);
        assert!(!Rung::Relayed.is_direct());
        assert!(Rung::HolePunched.is_direct());
    }
}
