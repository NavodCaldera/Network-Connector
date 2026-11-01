//! QR pairing with short-authentication-string (SAS) confirmation.
//!
//! Flow (device A shows the QR code, device B scans it):
//! 1. A creates a [`PairingOffer`] = (A's peer id, A's addresses, 32-byte one-time secret, expiry) and shows it
//!    as a QR code / text. The secret lives only in memory on A and is never logged.
//! 2. B dials A **by peer id** (`/p2p/<A>`), so the Noise/TLS handshake proves B reached A's key, not just an
//!    address. A sees B's key the same way.
//! 3. On `/nc/pair/1.0.0` they swap random nonces and HMAC proofs over a transcript that binds both peer keys
//!    and both nonces: `proof = HMAC-SHA256(secret, role || "nc-pair-v1" || A || B || nB || nA)`. The secret
//!    itself never crosses the network. A checks B's proof first (constant time), then proves itself.
//! 4. Both derive a 6-digit SAS from the same transcript and show it; each user confirms the digits match.
//! 5. Only after both confirmations each side adds the other's peer key to its [`crate::TrustStore`].
//!
//! The offer is single-use, expires (default 10 min), and is locked after 5 failed proofs, matching the
//! legacy 0.x pairing rules (`legacy/python-v0.3:docs/PAIRING.md`).
//!
//! TODO(proto): the offer encoding and the pairing frames are wire formats and belong in `proto/` (owned by
//! Core). They are local here for Phase 0; see the net-agent hand-back "Requests".

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use futures::{AsyncRead, AsyncWrite};
use hmac::{Hmac, Mac};
use libp2p::{Multiaddr, PeerId};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use tokio::sync::oneshot;

use crate::frame::{read_frame, write_frame};
use crate::NetError;

/// Prefix of the text form of an offer (what the QR code contains).
pub const OFFER_PREFIX: &str = "nc-pair:v1:";
/// Default offer lifetime.
pub const DEFAULT_OFFER_TTL: Duration = Duration::from_secs(600);
/// Failed proofs before an offer is locked.
pub const MAX_FAILED_ATTEMPTS: u32 = 5;
const MAX_FRAME: usize = 1024;
const MAX_ADDRS: usize = 16;

type HmacSha256 = Hmac<Sha256>;

/// What the QR code carries. `Debug` redacts the secret.
#[derive(Clone)]
pub struct PairingOffer {
    /// Peer key of the device showing the code.
    pub peer_id: PeerId,
    /// Addresses to dial (LAN, public or relay circuit).
    pub addrs: Vec<Multiaddr>,
    secret: [u8; 32],
    /// Expiry, seconds since the Unix epoch.
    pub expires_unix: u64,
}

impl std::fmt::Debug for PairingOffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingOffer")
            .field("peer_id", &self.peer_id)
            .field("addrs", &self.addrs)
            .field("secret", &"<redacted>")
            .field("expires_unix", &self.expires_unix)
            .finish()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OfferWire {
    p: String,
    a: Vec<String>,
    s: String,
    e: u64,
}

impl PairingOffer {
    pub(crate) fn new(peer_id: PeerId, addrs: Vec<Multiaddr>, ttl: Duration) -> Self {
        let mut secret = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut secret);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            peer_id,
            addrs,
            secret,
            expires_unix: now + ttl.as_secs(),
        }
    }

    /// Text for the QR code. Contains the secret: show it, never log or store it.
    pub fn to_qr_string(&self) -> String {
        let wire = OfferWire {
            p: self.peer_id.to_string(),
            a: self.addrs.iter().map(|a| a.to_string()).collect(),
            s: URL_SAFE_NO_PAD.encode(self.secret),
            e: self.expires_unix,
        };
        // Serialising plain strings and integers cannot fail.
        let json = serde_json::to_vec(&wire).unwrap_or_default();
        format!("{OFFER_PREFIX}{}", URL_SAFE_NO_PAD.encode(json))
    }

    /// Parses the QR text. Rejects malformed, oversized or expired offers.
    pub fn parse(text: &str) -> Result<Self, NetError> {
        let body = text
            .trim()
            .strip_prefix(OFFER_PREFIX)
            .ok_or(NetError::InvalidOffer)?;
        if body.len() > 4096 {
            return Err(NetError::InvalidOffer);
        }
        let json = URL_SAFE_NO_PAD
            .decode(body)
            .map_err(|_| NetError::InvalidOffer)?;
        let w: OfferWire = serde_json::from_slice(&json).map_err(|_| NetError::InvalidOffer)?;
        if w.a.len() > MAX_ADDRS {
            return Err(NetError::InvalidOffer);
        }
        let peer_id: PeerId = w.p.parse().map_err(|_| NetError::InvalidOffer)?;
        let addrs =
            w.a.iter()
                .map(|a| a.parse::<Multiaddr>())
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| NetError::InvalidOffer)?;
        let secret: [u8; 32] = URL_SAFE_NO_PAD
            .decode(&w.s)
            .map_err(|_| NetError::InvalidOffer)?
            .try_into()
            .map_err(|_| NetError::InvalidOffer)?;
        let offer = Self {
            peer_id,
            addrs,
            secret,
            expires_unix: w.e,
        };
        if offer.is_expired() {
            return Err(NetError::Pairing("offer expired".into()));
        }
        Ok(offer)
    }

    fn is_expired(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(u64::MAX);
        now >= self.expires_unix
    }
}

