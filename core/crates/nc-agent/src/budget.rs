//! Network budget gate around nc-tariff: decides whether a transfer may start, reserves its bytes atomically
//! and settles the actual count afterwards (PLAN.md §6).
//!
//! With a usage store ([`BudgetGate::with_usage_store`]) every settled transfer is also written to nc-store,
//! keyed by tariff profile and local day, and a new gate starts with the bytes already used in the current
//! billing period, so a restart does not hand out the budget again. Per-window caps are still counted in
//! memory only (nc-tariff has no way to seed them yet).

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use nc_store::Store;
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
pub struct Grant {
    ledger: Option<Arc<BudgetLedger>>,
    id: Option<ReservationId>,
    /// Usage-ledger key and local day the bytes are charged to.
    usage_key: Option<(String, String)>,
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
    usage: Mutex<Option<Store>>,
}

impl std::fmt::Debug for Grant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Grant")
            .field("until", &self.until)
            .field("bytes", &self.bytes)
            .finish_non_exhaustive()
    }
}

/// Usage-ledger key for a profile (the profile id: a new version of the same tariff keeps its usage).
fn ledger_key(p: &TariffProfile) -> String {
    format!("tariff:{}", p.profile_id)
}

fn day_string(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

fn local_day(p: &TariffProfile, at: DateTime<Utc>) -> NaiveDate {
    at.with_timezone(&p.timezone).date_naive()
}

/// First local day of the billing period containing `today` (reset day clamped to short months, like
/// nc-tariff).
fn period_start_day(p: &TariffProfile, today: NaiveDate) -> Option<NaiveDate> {
    let boundary = |y: i32, m: u32| -> Option<NaiveDate> {
        let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
        let last = NaiveDate::from_ymd_opt(ny, nm, 1)?.pred_opt()?.day();
        NaiveDate::from_ymd_opt(y, m, p.billing.reset_rule.day_of_month.min(last))
    };
    let this = boundary(today.year(), today.month())?;
    if today >= this {
        return Some(this);
    }
    let (py, pm) = if today.month() == 1 {
        (today.year() - 1, 12)
    } else {
        (today.year(), today.month() - 1)
    };
    boundary(py, pm)
}

impl BudgetGate {
    /// Gate for `budget`, with the ledger period containing `now`. Usage is kept in memory only.
    pub fn new(budget: &NetworkBudget, now: DateTime<Utc>) -> Result<Self, TariffError> {
        let g = Self {
            inner: Mutex::new(None),
            profile: Mutex::new(None),
            usage: Mutex::new(None),
        };
        g.update(budget, now)?;
        Ok(g)
    }

    /// Gate whose settled usage is persisted in `store` and restored from it (survives a restart).
    pub fn with_usage_store(
        budget: &NetworkBudget,
        now: DateTime<Utc>,
        store: Store,
    ) -> Result<Self, TariffError> {
        let g = Self {
            inner: Mutex::new(None),
            profile: Mutex::new(None),
            usage: Mutex::new(Some(store)),
        };
        g.update(budget, now)?;
        Ok(g)
    }

    /// Bytes recorded in the usage store for `p`'s billing period up to the local day of `now`.
    fn stored_usage(&self, p: &TariffProfile, now: DateTime<Utc>) -> Result<u64, TariffError> {
        let mut usage = self.usage.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(store) = usage.as_mut() else {
            return Ok(0);
        };
        let today = local_day(p, now);
        let start = period_start_day(p, today).ok_or(TariffError::Range)?;
        // Unreadable usage fails closed: the gate is not built rather than starting from zero.
        store
            .budget_usage(&ledger_key(p), &day_string(start), &day_string(today))
            .map_err(|_| TariffError::LedgerUnavailable)
    }

    /// Replace the budget. Outstanding grants settle against the ledger that issued them.
    pub fn update(&self, budget: &NetworkBudget, now: DateTime<Utc>) -> Result<(), TariffError> {
        let (ledger, profile) = match budget {
            NetworkBudget::Unmetered => (None, None),
            NetworkBudget::Metered(p) => {
                let ledger = BudgetLedger::new(p, now)?;
                let used = self.stored_usage(p, now)?;
                if used > 0 {
                    // Seed the period total with what earlier runs already used.
                    let id = ledger.reserve(0)?;
                    ledger.commit(id, used)?;
                }
                (Some(Arc::new(ledger)), Some(p.clone()))
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
        let (ledger, profile) = self.current();
        let usage_key = profile.map(|p| (ledger_key(&p), day_string(local_day(&p, now))));
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
            usage_key,
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
        if let Some((key, day)) = grant.usage_key {
            if let Some(store) = self
                .usage
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_mut()
            {
                if store.add_budget_usage(&key, &day, actual).is_err() {
                    // The in-memory ledger still counts it; only a restart could forget these bytes.
                    tracing::warn!("network usage not persisted");
                }
            }
        }
    }
}
