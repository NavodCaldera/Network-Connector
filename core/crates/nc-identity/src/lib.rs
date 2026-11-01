//! `nc-identity`: device root key, short-lived session certificates, keystore adapters (PLAN.md §5 "Identity").
//!
//! - [`DeviceKeyStore`] is the adapter trait. Hardware adapters (TPM via CNG, Android Keystore/StrongBox,
//!   Secure Enclave) implement it later and never export the key.
//! - [`SoftwareKeyStore`] is the current implementation: an Ed25519 seed in a protected file (DPAPI, user scope,
//!   on Windows; mode 0600 elsewhere). It reports [`Assurance::Software`] so peers can treat it as lower assurance.
//! - [`issue_session`] creates an ephemeral session key and a [`SignedSessionCert`] signed by the root;
//!   [`verify_session_cert`] checks one.
//!
//! Key material is zeroised on drop and never logged or put in error messages.

#![deny(clippy::undocumented_unsafe_blocks)]

mod keystore;
mod platform;
mod session;

pub use keystore::{Assurance, DeviceKeyStore, IdentityError, RootSigner, SoftwareKeyStore};
pub use nc_protocol::pb::v1::{SessionCert, SignedSessionCert};
pub use session::{issue_session, verify_session_cert, SessionKey, MAX_SESSION_TTL_MS};