/// Asks the local user to compare the SAS on both screens (responder side).
/// Answer with `true` only if the digits match.
#[derive(Debug)]
pub struct PairingPrompt {
    /// Authenticated key of the device asking to pair.
    pub peer: PeerId,
    /// 6-digit short authentication string to show.
    pub sas: String,
    /// Send the user's answer here.
    pub reply: oneshot::Sender<bool>,
}

/// Responder-side state for the single active offer.
pub(crate) struct HostOffer {
    pub offer: PairingOffer,
    pub failed: u32,
    pub used: bool,
    pub busy: bool,
}

impl HostOffer {
    pub fn usable(&self) -> bool {
        !self.used && !self.busy && self.failed < MAX_FAILED_ATTEMPTS && !self.offer.is_expired()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum Msg {
    Nonce(String),
    Proof(String),
    Confirm(bool),
    Reject,
}

fn transcript(responder: &PeerId, initiator: &PeerId, n_i: &[u8], n_r: &[u8]) -> Vec<u8> {
    let mut t = b"nc-pair-v1".to_vec();
    for part in [
        responder.to_bytes(),
        initiator.to_bytes(),
        n_i.to_vec(),
        n_r.to_vec(),
    ] {
        t.extend_from_slice(&(part.len() as u32).to_be_bytes());
        t.extend_from_slice(&part);
    }
    t
}

fn mac(secret: &[u8; 32], label: &[u8], t: &[u8]) -> [u8; 32] {
    // HMAC accepts keys of any length, so this cannot fail.
    let Ok(mut m) = HmacSha256::new_from_slice(secret) else {
        return [0u8; 32];
    };
    m.update(label);
    m.update(t);
    m.finalize().into_bytes().into()
}

/// 6-digit SAS from the transcript.
pub(crate) fn sas(secret: &[u8; 32], t: &[u8]) -> String {
    let h = mac(secret, b"sas", t);
    let n = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) % 1_000_000;
    format!("{n:06}")
}

async fn send<S: AsyncWrite + Unpin>(s: &mut S, m: &Msg) -> Result<(), NetError> {
    let b = serde_json::to_vec(m).map_err(|e| NetError::Protocol(e.to_string()))?;
    write_frame(s, &b).await
}

async fn recv<S: AsyncRead + Unpin>(s: &mut S) -> Result<Msg, NetError> {
    let b = read_frame(s, MAX_FRAME).await?;
    serde_json::from_slice(&b).map_err(|_| NetError::Protocol("bad pairing frame".into()))
}

fn nonce() -> [u8; 32] {
    let mut n = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut n);
    n
}

fn decode32(s: &str) -> Result<Vec<u8>, NetError> {
    let v = URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|_| NetError::Protocol("bad base64".into()))?;
    if v.len() != 32 {
        return Err(NetError::Protocol("bad length".into()));
    }
    Ok(v)
}

/// Initiator (scanner) side. `local` and `remote` are the authenticated peer keys of the connection.
/// Returns once both users confirmed; the caller then trusts `remote`.
pub(crate) async fn run_initiator<S, F, Fut>(
    s: &mut S,
    offer: &PairingOffer,
    local: PeerId,
    remote: PeerId,
    confirm: F,
) -> Result<(), NetError>
where
    S: AsyncRead + AsyncWrite + Unpin,
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    if remote != offer.peer_id {
        return Err(NetError::Pairing(
            "connected peer is not the peer in the offer".into(),
        ));
    }
    let n_i = nonce();
    send(s, &Msg::Nonce(URL_SAFE_NO_PAD.encode(n_i))).await?;
    let Msg::Nonce(n_r) = recv(s).await? else {
        return Err(NetError::Pairing(
            "offer rejected by the other device".into(),
        ));
    };
    let n_r = decode32(&n_r)?;
    let t = transcript(&remote, &local, &n_i, &n_r);
    send(
        s,
        &Msg::Proof(URL_SAFE_NO_PAD.encode(mac(&offer.secret, b"init", &t))),
    )
    .await?;
    let Msg::Proof(p_r) = recv(s).await? else {
        return Err(NetError::Pairing(
            "pairing code incorrect or offer no longer valid".into(),
        ));
    };
    let expected = mac(&offer.secret, b"resp", &t);
    if !bool::from(decode32(&p_r)?.ct_eq(&expected)) {
        return Err(NetError::Pairing(
            "other device failed to prove it knows the pairing secret".into(),
        ));
    }
    let ok = confirm(sas(&offer.secret, &t)).await;
    send(s, &Msg::Confirm(ok)).await?;
    if !ok {
        return Err(NetError::Pairing("SAS not confirmed locally".into()));
    }
    match recv(s).await? {
        Msg::Confirm(true) => Ok(()),
        _ => Err(NetError::Pairing(
            "SAS not confirmed on the other device".into(),
        )),
    }
}

