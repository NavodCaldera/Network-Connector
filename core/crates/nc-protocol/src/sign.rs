//! Domain-separated Ed25519 signing and manifest verification.

use ed25519_dalek::{Signature, VerifyingKey};
use prost::Message;

use crate::pb::v1::{Limits, SignedManifest, TaskManifest};
use crate::{PROFILE_CPU_CORE_V1, PROTOCOL_VERSION};

/// Domain for manifest signatures.
pub const DOMAIN_MANIFEST: &str = "nc.v1.TaskManifest";
/// Domain for module-hash signatures by the publisher.
pub const DOMAIN_MODULE: &str = "nc.v1.module";
/// Largest encoded manifest accepted (bytes), checked before any parsing.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
/// Most inputs one manifest may list.
pub const MAX_INPUTS: usize = 1024;

/// Errors from signing and verification. Messages never contain key material or job data.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProtocolError {
    /// The signer could not produce a signature (e.g. hardware key unavailable).
    #[error("signing failed: {0}")]
    Sign(String),
    /// A key had the wrong length or is not a valid Ed25519 point.
    #[error("bad public key")]
    BadKey,
    /// Signature malformed or does not verify.
    #[error("bad signature")]
    BadSignature,
    /// The signer is not one the caller trusts.
    #[error("untrusted signer")]
    UntrustedSigner,
    /// Encoded message too large.
    #[error("message too large")]
    TooLarge,
    /// Protobuf decode failed.
    #[error("decode failed")]
    Decode,
    /// The manifest is structurally invalid.
    #[error("invalid manifest: {0}")]
    Invalid(&'static str),
    /// Unsupported protocol major version.
    #[error("unsupported protocol version")]
    Version,
    /// The manifest's expiry is in the past.
    #[error("manifest expired")]
    Expired,
}

/// Something that can sign with an Ed25519 key: the device root key store, a session key, a test key.
pub trait Signer {
    /// Public key (32 bytes).
    fn public_key(&self) -> [u8; 32];
    /// Ed25519 signature over `msg`.
    fn sign(&self, msg: &[u8]) -> Result<[u8; 64], ProtocolError>;
}

fn message(domain: &str, bytes: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(domain.len() + 1 + bytes.len());
    m.extend_from_slice(domain.as_bytes());
    m.push(0);
    m.extend_from_slice(bytes);
    m
}

/// Sign `domain || 0 || bytes`.
pub fn sign_domain(
    signer: &dyn Signer,
    domain: &str,
    bytes: &[u8],
) -> Result<[u8; 64], ProtocolError> {
    signer.sign(&message(domain, bytes))
}

/// Verify a signature made with [`sign_domain`]. Uses strict verification (rejects malleable signatures and
/// small-order keys).
pub fn verify_domain(
    public_key: &[u8],
    domain: &str,
    bytes: &[u8],
    signature: &[u8],
) -> Result<(), ProtocolError> {
    let pk: [u8; 32] = public_key.try_into().map_err(|_| ProtocolError::BadKey)?;
    let vk = VerifyingKey::from_bytes(&pk).map_err(|_| ProtocolError::BadKey)?;
    let sig: [u8; 64] = signature
        .try_into()
        .map_err(|_| ProtocolError::BadSignature)?;
    let sig = Signature::from_bytes(&sig);
    vk.verify_strict(&message(domain, bytes), &sig)
        .map_err(|_| ProtocolError::BadSignature)
}

/// Publisher signature over a module hash.
pub fn sign_module(
    signer: &dyn Signer,
    module_sha256: &[u8; 32],
) -> Result<[u8; 64], ProtocolError> {
    sign_domain(signer, DOMAIN_MODULE, module_sha256)
}

/// Check a publisher signature over a module hash.
pub fn verify_module_signature(
    publisher_key: &[u8],
    module_sha256: &[u8],
    signature: &[u8],
) -> Result<(), ProtocolError> {
    if module_sha256.len() != 32 {
        return Err(ProtocolError::Invalid("module_sha256 must be 32 bytes"));
    }
    verify_domain(publisher_key, DOMAIN_MODULE, module_sha256, signature)
}

