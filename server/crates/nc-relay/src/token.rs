//! Short-lived relay tokens, issued by the coordinator and checked by the relay.
//!
//! Format and HMAC byte layout live in proto/README.md. The HTTP API transports
//! v1.base64url(prost RelayToken); P2P auth transports the binary token directly.
//! Tokens are bound to one peer/account/device and live at most MAX_TTL_SECS.

use hmac::{Hmac, Mac};
use nc_protocol::pb::v1::RelayToken;
pub use nc_protocol::pb::v1::RelayTokenClaims as Claims;
use nc_protocol::wire::{decode_strict, relay_token_from_text, relay_token_to_text};
use prost::Message;
use sha2::Sha256;
use subtle::ConstantTimeEq;

/// Longest lifetime a token may have.
pub const MAX_TTL_SECS: u64 = 15 * 60;
/// Largest encoded token we will look at.
pub const MAX_TOKEN_LEN: usize = 2048;
const DOMAIN: &[u8] = b"nc-relay-token-v1.";

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

    fn mac(&self, claims: &[u8]) -> Vec<u8> {
        // HMAC accepts any key length; this cannot fail.
        let Ok(mut m) = Hmac::<Sha256>::new_from_slice(&self.0) else {
            return Vec::new();
        };
        m.update(DOMAIN);
        m.update(claims);
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
            peer: Some(peer.to_owned()),
            account: Some(account.to_owned()),
            device: Some(device.to_owned()),
            iat: Some(now),
            exp: Some(now + ttl_secs.min(MAX_TTL_SECS)),
        };
        let bytes = claims.encode_to_vec();
        let signature = self.mac(&bytes);
        let token = RelayToken {
            version: 1,
            claims: bytes,
            signature,
        };
        (relay_token_to_text(&token), claims)
    }

    /// Verifies signature and time window. The caller must also check `claims.peer` against the
    /// authenticated peer id of the connection.
    pub fn verify(&self, token: &str, now: u64) -> Result<Claims, TokenError> {
        if token.len() > MAX_TOKEN_LEN {
            return Err(TokenError::Malformed);
        }
        let token = relay_token_from_text(token).map_err(|_| TokenError::Malformed)?;
        self.verify_binary(&token, now)
    }

    /// Verify a generated binary token, including size, version, HMAC and lifetime.
    /// The caller must also bind its peer to the authenticated connection.
    pub fn verify_binary(&self, token: &RelayToken, now: u64) -> Result<Claims, TokenError> {
        if token.version != 1 || relay_token_to_text(token).len() > MAX_TOKEN_LEN {
            return Err(TokenError::Malformed);
        }
        if !bool::from(self.mac(&token.claims).as_slice().ct_eq(&token.signature)) {
            return Err(TokenError::BadSignature);
        }
        let c: Claims = decode_strict(&token.claims).map_err(|_| TokenError::Malformed)?;
        let (Some(_), Some(_), Some(_), Some(iat), Some(exp)) =
            (&c.peer, &c.account, &c.device, c.iat, c.exp)
        else {
            return Err(TokenError::Malformed);
        };
        if exp <= now || iat > now + 60 || exp.saturating_sub(iat) > MAX_TTL_SECS {
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
        let mut forged = relay_token_from_text(&t).unwrap();
        let (other, _) = k.issue("peerB", "acct", "dev", 1000, 600);
        forged.claims = relay_token_from_text(&other).unwrap().claims;
        assert_eq!(
            k.verify(&relay_token_to_text(&forged), 1001),
            Err(TokenError::BadSignature)
        );
        assert_eq!(k.verify("garbage", 1001), Err(TokenError::Malformed));
        // TTL clamped.
        let (_, c) = k.issue("p", "a", "d", 0, 99_999);
        assert_eq!(c.exp, Some(MAX_TTL_SECS));
        assert!(!format!("{k:?}").contains('9'));
    }
}

#[cfg(test)]
mod golden {
    #[test]
    fn issued_token_has_fixed_bytes() {
        let (text, _) = super::TokenKey::new([9; 32]).issue("peerA", "acct", "dev", 1000, 600);
        assert_eq!(text, "v1.CAESGAoFcGVlckESBGFjY3QaA2RldiDoByjADBogunHkde5oy3zzFC_ZckY0OrkilT3VkvoOdczG3ei7Yi4");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod boundary_tests {
    use super::*;
    #[test]
    fn preserves_caps_unknown_field_and_time_checks() {
        let key = TokenKey::new([9; 32]);
        let (text, claims) = key.issue("peerA", "acct", "dev", 1000, 600);
        assert_eq!(
            key.verify(&"x".repeat(MAX_TOKEN_LEN + 1), 1000),
            Err(TokenError::Malformed)
        );
        assert_eq!(key.verify(&text, 1600), Err(TokenError::Expired));
        assert_eq!(key.verify(&text, 900), Err(TokenError::Expired));
        let mut token = relay_token_from_text(&text).unwrap();
        token.version = 2;
        assert_eq!(key.verify_binary(&token, 1000), Err(TokenError::Malformed));
        token.version = 1;
        token.claims.extend_from_slice(&[0x78, 1]);
        token.signature = key.mac(&token.claims);
        assert_eq!(key.verify_binary(&token, 1000), Err(TokenError::Malformed));
        token.claims = Claims {
            exp: Some(claims.iat.unwrap() + MAX_TTL_SECS + 1),
            ..claims
        }
        .encode_to_vec();
        token.signature = key.mac(&token.claims);
        assert_eq!(key.verify_binary(&token, 1000), Err(TokenError::Expired));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod required_claim_tests {
    use super::*;
    #[test]
    fn every_claim_is_required_even_when_zero_is_valid() {
        let key = TokenKey::new([9; 32]);
        let (text, claims) = key.issue("", "", "", 0, 600);
        assert_eq!(key.verify(&text, 0).unwrap(), claims);
        for i in 0..5 {
            let mut incomplete = claims.clone();
            match i {
                0 => incomplete.peer = None,
                1 => incomplete.account = None,
                2 => incomplete.device = None,
                3 => incomplete.iat = None,
                _ => incomplete.exp = None,
            }
            let bytes = incomplete.encode_to_vec();
            let token = RelayToken {
                version: 1,
                signature: key.mac(&bytes),
                claims: bytes,
            };
            assert_eq!(key.verify_binary(&token, 0), Err(TokenError::Malformed));
        }
    }
}
