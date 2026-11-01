//! Module hash and publisher-signature checks, done in the parent before any process is started.

use sha2::{Digest, Sha256};

/// SHA-256 of `bytes`.
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// Publisher signature over the module hash, as carried by the task manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublisherSignature {
    /// Publisher public key (algorithm defined by the verifier, Ed25519 in v1).
    pub public_key: Vec<u8>,
    /// Signature bytes.
    pub signature: Vec<u8>,
}

/// Checks that a module hash was signed by a publisher the device trusts.
///
/// There is deliberately no "accept everything" implementation in this crate: the agent must pass a real
/// verifier (Ed25519 via `nc-identity`), and a job without a valid signature is refused.
pub trait ModuleVerifier: Send + Sync {
    /// Return `Ok(())` only if `sig` is a valid signature by a trusted publisher over `module_sha256`.
    fn verify(&self, module_sha256: &[u8; 32], sig: &PublisherSignature) -> Result<(), String>;
}
