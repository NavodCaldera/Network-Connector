//! Pure owner-policy decisions. No I/O, clock reads or enforcement side effects.
//!
//! The supervisor supplies authenticated peer identities and fresh device snapshots,
//! reserves resources atomically after admission, and acts on Pause/Stop immediately.
//! CPU percentages are best effort; these decisions do not implement CPU throttling
//! or promise the supervisor's one-second Stop. Recheck on every relevant event.
//!
//! ```
//! use nc_policy::OwnerPolicy;
//! let policy = OwnerPolicy::default();
//! assert!(!policy.accept_jobs);
//! policy.validate()?;
//! # Ok::<(), nc_policy::ValidationError>(())
//! ```
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod engine;
mod hours;
mod key;
mod model;

pub use engine::*;
pub use key::*;
pub use model::*;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
