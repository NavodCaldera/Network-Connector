//! Short-lived relay tokens, issued by the coordinator and checked by the relay.
//!
//! Format: `v1.<base64url(claims JSON)>.<base64url(HMAC-SHA256(key, "nc-relay-token-v1." || claims_b64))>`.
//! The coordinator and the relay share the 32-byte key (env `NC_RELAY_TOKEN_KEY`). Tokens are bound to one
//! libp2p peer id, one account and one device, and live at most [`MAX_TTL_SECS`].
//!
//! TODO(proto): the token claims are a wire format and should move to `proto/` (Core owns it); listed in
//! the net-agent hand-back "Requests".

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;

/// Longest lifetime a token may have.
pub const MAX_TTL_SECS: u64 = 15 * 60;
/// Largest encoded token we will look at.
pub const MAX_TOKEN_LEN: usize = 2048;
const DOMAIN: &[u8] = b"nc-relay-token-v1.";

/// What a token grants.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claims {
    /// libp2p peer id the token is for.
    pub peer: String,
    /// Account id (quota key).
    pub account: String,
    /// Device id (quota key).
    pub device: String,
    /// Issued at, Unix seconds.
    pub iat: u64,
    /// Expires at, Unix seconds.
    pub exp: u64,
}

/// Why a token was refused. Deliberately coarse; never echoes the token.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TokenError {
    /// Malformed or wrong version.
    #[error("malformed token")]
    Malformed,
    /// Signature does not verify.
    #[error("bad signature")]
    BadSignature,
    /// Expired, not yet valid, or lifetime too long.
    #[error("token not valid at this time")]
    Expired,
}

/// HMAC key shared by coordinator and relay. `Debug` is redacted.
#[derive(Clone)]
pub struct TokenKey([u8; 32]);

impl std::fmt::Debug for TokenKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TokenKey(<redacted>)")
    }
}

impl TokenKey {
    /// From raw bytes.
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// From 64 hex characters (the env var format).
    pub fn from_hex(s: &str) -> Option<Self> {
        let v = hex::decode(s.trim()).ok()?;
        Some(Self(v.try_into().ok()?))
    }

    fn mac(&self, claims_b64: &str) -> Vec<u8> {
        // HMAC accepts any key length; this cannot fail.
        let Ok(mut m) = Hmac::<Sha256>::new_from_slice(&self.0) else {
            return Vec::new();
        };
        m.update(DOMAIN);
        m.update(claims_b64.as_bytes());
        m.finalize().into_bytes().to_vec()
    }

    /// Issues a token. `ttl_secs` is clamped to [`MAX_TTL_SECS`].
    pub fn issue(
        &self,
        peer: &str,
        account: &str,
        device: &str,
        now: u64,
        ttl_secs: u64,
    ) -> (String, Claims) {
        let claims = Claims {
            peer: peer.to_owned(),
            account: account.to_owned(),
            device: device.to_owned(),
            iat: now,
            exp: now + ttl_secs.min(MAX_TTL_SECS),
        };
        // Serialising strings and integers cannot fail.
        let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap_or_default());
        let sig = URL_SAFE_NO_PAD.encode(self.mac(&body));
        (format!("v1.{body}.{sig}"), claims)
    }

    /// Verifies signature and time window. The caller must also check `claims.peer` against the
    /// authenticated peer id of the connection.
    pub fn verify(&self, token: &str, now: u64) -> Result<Claims, TokenError> {
        if token.len() > MAX_TOKEN_LEN {
            return Err(TokenError::Malformed);
        }
        let mut parts = token.split('.');
        let (Some("v1"), Some(body), Some(sig), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(TokenError::Malformed);
        };
        let sig = URL_SAFE_NO_PAD
            .decode(sig)
            .map_err(|_| TokenError::Malformed)?;
        if !bool::from(self.mac(body).as_slice().ct_eq(&sig)) {
            return Err(TokenError::BadSignature);
        }
        let json = URL_SAFE_NO_PAD
            .decode(body)
            .map_err(|_| TokenError::Malformed)?;
        let c: Claims = serde_json::from_slice(&json).map_err(|_| TokenError::Malformed)?;
        if c.exp <= now || c.iat > now + 60 || c.exp.saturating_sub(c.iat) > MAX_TTL_SECS {
            return Err(TokenError::Expired);
        }
        Ok(c)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn issue_verify_and_reject() {
        let k = TokenKey::new([9; 32]);
        let (t, c) = k.issue("peerA", "acct", "dev", 1000, 600);
        assert_eq!(k.verify(&t, 1001).unwrap(), c);
        // Expired.
        assert_eq!(k.verify(&t, 1600), Err(TokenError::Expired));
        // Wrong key.
        assert_eq!(
            TokenKey::new([1; 32]).verify(&t, 1001),
            Err(TokenError::BadSignature)
        );
        // Tampered claims.
        let mut parts: Vec<&str> = t.split('.').collect();
        let (forged, _) = k.issue("peerB", "acct", "dev", 1000, 600);
        let fb: Vec<&str> = forged.split('.').collect();
        parts[1] = fb[1];
        assert_eq!(
            k.verify(&parts.join("."), 1001),
            Err(TokenError::BadSignature)
        );
        assert_eq!(k.verify("garbage", 1001), Err(TokenError::Malformed));
        // TTL clamped.
        let (_, c) = k.issue("p", "a", "d", 0, 99_999);
        assert_eq!(c.exp, MAX_TTL_SECS);
        assert!(!format!("{k:?}").contains('9'));
    }
}
