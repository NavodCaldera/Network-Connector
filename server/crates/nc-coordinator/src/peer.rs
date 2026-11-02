//! 1.L14: device libp2p PeerId registration and the peer hints a lease carries.
//!
//! A device proves that its libp2p key belongs to it twice: the HTTP request is signed by the device
//! key (NC-REQ-V1), and the libp2p key signs `nc.v1.PeerBinding\0 || device_public_key`. The server
//! derives the PeerId from the libp2p public key itself, so a device cannot claim another device's
//! PeerId. Hints are connection details only, never authorisation (AGENT.md §1 rule 2).

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ed25519_dalek::{Signature, VerifyingKey};
use libp2p::multiaddr::Protocol;
use libp2p::{identity, Multiaddr, PeerId};
use nc_protocol::pb::v1::{PeerHints, PeerRegisterRequest, PeerRegisterResponse};
use sqlx::Row;
use uuid::Uuid;

use super::*;

/// How long a peer registration stays fresh; leases carry no hints for a stale one.
pub const PEER_TTL_SECS: i64 = 600;
/// Most addresses one device may register.
pub const MAX_PEER_ADDRS: usize = 16;
/// Longest accepted multiaddr text.
pub const MAX_ADDR_LEN: usize = 256;
/// Domain prefix of the binding statement signed by the libp2p key.
pub const PEER_BINDING_DOMAIN: &[u8] = b"nc.v1.PeerBinding\0";

/// Bytes the libp2p key signs to bind itself to `device_public_key`.
pub fn binding_message(device_public_key: &[u8; 32]) -> Vec<u8> {
    let mut m = PEER_BINDING_DOMAIN.to_vec();
    m.extend_from_slice(device_public_key);
    m
}

/// A request that passed every check.
#[derive(Debug, PartialEq, Eq)]
pub struct Verified {
    pub peer_id: String,
    pub addrs: Vec<String>,
}

fn bad(m: &str) -> ApiError {
    ApiError::bad_request(m.to_owned())
}

/// Checks the binding signature, derives the PeerId and validates the addresses.
pub fn verify_request(
    req: &PeerRegisterRequest,
    device_public_key: &[u8; 32],
) -> Result<Verified, ApiError> {
    let pk: [u8; 32] = URL_SAFE_NO_PAD
        .decode(&req.libp2p_public_key)
        .ok()
        .and_then(|v| v.try_into().ok())
        .ok_or_else(|| bad("libp2p_public_key must be a base64url Ed25519 key"))?;
    let sig: [u8; 64] = URL_SAFE_NO_PAD
        .decode(&req.binding_signature)
        .ok()
        .and_then(|v| v.try_into().ok())
        .ok_or_else(|| bad("binding_signature must be a base64url Ed25519 signature"))?;
    let vk = VerifyingKey::from_bytes(&pk).map_err(|_| bad("invalid libp2p public key"))?;
    vk.verify_strict(
        &binding_message(device_public_key),
        &Signature::from_bytes(&sig),
    )
    .map_err(|_| ApiError::unauthorized("peer binding signature does not verify"))?;
    let lp = identity::ed25519::PublicKey::try_from_bytes(&pk)
        .map_err(|_| bad("invalid libp2p public key"))?;
    let derived = PeerId::from_public_key(&identity::PublicKey::from(lp)).to_string();
    if derived != req.peer_id {
        return Err(bad("peer_id does not match libp2p_public_key"));
    }
    if req.addrs.len() > MAX_PEER_ADDRS {
        return Err(bad("too many addresses"));
    }
    let mut addrs: Vec<String> = Vec::with_capacity(req.addrs.len());
    for a in &req.addrs {
        let a = validate_addr(a).ok_or_else(|| bad("invalid multiaddr"))?;
        if !addrs.contains(&a) {
            addrs.push(a);
        }
    }
    Ok(Verified {
        peer_id: derived,
        addrs,
    })
}

