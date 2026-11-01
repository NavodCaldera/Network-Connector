//! Short-lived session keys certified by the device root key.

use ed25519_dalek::{Signer as _, SigningKey};
use nc_protocol::pb::v1::{KeyAssurance, SessionCert, SignedSessionCert};
use nc_protocol::{sign_domain, verify_domain, ProtocolError, PROTOCOL_VERSION};
use prost::Message;

use crate::keystore::{random_seed, Assurance, DeviceKeyStore, IdentityError, RootSigner};

/// Domain for session-certificate signatures.
pub const DOMAIN_SESSION_CERT: &str = "nc.v1.SessionCert";
/// Longest session lifetime accepted (24 h).
pub const MAX_SESSION_TTL_MS: i64 = 24 * 60 * 60 * 1000;
/// Clock skew tolerated on `not_before` (5 min).
const SKEW_MS: i64 = 5 * 60 * 1000;
const MAX_CERT_BYTES: usize = 1024;

/// An ephemeral session key plus its certificate. The private key never leaves memory and is zeroised on drop.
pub struct SessionKey {
    key: SigningKey,
    /// Certificate to present to peers.
    pub cert: SignedSessionCert,
}

impl std::fmt::Debug for SessionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionKey").finish_non_exhaustive()
    }
}

impl nc_protocol::Signer for SessionKey {
    fn public_key(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }
    fn sign(&self, msg: &[u8]) -> Result<[u8; 64], ProtocolError> {
        Ok(self.key.sign(msg).to_bytes())
    }
}

/// Create a session key valid for `[now, now + ttl_ms)`, certified by `root`.
pub fn issue_session(
    root: &dyn DeviceKeyStore,
    now_unix_ms: i64,
    ttl_ms: i64,
) -> Result<SessionKey, IdentityError> {
    if ttl_ms <= 0 || ttl_ms > MAX_SESSION_TTL_MS {
        return Err(IdentityError::Cert("ttl out of range"));
    }
    let seed = random_seed()?;
    let key = SigningKey::from_bytes(&seed);
    let cert = SessionCert {
        version: Some(PROTOCOL_VERSION),
        device_root_key: root.public_key().to_vec(),
        session_key: key.verifying_key().to_bytes().to_vec(),
        not_before_unix_ms: now_unix_ms,
        not_after_unix_ms: now_unix_ms.saturating_add(ttl_ms),
        root_assurance: match root.assurance() {
            Assurance::Software => KeyAssurance::Software,
            Assurance::Hardware => KeyAssurance::Hardware,
        } as i32,
    };
    let bytes = cert.encode_to_vec();
    let signer: &dyn nc_protocol::Signer = &RootSigner(root);
    let signature = sign_domain(signer, DOMAIN_SESSION_CERT, &bytes)?;
    Ok(SessionKey {
        key,
        cert: SignedSessionCert {
            cert: bytes,
            signature: signature.to_vec(),
        },
    })
}

/// Verify a session certificate: signature by the root key it names, optional pinned root (from pairing or a
/// coordinator certificate), lifetime ≤ 24 h, and `now` inside the validity window.
pub fn verify_session_cert(
    signed: &SignedSessionCert,
    expected_root: Option<&[u8; 32]>,
    now_unix_ms: i64,
) -> Result<SessionCert, IdentityError> {
    if signed.cert.len() > MAX_CERT_BYTES {
        return Err(IdentityError::Cert("too large"));
    }
    let cert = SessionCert::decode(signed.cert.as_slice())
        .map_err(|_| IdentityError::Cert("decode failed"))?;
    if let Some(root) = expected_root {
        if cert.device_root_key.as_slice() != root.as_slice() {
            return Err(IdentityError::Cert("unexpected root key"));
        }
    }
    verify_domain(
        &cert.device_root_key,
        DOMAIN_SESSION_CERT,
        &signed.cert,
        &signed.signature,
    )?;
    if cert.session_key.len() != 32 {
        return Err(IdentityError::Cert("bad session key"));
    }
    match &cert.version {
        Some(v) if v.major == PROTOCOL_VERSION.major => {}
        _ => return Err(IdentityError::Cert("unsupported version")),
    }
    let ttl = cert
        .not_after_unix_ms
        .saturating_sub(cert.not_before_unix_ms);
    if ttl <= 0 || ttl > MAX_SESSION_TTL_MS {
        return Err(IdentityError::Cert("lifetime out of range"));
    }
    if now_unix_ms.saturating_add(SKEW_MS) < cert.not_before_unix_ms {
        return Err(IdentityError::Cert("not yet valid"));
    }
    if now_unix_ms >= cert.not_after_unix_ms {
        return Err(IdentityError::Cert("expired"));
    }
    Ok(cert)
}
