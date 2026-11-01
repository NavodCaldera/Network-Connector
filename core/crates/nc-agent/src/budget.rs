//! Network budget gate around nc-tariff: decides whether a transfer may start, reserves its bytes atomically
//! and settles the actual count afterwards (PLAN.md §6).

use chrono::{DateTime, Utc};
use nc_tariff::{
    decide, BudgetLedger, Decision, DenyReason, NetworkObservation, ReservationId, TariffError,
    TariffProfile, TrafficClass, TransferRequest,
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, PoisonError};

/// What the owner said about this connection's cost.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkBudget {
    /// The owner confirmed the connection is unmetered: no window or byte limits.
    Unmetered,
    /// Metered: windows and byte budgets from this profile apply.
    Metered(TariffProfile),
}

/// Why the gate refused a transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Outside the allowed window, wrong network, or the window ends before the transfer can finish.
    Window,
    /// Not enough bytes left in the period or window.
    Budget,
}

/// Permission for one transfer.
#[derive(Debug)]
pub struct Grant {
    ledger: Option<Arc<BudgetLedger>>,
    id: Option<ReservationId>,
    /// Exclusive deadline; the transfer must stop at it. `None` for unmetered.
    pub until: Option<DateTime<Utc>>,
    /// Bytes reserved (the transfer must not exceed it).
    pub bytes: u64,
    /// Network the grant was issued for; any change stops the transfer.
    pub network: NetworkObservation,
}

/// Thread-safe gate.
#[derive(Debug)]
pub struct BudgetGate {
    inner: Mutex<Option<Arc<BudgetLedger>>>,
    profile: Mutex<Option<TariffProfile>>,
}

impl BudgetGate {
    /// Gate for `budget`, with the ledger period containing `now`.
    pub fn new(budget: &NetworkBudget, now: DateTime<Utc>) -> Result<Self, TariffError> {
        let g = Self {
            inner: Mutex::new(None),
            profile: Mutex::new(None),
        };
        g.update(budget, now)?;
        Ok(g)
    }

    /// Replace the budget. Outstanding grants settle against the ledger that issued them.
    pub fn update(&self, budget: &NetworkBudget, now: DateTime<Utc>) -> Result<(), TariffError> {
        let (ledger, profile) = match budget {
            NetworkBudget::Unmetered => (None, None),
            NetworkBudget::Metered(p) => {
                (Some(Arc::new(BudgetLedger::new(p, now)?)), Some(p.clone()))
            }
        };
        *self.inner.lock().unwrap_or_else(PoisonError::into_inner) = ledger;
        *self.profile.lock().unwrap_or_else(PoisonError::into_inner) = profile;
        Ok(())
    }

    fn current(&self) -> (Option<Arc<BudgetLedger>>, Option<TariffProfile>) {
        (
            self.inner
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
            self.profile
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
        )
    }

    /// Bytes left in the period after the safety reserve; `None` when unmetered.
    pub fn available_bytes(&self) -> Option<u64> {
        let (ledger, _) = self.current();
        ledger.and_then(|l| l.snapshot().ok().map(|s| s.available_bytes()))
    }

    /// Would a transfer of `bytes` in `class` be allowed now? Does not reserve.
    pub fn check(
        &self,
        class: TrafficClass,
        bytes: u64,
        now: DateTime<Utc>,
        net: &NetworkObservation,
    ) -> Result<Option<DateTime<Utc>>, Refusal> {
        let (Some(ledger), Some(profile)) = self.current() else {
            return Ok(None);
        };
        let state = ledger.snapshot().map_err(|_| Refusal::Window)?;
        match decide(&profile, &state, now, net, TransferRequest { class, bytes }) {
            Decision::Allow { until, .. } => Ok(Some(until)),
            Decision::Defer { .. } => {
                if state.available_bytes() < bytes {
                    Err(Refusal::Budget)
                } else {
                    Err(Refusal::Window)
                }
            }
            Decision::Deny {
                reason: DenyReason::UnsettledPeriod,
            } => Err(Refusal::Budget),
            Decision::Deny { .. } => Err(Refusal::Window),
        }
    }

    /// Check and reserve `bytes` atomically.
    pub fn authorize(
        &self,
        class: TrafficClass,
        bytes: u64,
        now: DateTime<Utc>,
        net: &NetworkObservation,
    ) -> Result<Grant, Refusal> {
        let until = self.check(class, bytes, now, net)?;
        let (ledger, _) = self.current();
        let id = match &ledger {
            None => None,
            Some(l) => Some(
                l.reserve_transfer(now, net, TransferRequest { class, bytes })
                    .map_err(|e| match e {
                        TariffError::InsufficientBudget => Refusal::Budget,
                        _ => Refusal::Window,
                    })?,
            ),
        };
        Ok(Grant {
            ledger,
            id,
            until,
            bytes,
            network: net.clone(),
        })
    }

    /// Settle a grant with the bytes actually moved (overage is recorded by nc-tariff, not dropped).
    pub fn settle(&self, grant: Grant, actual: u64) {
        if let (Some(l), Some(id)) = (grant.ledger, grant.id) {
            // A failed commit means the reservation was already settled; nothing else to do.
            let _ = l.commit(id, actual);
        }
    }
}