/// Plain transport addresses only: no `/p2p`, `/p2p-circuit` or other protocols, so a device cannot
/// make another peer's identity or a relay circuit look like its own address.
fn validate_addr(a: &str) -> Option<String> {
    if a.is_empty() || a.len() > MAX_ADDR_LEN {
        return None;
    }
    let m: Multiaddr = a.parse().ok()?;
    let mut has_host = false;
    for p in m.iter() {
        match p {
            Protocol::Ip4(_)
            | Protocol::Ip6(_)
            | Protocol::Dns(_)
            | Protocol::Dns4(_)
            | Protocol::Dns6(_) => has_host = true,
            Protocol::Tcp(_) | Protocol::Udp(_) | Protocol::QuicV1 => {}
            _ => return None,
        }
    }
    has_host.then(|| m.to_string())
}

/// Validates the configured relay address: a transport address followed by `/p2p/<relay id>`.
pub fn validate_relay_addr(a: &str) -> Option<String> {
    let m: Multiaddr = a.trim().parse().ok()?;
    let mut it: Vec<Protocol> = m.iter().collect();
    let Some(Protocol::P2p(_)) = it.pop() else {
        return None;
    };
    let base: Multiaddr = it.into_iter().collect();
    validate_addr(&base.to_string())?;
    Some(m.to_string())
}

/// `POST /v1/devices/peer`.
pub async fn register(
    State(s): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<PeerRegisterResponse>, ApiError> {
    let (c, b) = read_signed(&s, &method, &uri, &headers, body).await?;
    let req: PeerRegisterRequest = parse(&b)?;
    let d = device(&s, &c).await?;
    let v = verify_request(&req, &c.public_key)?;
    let t = now();
    let res = sqlx::query(
        "UPDATE devices SET peer_id=$1, peer_addrs=$2, peer_seen_at=$3 WHERE id=$4 AND public_key IS NOT NULL",
    )
    .bind(&v.peer_id)
    .bind(&v.addrs)
    .bind(t)
    .bind(d.id)
    .execute(&s.pool)
    .await;
    match res {
        Ok(r) if r.rows_affected() == 1 => {}
        Ok(_) => return Err(ApiError::unauthorized("device not registered")),
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            return Err(ApiError::conflict(
                "peer_id_taken",
                "peer id is registered to another device",
            ))
        }
        Err(e) => return Err(e.into()),
    }
    tracing::info!(device_id = %d.id, addrs = v.addrs.len(), "peer registered");
    Ok(Json(PeerRegisterResponse {
        peer_id: v.peer_id,
        expires_at: (t + PEER_TTL_SECS) as u64,
    }))
}

