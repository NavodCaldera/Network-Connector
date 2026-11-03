//! Network budget gate around nc-tariff: decides whether a transfer may start, reserves its bytes atomically
//! and settles the actual count afterwards (PLAN.md §6).
//!
//! With a usage store ([`BudgetGate::with_usage_store`]) every settled transfer is also written to nc-store,
//! keyed by tariff profile and local day, and a new gate starts with the bytes already used in the current
//! billing period, so a restart does not hand out the budget again. Bytes moved inside a tariff window are also
//! written under a per-occurrence key, and the window active at start-up is seeded from it
//! ([`nc_tariff::BudgetLedger::seed_usage`]), so per-window caps survive a restart too.

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use nc_store::Store;
use nc_tariff::{
    active_window, decide, BudgetLedger, Decision, DenyReason, NetworkObservation, ReservationId,
    TariffError, TariffProfile, TrafficClass, TransferRequest, WindowOccurrence,
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
    /// Usage key of the window occurrence the bytes are also charged to.
    window_key: Option<String>,
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

/// Usage-ledger key for one window occurrence: hashed profile id (keeps the key short), window index and
/// occurrence start.
fn window_key(p: &TariffProfile, occ: &WindowOccurrence) -> String {
    use sha2::Digest;
    let h = hex::encode(sha2::Sha256::digest(p.profile_id.as_bytes()));
    format!(
        "tariff-window:{}:{}:{}",
        &h[..32],
        occ.index(),
        occ.start().timestamp()
    )
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

    /// Bytes recorded for the window occurrence active at `now` (empty outside windows or without a store).
    fn stored_window_usage(
        &self,
        p: &TariffProfile,
        now: DateTime<Utc>,
    ) -> Result<Vec<(WindowOccurrence, u64)>, TariffError> {
        let mut usage = self.usage.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(store) = usage.as_mut() else {
            return Ok(Vec::new());
        };
        let Some(occ) = active_window(p, now)? else {
            return Ok(Vec::new());
        };
        let from = local_day(p, occ.start());
        let bytes = store
            .budget_usage(
                &window_key(p, &occ),
                &day_string(from),
                &day_string(local_day(p, now)),
            )
            .map_err(|_| TariffError::LedgerUnavailable)?;
        Ok(if bytes > 0 {
            vec![(occ, bytes)]
        } else {
            Vec::new()
        })
    }

    /// Replace the budget. Outstanding grants settle against the ledger that issued them.
    pub fn update(&self, budget: &NetworkBudget, now: DateTime<Utc>) -> Result<(), TariffError> {
        let (ledger, profile) = match budget {
            NetworkBudget::Unmetered => (None, None),
            NetworkBudget::Metered(p) => {
                let ledger = BudgetLedger::new(p, now)?;
                let used = self.stored_usage(p, now)?;
                let windows = self.stored_window_usage(p, now)?;
                // Seed the period total and the active window with what earlier runs already used.
                ledger.seed_usage(used, &windows)?;
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
        let usage_key = profile
            .as_ref()
            .map(|p| (ledger_key(p), day_string(local_day(p, now))));
        // The same occurrence reserve_transfer charges (same profile, same `now`).
        let window_key = match &profile {
            Some(p) => active_window(p, now)
                .map_err(|_| Refusal::Window)?
                .map(|o| window_key(p, &o)),
            None => None,
        };
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
            window_key,
            until,
            bytes,
            network: net.clone(),
        })
    }

    /// Authorize and settle `bytes` at once (bytes about to be sent in one piece). `false`: refused.
    pub fn charge(
        &self,
        class: TrafficClass,
        bytes: u64,
        now: DateTime<Utc>,
        net: &NetworkObservation,
    ) -> bool {
        match self.authorize(class, bytes, now, net) {
            Ok(g) => {
                self.settle(g, bytes);
                true
            }
            Err(_) => false,
        }
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
                if let Some(wk) = &grant.window_key {
                    if store.add_budget_usage(wk, &day, actual).is_err() {
                        tracing::warn!("window usage not persisted");
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn setup() -> (NetworkBudget, NetworkObservation) {
        let p = nc_tariff::sim::example_profile().unwrap();
        let net = NetworkObservation {
            interface: p.binding.interface,
            connection_id: Some(p.binding.connection_id.clone()),
            bytes_per_second: 1_000_000,
        };
        (NetworkBudget::Metered(p), net)
    }

    fn gate(budget: &NetworkBudget, now: DateTime<Utc>, db: &std::path::Path) -> BudgetGate {
        BudgetGate::with_usage_store(budget, now, Store::open(db).unwrap()).unwrap()
    }

    #[test]
    fn window_cap_survives_restart_across_midnight() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("budget.sqlite");
        let (budget, net) = setup();
        // 23:00 local (Colombo): use 30 MB of the 50 MB window cap.
        let t1 = at("2026-01-05T17:30:00Z");
        let g = gate(&budget, t1, &db);
        let grant = g
            .authorize(TrafficClass::Bulk, 30_000_000, t1, &net)
            .unwrap();
        g.settle(grant, 30_000_000);
        drop(g);
        // Restart at 02:00 local the next day: same window occurrence, 20 MB left.
        let t2 = at("2026-01-05T20:30:00Z");
        let g = gate(&budget, t2, &db);
        assert!(g
            .authorize(TrafficClass::Bulk, 20_000_001, t2, &net)
            .is_err());
        let grant = g
            .authorize(TrafficClass::Bulk, 15_000_000, t2, &net)
            .unwrap();
        g.settle(grant, 15_000_000);
        drop(g);
        // Second restart in the same occurrence sees both runs (45 MB used).
        let g = gate(&budget, t2, &db);
        assert!(g
            .authorize(TrafficClass::Bulk, 5_000_001, t2, &net)
            .is_err());
        assert!(g.check(TrafficClass::Bulk, 5_000_000, t2, &net).is_ok());
        // Period total also kept: 450 MB spendable - 45 MB.
        assert_eq!(g.available_bytes(), Some(405_000_000));
        // A memory-only gate would hand the window out again (the old behaviour).
        let mem = BudgetGate::new(&budget, t2).unwrap();
        assert!(mem.check(TrafficClass::Bulk, 40_000_000, t2, &net).is_ok());
        drop(g);
        // The next night is a new occurrence with the full cap.
        let t3 = at("2026-01-06T17:00:00Z");
        let g = gate(&budget, t3, &db);
        assert!(g.check(TrafficClass::Bulk, 50_000_000, t3, &net).is_ok());
        assert!(g.check(TrafficClass::Bulk, 50_000_001, t3, &net).is_err());
    }

    #[test]
    fn restart_outside_window_and_on_cellular_still_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("budget.sqlite");
        let (budget, net) = setup();
        let noon = at("2026-01-06T06:30:00Z");
        let g = gate(&budget, noon, &db);
        assert!(g.check(TrafficClass::Bulk, 1, noon, &net).is_err());
        let night = at("2026-01-06T17:00:00Z");
        let mut cell = net.clone();
        cell.interface = nc_tariff::InterfaceClass::Cellular;
        assert!(g.check(TrafficClass::Bulk, 1, night, &cell).is_err());
        assert!(g.check(TrafficClass::Bulk, 1, night, &net).is_ok());
    }
}
