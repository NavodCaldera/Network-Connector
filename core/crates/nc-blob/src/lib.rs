//! Synchronous encrypted, resumable local blob storage.
//!
//! The caller owns keys, authenticates manifests and enforces policy/tariff limits.
//! This local manifest is not a replacement for the protocol's wire messages.
//! See the crate README for storage format, locking and durability boundaries.
#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod model;
mod store;
pub use model::*;
pub use store::*;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