/// Outcome of the responder's proof check, so the node can count failed attempts.
pub(crate) enum ResponderStage {
    /// Proof verified; SAS to show.
    Verified { sas: String },
    /// Wrong proof (counts towards the lock-out).
    BadProof,
}

/// Responder step 1: nonce exchange and proof check.
pub(crate) async fn responder_verify<S>(
    s: &mut S,
    secret: &[u8; 32],
    local: PeerId,
    remote: PeerId,
) -> Result<ResponderStage, NetError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let Msg::Nonce(n_i) = recv(s).await? else {
        return Err(NetError::Protocol("expected nonce".into()));
    };
    let n_i = decode32(&n_i)?;
    let n_r = nonce();
    send(s, &Msg::Nonce(URL_SAFE_NO_PAD.encode(n_r))).await?;
    let t = transcript(&local, &remote, &n_i, &n_r);
    let Msg::Proof(p_i) = recv(s).await? else {
        return Err(NetError::Protocol("expected proof".into()));
    };
    let expected = mac(secret, b"init", &t);
    if !bool::from(decode32(&p_i)?.ct_eq(&expected)) {
        let _ = send(s, &Msg::Reject).await;
        return Ok(ResponderStage::BadProof);
    }
    send(
        s,
        &Msg::Proof(URL_SAFE_NO_PAD.encode(mac(secret, b"resp", &t))),
    )
    .await?;
    Ok(ResponderStage::Verified {
        sas: sas(secret, &t),
    })
}

/// Responder step 2: exchange SAS confirmations. Returns true if both users confirmed.
pub(crate) async fn responder_confirm<S>(s: &mut S, local_ok: bool) -> Result<bool, NetError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let remote_ok = matches!(recv(s).await?, Msg::Confirm(true));
    send(s, &Msg::Confirm(local_ok && remote_ok)).await?;
    Ok(local_ok && remote_ok)
}

/// Responder refuses without revealing why (no usable offer).
pub(crate) async fn responder_refuse<S: AsyncWrite + Unpin>(s: &mut S) {
    let _ = send(s, &Msg::Reject).await;
}

impl HostOffer {
    pub fn secret(&self) -> [u8; 32] {
        self.offer.secret
    }
}

/// Test helper: same peer id and addresses, different (random) secret. Not part of the API.
#[doc(hidden)]
pub fn forge_for_test(o: &PairingOffer) -> PairingOffer {
    let mut f = PairingOffer::new(o.peer_id, o.addrs.clone(), DEFAULT_OFFER_TTL);
    f.expires_unix = o.expires_unix;
    f
}

/// Test helper: same peer id and secret, other addresses. Not part of the API.
#[doc(hidden)]
pub fn with_addrs_for_test(o: &PairingOffer, addrs: Vec<Multiaddr>) -> PairingOffer {
    let mut f = o.clone();
    f.addrs = addrs;
    f
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn offer_roundtrip_and_redaction() {
        let p = PeerId::random();
        let o = PairingOffer::new(
            p,
            vec!["/ip4/192.168.1.2/udp/1/quic-v1".parse().unwrap()],
            DEFAULT_OFFER_TTL,
        );
        let back = PairingOffer::parse(&o.to_qr_string()).unwrap();
        assert_eq!(back.peer_id, p);
        assert_eq!(back.secret, o.secret);
        assert_eq!(back.addrs, o.addrs);
        let dbg = format!("{o:?}");
        assert!(dbg.contains("redacted"));
        assert!(!dbg.contains(&URL_SAFE_NO_PAD.encode(o.secret)));
    }

    #[test]
    fn rejects_bad_and_expired_offers() {
        assert!(PairingOffer::parse("hello").is_err());
        assert!(PairingOffer::parse("nc-pair:v1:!!!").is_err());
        let mut o = PairingOffer::new(PeerId::random(), vec![], DEFAULT_OFFER_TTL);
        o.expires_unix = 1;
        assert!(PairingOffer::parse(&o.to_qr_string()).is_err());
    }

    #[test]
    fn sas_depends_on_transcript() {
        let s = [7u8; 32];
        let (a, b) = (PeerId::random(), PeerId::random());
        let t1 = transcript(&a, &b, &[1; 32], &[2; 32]);
        let t2 = transcript(&a, &b, &[1; 32], &[3; 32]);
        assert_eq!(sas(&s, &t1).len(), 6);
        assert_eq!(sas(&s, &t1), sas(&s, &t1));
        assert_ne!(mac(&s, b"init", &t1), mac(&s, b"init", &t2));
        assert_ne!(mac(&s, b"init", &t1), mac(&s, b"resp", &t1));
    }
}
