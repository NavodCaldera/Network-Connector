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
/// There is deliberately no "accept everything" implementation in this crate: the agent passes
/// [`Ed25519Publishers`], and a job without a valid signature is refused.
pub trait ModuleVerifier: Send + Sync {
    /// Return `Ok(())` only if `sig` is a valid signature by a trusted publisher over `module_sha256`.
    fn verify(&self, module_sha256: &[u8; 32], sig: &PublisherSignature) -> Result<(), String>;
}

/// Ed25519 publisher check (`nc_protocol::verify_module_signature`) against a fixed set of trusted keys.
#[derive(Debug, Clone, Default)]
pub struct Ed25519Publishers {
    trusted: Vec<[u8; 32]>,
}

impl Ed25519Publishers {
    /// Trust exactly these publisher keys.
    pub fn new(trusted: impl IntoIterator<Item = [u8; 32]>) -> Self {
        Self {
            trusted: trusted.into_iter().collect(),
        }
    }
}

impl ModuleVerifier for Ed25519Publishers {
    fn verify(&self, module_sha256: &[u8; 32], sig: &PublisherSignature) -> Result<(), String> {
        let key: [u8; 32] = sig
            .public_key
            .as_slice()
            .try_into()
            .map_err(|_| "bad publisher key".to_string())?;
        if !self.trusted.contains(&key) {
            return Err("untrusted publisher".into());
        }
        nc_protocol::verify_module_signature(&key, module_sha256, &sig.signature)
            .map_err(|e| e.to_string())
    }
}