/// Hints for a fresh lease: the requester's fresh registration and, when relay tokens are enabled and
/// the helper has a fresh PeerId, a relay token for the helper scoped to this lease and requester.
pub(crate) async fn hints_for_lease(
    s: &AppState,
    helper: &Device,
    task: Uuid,
    lease: Uuid,
) -> Result<Option<PeerHints>, ApiError> {
    let t = now();
    let row = sqlx::query(
        "SELECT d.peer_id, d.peer_addrs FROM tasks t JOIN jobs j ON j.id=t.job_id
         JOIN devices d ON d.id=j.requester
         WHERE t.id=$1 AND d.public_key IS NOT NULL AND d.peer_id IS NOT NULL AND d.peer_seen_at > $2",
    )
    .bind(task)
    .bind(t - PEER_TTL_SECS)
    .fetch_optional(&s.pool)
    .await?;
    let Some(row) = row else { return Ok(None) };
    let peer_id: String = row.try_get("peer_id")?;
    let addrs: Vec<String> = row.try_get("peer_addrs")?;
    let mut hints = PeerHints {
        peer_id,
        addrs,
        relay_addr: s.relay_addr.clone().unwrap_or_default(),
        ..Default::default()
    };
    if let Some(key) = &s.relay_key {
        let own: Option<String> = sqlx::query_scalar(
            "SELECT peer_id FROM devices WHERE id=$1 AND peer_id IS NOT NULL AND peer_seen_at > $2",
        )
        .bind(helper.id)
        .bind(t - PEER_TTL_SECS)
        .fetch_optional(&s.pool)
        .await?;
        if let Some(own) = own {
            let (token, claims) = key.issue_scoped(
                &own,
                &helper.account,
                &helper.id.to_string(),
                &lease.to_string(),
                &hints.peer_id,
                nc_relay::unix_now(),
                RELAY_TOKEN_TTL_SECS,
            );
            hints.relay_token = token;
            hints.relay_token_expires_at = claims.exp.unwrap_or(0);
        }
    }
    Ok(Some(hints))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn req(lp: &SigningKey, device: &[u8; 32], addrs: &[&str]) -> PeerRegisterRequest {
        let pk =
            identity::ed25519::PublicKey::try_from_bytes(lp.verifying_key().as_bytes()).unwrap();
        PeerRegisterRequest {
            peer_id: PeerId::from_public_key(&identity::PublicKey::from(pk)).to_string(),
            libp2p_public_key: URL_SAFE_NO_PAD.encode(lp.verifying_key().as_bytes()),
            binding_signature: URL_SAFE_NO_PAD.encode(lp.sign(&binding_message(device)).to_bytes()),
            addrs: addrs.iter().map(|a| a.to_string()).collect(),
        }
    }

    #[test]
    fn binding_derives_peer_id_and_rejects_spoofing() {
        let lp = SigningKey::from_bytes(&[7; 32]);
        let dev = [1u8; 32];
        let r = req(
            &lp,
            &dev,
            &[
                "/ip4/192.168.1.5/udp/4001/quic-v1",
                "/ip4/192.168.1.5/udp/4001/quic-v1",
            ],
        );
        let v = verify_request(&r, &dev).unwrap();
        assert_eq!(v.peer_id, r.peer_id);
        assert!(v.peer_id.starts_with("12D3KooW"));
        assert_eq!(v.addrs.len(), 1, "duplicates collapse");
        // Same binding replayed by a different device key: signature does not cover it.
        assert!(verify_request(&r, &[2u8; 32]).is_err());
        // Claiming another PeerId with a valid binding of one's own key.
        let other = req(&SigningKey::from_bytes(&[8; 32]), &dev, &[]);
        let spoof = PeerRegisterRequest {
            peer_id: other.peer_id,
            ..r.clone()
        };
        assert!(verify_request(&spoof, &dev).is_err());
        // Signature by a different libp2p key.
        let forged = PeerRegisterRequest {
            binding_signature: other.binding_signature,
            ..r.clone()
        };
        assert!(verify_request(&forged, &dev).is_err());
        let short = PeerRegisterRequest {
            libp2p_public_key: "AAAA".into(),
            ..r
        };
        assert!(verify_request(&short, &dev).is_err());
    }

    #[test]
    fn addresses_are_plain_transport_and_capped() {
        let lp = SigningKey::from_bytes(&[7; 32]);
        let dev = [1u8; 32];
        for ok in [
            "/ip4/10.0.0.2/tcp/4001",
            "/ip6/::1/udp/9/quic-v1",
            "/dns4/a.example/tcp/443",
        ] {
            assert!(verify_request(&req(&lp, &dev, &[ok]), &dev).is_ok(), "{ok}");
        }
        let pid = req(&lp, &dev, &[]).peer_id;
        for bad in [
            "",
            "not-a-multiaddr",
            "/tcp/4001",
            &format!("/ip4/1.2.3.4/tcp/1/p2p/{pid}"),
            &format!("/ip4/1.2.3.4/tcp/1/p2p/{pid}/p2p-circuit"),
            "/ip4/1.2.3.4/tcp/1/ws",
        ] {
            assert!(
                verify_request(&req(&lp, &dev, &[bad]), &dev).is_err(),
                "{bad}"
            );
        }
        let long = format!("/dns4/{}/tcp/1", "a".repeat(MAX_ADDR_LEN));
        assert!(verify_request(&req(&lp, &dev, &[&long]), &dev).is_err());
        let many: Vec<String> = (0..=MAX_PEER_ADDRS)
            .map(|i| format!("/ip4/10.0.0.{i}/tcp/1"))
            .collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        assert!(verify_request(&req(&lp, &dev, &many), &dev).is_err());
    }

    #[test]
    fn relay_addr_needs_relay_peer_id() {
        let pid = req(&SigningKey::from_bytes(&[7; 32]), &[1; 32], &[]).peer_id;
        assert!(
            validate_relay_addr(&format!("/ip4/127.0.0.1/udp/4001/quic-v1/p2p/{pid}")).is_some()
        );
        assert!(validate_relay_addr("/ip4/127.0.0.1/udp/4001/quic-v1").is_none());
        assert!(validate_relay_addr(&format!("/p2p/{pid}")).is_none());
    }
}
