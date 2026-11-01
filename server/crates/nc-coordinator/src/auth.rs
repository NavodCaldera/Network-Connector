//! Signed requests: every `/v1` call is signed by the device's Ed25519 key.
//!
//! Headers:
//! - `x-nc-key`: base64url(32-byte Ed25519 public key)
//! - `x-nc-ts`: Unix seconds; must be within [`MAX_SKEW_SECS`] of server time
//! - `x-nc-nonce`: 16..=64 chars `[A-Za-z0-9_-]`, unique per key within the window (replay protection)
//! - `x-nc-sig`: base64url(Ed25519 signature over [`signing_string`])
//!
//! Phase 0 has no OIDC accounts yet: `register` additionally needs the owner's enrolment secret
//! (`x-nc-enroll`, env `NC_ENROLL_SECRET`), and every enrolled device belongs to account `owner`.
//!
//! Exact signing bytes are specified in `proto/README.md` and pinned by a fixed-vector test.

use axum::http::{HeaderMap, Method};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};

use crate::error::ApiError;

/// Allowed clock difference.
pub const MAX_SKEW_SECS: i64 = 300;

/// Verified caller.
#[derive(Debug, Clone)]
pub struct Caller {
    pub public_key: [u8; 32],
    pub nonce: String,
    pub ts: i64,
}

/// Exact bytes that are signed.
pub fn signing_string(method: &str, path: &str, ts: i64, nonce: &str, body: &[u8]) -> String {
    format!(
        "NC-REQ-V1\n{method}\n{path}\n{ts}\n{nonce}\n{}",
        hex::encode(Sha256::digest(body))
    )
}

fn header<'a>(h: &'a HeaderMap, name: &str) -> Result<&'a str, ApiError> {
    h.get(name)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| ApiError::unauthorized(format!("missing {name} header")))
}

/// Checks headers and signature. Replay (nonce reuse) is checked by the caller against the database.
pub fn verify(
    method: &Method,
    path: &str,
    headers: &HeaderMap,
    body: &[u8],
    now: i64,
) -> Result<Caller, ApiError> {
    let key_b = URL_SAFE_NO_PAD
        .decode(header(headers, "x-nc-key")?)
        .map_err(|_| ApiError::unauthorized("bad x-nc-key"))?;
    let public_key: [u8; 32] = key_b
        .try_into()
        .map_err(|_| ApiError::unauthorized("bad x-nc-key"))?;
    let vk = VerifyingKey::from_bytes(&public_key)
        .map_err(|_| ApiError::unauthorized("bad x-nc-key"))?;
    let ts: i64 = header(headers, "x-nc-ts")?
        .parse()
        .map_err(|_| ApiError::unauthorized("bad x-nc-ts"))?;
    if (now - ts).abs() > MAX_SKEW_SECS {
        return Err(ApiError::unauthorized(
            "request timestamp outside allowed window",
        ));
    }
    let nonce = header(headers, "x-nc-nonce")?;
    let nonce_ok = (16..=64).contains(&nonce.len())
        && nonce
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !nonce_ok {
        return Err(ApiError::unauthorized("bad x-nc-nonce"));
    }
    let sig_b = URL_SAFE_NO_PAD
        .decode(header(headers, "x-nc-sig")?)
        .map_err(|_| ApiError::unauthorized("bad x-nc-sig"))?;
    let sig = Signature::from_slice(&sig_b).map_err(|_| ApiError::unauthorized("bad x-nc-sig"))?;
    let msg = signing_string(method.as_str(), path, ts, nonce, body);
    vk.verify(msg.as_bytes(), &sig)
        .map_err(|_| ApiError::unauthorized("signature does not verify"))?;
    Ok(Caller {
        public_key,
        nonce: nonce.to_owned(),
        ts,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn signed(sk: &SigningKey, path: &str, ts: i64, nonce: &str, body: &[u8]) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            "x-nc-key",
            URL_SAFE_NO_PAD
                .encode(sk.verifying_key().to_bytes())
                .parse()
                .unwrap(),
        );
        h.insert("x-nc-ts", ts.to_string().parse().unwrap());
        h.insert("x-nc-nonce", nonce.parse().unwrap());
        let sig = sk.sign(signing_string("POST", path, ts, nonce, body).as_bytes());
        h.insert(
            "x-nc-sig",
            URL_SAFE_NO_PAD.encode(sig.to_bytes()).parse().unwrap(),
        );
        h
    }

    #[test]
    fn accepts_good_rejects_tampering() {
        let sk = SigningKey::from_bytes(&[3u8; 32]);
        let h = signed(&sk, "/v1/x", 1000, "abcdefghijklmnop", b"{}");
        assert!(verify(&Method::POST, "/v1/x", &h, b"{}", 1000).is_ok());
        // Body changed.
        assert!(verify(&Method::POST, "/v1/x", &h, b"{ }", 1000).is_err());
        // Path changed.
        assert!(verify(&Method::POST, "/v1/y", &h, b"{}", 1000).is_err());
        // Too old.
        assert!(verify(&Method::POST, "/v1/x", &h, b"{}", 2000).is_err());
        // Missing header.
        let mut h2 = h.clone();
        h2.remove("x-nc-sig");
        assert!(verify(&Method::POST, "/v1/x", &h2, b"{}", 1000).is_err());
    }
}

#[cfg(test)]
mod fixed_vector {
    #[test]
    fn request_signing_bytes_do_not_drift() {
        assert_eq!(super::signing_string("POST", "/v1/leases/request", 1700000000,
            "abcdefghijklmnop", b"{}").as_bytes(),
            b"NC-REQ-V1\nPOST\n/v1/leases/request\n1700000000\nabcdefghijklmnop\n44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a");
    }
}
