//! `nc-protocol`: Protobuf types (generated from `proto/`), protocol version, and signing/verification of
//! task manifests and module hashes.
//!
//! Every signature is Ed25519 over `domain || bytes`, where `domain` names the message type, so a signature for
//! one kind of object can never be replayed as another. Signatures cover the **exact received bytes**; decoding
//! happens only after the signature has been checked.

pub mod sign;

pub use sign::{
    sign_domain, sign_manifest, sign_module, verify_domain, verify_manifest,
    verify_module_signature, ManifestPolicy, ProtocolError, Signer,
};

/// Generated Protobuf types. Never hand-write a second copy of these (AGENT.md §1).
#[allow(clippy::all, clippy::pedantic, missing_docs)]
pub mod pb {
    /// Package `nc.v1`.
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/nc.v1.rs"));
    }
}

/// Protocol version this build speaks.
pub const PROTOCOL_VERSION: pb::v1::ProtocolVersion =
    pb::v1::ProtocolVersion { major: 1, minor: 0 };

/// The only execution profile in v1.
pub const PROFILE_CPU_CORE_V1: &str = "cpu-core-v1";

mod json_uuid;
/// Strict protobuf decoding and binary-token text transport helpers.
pub mod wire;

/// Real job signature and coordinator caps.
pub mod jobs;