/// Encode and sign a manifest.
pub fn sign_manifest(
    m: &TaskManifest,
    signer: &dyn Signer,
) -> Result<SignedManifest, ProtocolError> {
    let bytes = m.encode_to_vec();
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(ProtocolError::TooLarge);
    }
    let signature = sign_domain(signer, DOMAIN_MANIFEST, &bytes)?;
    Ok(SignedManifest {
        manifest: bytes,
        signer_key: signer.public_key().to_vec(),
        signature: signature.to_vec(),
    })
}

/// What the verifying device trusts.
pub struct ManifestPolicy<'a> {
    /// Issuers (coordinator or paired requester keys) whose manifests are accepted.
    pub trusted_issuers: &'a [[u8; 32]],
    /// Publishers whose modules are accepted.
    pub trusted_publishers: &'a [[u8; 32]],
    /// Current time, Unix ms.
    pub now_unix_ms: i64,
}

fn check_limits(l: &Limits) -> Result<(), ProtocolError> {
    if l.fuel == 0 || l.wall_ms == 0 || l.memory_bytes == 0 || l.stack_bytes == 0 {
        return Err(ProtocolError::Invalid("limits must be non-zero"));
    }
    Ok(())
}

/// Verify a signed manifest and return the decoded manifest.
///
/// Order: size cap → issuer trusted → issuer signature over the exact bytes → decode → version → structure →
/// publisher trusted → publisher signature over the module hash → expiry.
pub fn verify_manifest(
    s: &SignedManifest,
    policy: &ManifestPolicy<'_>,
) -> Result<TaskManifest, ProtocolError> {
    if s.manifest.len() > MAX_MANIFEST_BYTES {
        return Err(ProtocolError::TooLarge);
    }
    let issuer: [u8; 32] = s
        .signer_key
        .as_slice()
        .try_into()
        .map_err(|_| ProtocolError::BadKey)?;
    if !policy.trusted_issuers.contains(&issuer) {
        return Err(ProtocolError::UntrustedSigner);
    }
    verify_domain(&issuer, DOMAIN_MANIFEST, &s.manifest, &s.signature)?;
    let m = crate::wire::decode_strict::<TaskManifest>(&s.manifest)
        .map_err(|_| ProtocolError::Decode)?;

    match &m.version {
        Some(v) if v.major == PROTOCOL_VERSION.major => {}
        _ => return Err(ProtocolError::Version),
    }
    if m.task_id.len() != 16 {
        return Err(ProtocolError::Invalid("task_id must be 16 bytes"));
    }
    if m.profile != PROFILE_CPU_CORE_V1 {
        return Err(ProtocolError::Invalid("unknown profile"));
    }
    if m.module_sha256.len() != 32 {
        return Err(ProtocolError::Invalid("module_sha256 must be 32 bytes"));
    }
    if m.inputs.len() > MAX_INPUTS || m.inputs.iter().any(|i| i.sha256.len() != 32) {
        return Err(ProtocolError::Invalid("bad inputs"));
    }
    check_limits(
        m.limits
            .as_ref()
            .ok_or(ProtocolError::Invalid("limits missing"))?,
    )?;
    if m.verifier.is_none() {
        return Err(ProtocolError::Invalid("verifier missing"));
    }
    let publisher: [u8; 32] = m
        .publisher_key
        .as_slice()
        .try_into()
        .map_err(|_| ProtocolError::BadKey)?;
    if !policy.trusted_publishers.contains(&publisher) {
        return Err(ProtocolError::UntrustedSigner);
    }
    verify_module_signature(&publisher, &m.module_sha256, &m.module_signature)?;
    if m.expires_at_unix_ms <= policy.now_unix_ms {
        return Err(ProtocolError::Expired);
    }
    Ok(m)
}
