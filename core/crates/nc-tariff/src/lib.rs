//! Pure tariff decisions, synchronized byte accounting and deterministic planning.
//!
//! Profiles are owner configuration, never ISP facts. Callers enforce consent,
//! power/thermal limits and Stop separately, and recheck before every bounded chunk.
//! Throughput is an estimate, not a guarantee or the ISP's billing counter.
//!
//! ```
//! use nc_tariff::{sim, BudgetLedger, NetworkObservation, TrafficClass, TransferRequest};
//! let profile = sim::example_profile()?; // Synthetic configuration, not an ISP offer.
//! let now = "2026-01-05T18:00:00Z".parse()?;
//! let ledger = BudgetLedger::new(&profile, now)?;
//! let network = NetworkObservation {
//!     interface: profile.binding.interface,
//!     connection_id: Some(profile.binding.connection_id.clone()),
//!     bytes_per_second: 1_000,
//! };
//! let id = ledger.reserve_transfer(now, &network,
//!     TransferRequest { class: TrafficClass::Bulk, bytes: 1_000 })?;
//! let report = ledger.commit(id, 1_020)?; // Include retransmissions/relay overhead.
//! assert_eq!(report.estimate_overage_bytes, 20);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod budget;
mod decision;
mod model;
mod planner;
/// Deterministic synthetic night simulations; no real ISP or device measurements.
pub mod sim;
mod time;

pub use budget::*;
pub use decision::*;
pub use model::*;
pub use planner::*;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
