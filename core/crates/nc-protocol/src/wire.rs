//! Binary protocol boundaries. Callers enforce their size caps before decoding.
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use prost::Message;

/// Boundary decoding failure; never contains payloads or token text.
#[derive(Debug, thiserror::Error)]
pub enum WireError {
    /// Malformed protobuf.
    #[error("malformed protobuf")]
    Protobuf(#[from] prost::DecodeError),
    /// Unknown, duplicate or noncanonical fields.
    #[error("unknown fields or noncanonical protobuf encoding")]
    NonCanonical,
    /// Invalid token encoding/version.
    #[error("invalid token encoding or version")]
    Token,
}
use crate::pb::v1::{PairingOffer, RelayAuthFrame, RelayToken, RelayTokenResponse};

/// Decode canonical prost bytes, rejecting unknown fields (including nested ones),
/// duplicate fields and noncanonical encodings instead of silently discarding them.
/// This does not replace semantic, signature, time or caller-specific size validation.
pub fn decode_strict<M: Message + Default>(bytes: &[u8]) -> Result<M, WireError> {
    let value = M::decode(bytes)?;
    if value.encode_to_vec() != bytes {
        return Err(WireError::NonCanonical);
    }
    Ok(value)
}

/// Decode the JSON API's opaque token text to its binary message. HMAC verification
/// remains the relay's responsibility. The caller checks the text length first.
pub fn relay_token_from_text(text: &str) -> Result<RelayToken, WireError> {
    if text.len() > 2048 {
        return Err(WireError::Token);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(text.strip_prefix("v1.").ok_or(WireError::Token)?)
        .map_err(|_| WireError::Token)?;
    let token: RelayToken = decode_strict(&bytes)?;
    if token.version != 1 {
        return Err(WireError::Token);
    }
    Ok(token)
}

/// Encode the binary token for the JSON HTTP API. Never log this text.
pub fn relay_token_to_text(token: &RelayToken) -> String {
    format!("v1.{}", URL_SAFE_NO_PAD.encode(token.encode_to_vec()))
}

macro_rules! redacted_debug {
    ($($ty:ty),+ $(,)?) => {$ (
        impl std::fmt::Debug for $ty {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(concat!(stringify!($ty), "(<redacted>)"))
            }
        }
    )+};
}
redacted_debug!(PairingOffer, RelayAuthFrame, RelayToken, RelayTokenResponse);
