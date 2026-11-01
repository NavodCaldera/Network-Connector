use crate::{
    time::{period, Slot},
    *,
};
use chrono::{DateTime, Utc};
use std::{collections::BTreeMap, sync::Mutex};

/// Unique opaque reservation identifier; never reused on monthly renewal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReservationId(u64);

#[derive(Clone, Debug)]
struct Reservation {
    bytes: u64,
    slot: Option<Slot>,
}

/// Immutable snapshot used by decisions; callers cannot forge or mutate accounting.
#[derive(Clone, Debug)]
pub struct BudgetState {
    pub(crate) profile: TariffProfile,
    pub(crate) start: DateTime<Utc>,
    pub(crate) end: DateTime<Utc>,
    pub(crate) observed: DateTime<Utc>,
    committed: u64,
    next_id: u64,
    reservations: BTreeMap<ReservationId, Reservation>,
    windows: BTreeMap<Slot, u64>,
}

impl BudgetState {
    /// Accounted bytes in this period, including any reported overage.
    pub fn committed_bytes(&self) -> u64 {
        self.committed
    }
    /// Unspent, unreserved period allocation after the safety reserve.
    pub fn available_bytes(&self) -> u64 {
        self.profile
            .spendable_bytes()
            .saturating_sub(self.committed)
            .saturating_sub(
                self.reservations
                    .values()
                    .fold(0u64, |n, r| n.saturating_add(r.bytes)),
            )
    }
    pub(crate) fn window_available(&self, slot: &Slot) -> u64 {
        let reserved = self
            .reservations
            .values()
            .filter(|r| r.slot.as_ref() == Some(slot))
            .fold(0u64, |n, r| n.saturating_add(r.bytes));
        self.profile.windows[slot.index]
            .max_bytes
            .saturating_sub(*self.windows.get(slot).unwrap_or(&0))
            .saturating_sub(reserved)
    }
    pub(crate) fn advance(&mut self, now: DateTime<Utc>) -> Result<(), TariffError> {
        if now < self.observed {
            return Err(TariffError::NotAllowed);
        }
        if now >= self.end {
            let (start, end) = period(&self.profile, now)?;
            // Require settlement before renewal; outstanding bytes must not disappear.
            if !self.reservations.is_empty() {
                return Err(TariffError::NotAllowed);
            }
            self.start = start;
            self.end = end;
            self.committed = 0;
            self.windows.retain(|s, _| s.end > now);
        }
        self.observed = now;
        Ok(())
    }
}

/// Outcome of reconciliation; overages are recorded rather than silently discarded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitReport {
    /// Actual total accounted bytes for this reservation.
    pub actual_bytes: u64,
    /// Amount by which actual bytes exceeded the reservation estimate.
    pub estimate_overage_bytes: u64,
    /// Cumulative period bytes above the spendable allocation.
    pub budget_overage_bytes: u64,
    /// Cumulative occurrence bytes above its window cap.
    pub window_overage_bytes: u64,
    /// Accounting saturated at u64::MAX; further transfers are refused.
    pub saturated: bool,
}

/// In-memory synchronized ledger. Each reserve/settle operation holds one mutex.
///
/// No I/O or persistence is performed. Callers must persist state in their own store
/// before sending bytes and settle active reservations before advancing a period.
#[derive(Debug)]
pub struct BudgetLedger {
    state: Mutex<BudgetState>,
}

impl BudgetLedger {
    /// Initializes a ledger for the local billing period containing `now`.
    pub fn new(profile: &TariffProfile, now: DateTime<Utc>) -> Result<Self, TariffError> {
        profile.validate()?;
        let (start, end) = period(profile, now)?;
        Ok(Self::from_state(BudgetState {
            profile: profile.clone(),
            start,
            end,
            observed: now,
            committed: 0,
            next_id: 0,
            reservations: BTreeMap::new(),
            windows: BTreeMap::new(),
        }))
    }
    pub(crate) fn from_state(state: BudgetState) -> Self {
        Self {
            state: Mutex::new(state),
        }
    }
    /// Returns a coherent decision snapshot, failing closed after mutex poisoning.
    pub fn snapshot(&self) -> Result<BudgetState, TariffError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| TariffError::LedgerUnavailable)?
            .clone())
    }
    /// Advances time and renews the allocation; backward clocks and unsettled renewal fail closed.
    pub fn advance(&self, now: DateTime<Utc>) -> Result<(), TariffError> {
        self.state
            .lock()
            .map_err(|_| TariffError::LedgerUnavailable)?
            .advance(now)
    }
    /// Atomically reserves period bytes only. This is not permission to send traffic.
    /// Use `reserve_transfer` for window/network authorization and occurrence accounting.
    pub fn reserve(&self, bytes: u64) -> Result<ReservationId, TariffError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| TariffError::LedgerUnavailable)?;
        Self::insert(&mut state, bytes, None)
    }
    fn insert(
        state: &mut BudgetState,
        bytes: u64,
        slot: Option<Slot>,
    ) -> Result<ReservationId, TariffError> {
        if bytes > state.available_bytes() {
            return Err(TariffError::InsufficientBudget);
        }
        let next = state.next_id.checked_add(1).ok_or(TariffError::Range)?;
        let id = ReservationId(next);
        state.next_id = next;
        state.reservations.insert(id, Reservation { bytes, slot });
        Ok(id)
    }
    /// Checks policy and reserves atomically, avoiding snapshot-to-reservation races.
    pub fn reserve_transfer(
        &self,
        now: DateTime<Utc>,
        network: &NetworkObservation,
        request: TransferRequest,
    ) -> Result<ReservationId, TariffError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| TariffError::LedgerUnavailable)?;
        state.advance(now)?;
        let profile = state.profile.clone();
        if !matches!(
            decide(&profile, &state, now, network, request),
            Decision::Allow { .. }
        ) {
            return Err(TariffError::NotAllowed);
        }
        let slot = crate::decision::active_slot(&profile, now)?;
        Self::insert(&mut state, request.bytes, slot)
    }
    /// Reconciles exactly once. Actual bytes may exceed the estimate/caps and are reported.
    pub fn commit(
        &self,
        id: ReservationId,
        actual_bytes: u64,
    ) -> Result<CommitReport, TariffError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| TariffError::LedgerUnavailable)?;
        let reservation = state
            .reservations
            .remove(&id)
            .ok_or(TariffError::UnknownReservation)?;
        let (total, saturated) = state.committed.overflowing_add(actual_bytes);
        state.committed = if saturated { u64::MAX } else { total };
        let mut window_overage_bytes = 0;
        if let Some(slot) = reservation.slot {
            let cap = state.profile.windows[slot.index].max_bytes;
            let used = state.windows.entry(slot).or_default();
            *used = used.saturating_add(actual_bytes);
            window_overage_bytes = used.saturating_sub(cap);
        }
        Ok(CommitReport {
            actual_bytes,
            estimate_overage_bytes: actual_bytes.saturating_sub(reservation.bytes),
            budget_overage_bytes: state
                .committed
                .saturating_sub(state.profile.spendable_bytes()),
            window_overage_bytes,
            saturated,
        })
    }
    /// Releases an unused reservation exactly once.
    pub fn release(&self, id: ReservationId) -> Result<(), TariffError> {
        self.state
            .lock()
            .map_err(|_| TariffError::LedgerUnavailable)?
            .reservations
            .remove(&id)
            .ok_or(TariffError::UnknownReservation)?;
        Ok(())
    }
}
