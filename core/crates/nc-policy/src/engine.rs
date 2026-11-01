use crate::{hours, DeviceState, OwnerPolicy, TaskOffer, ThermalLevel, ValidationError};
use chrono::{DateTime, Duration, Utc};
use nc_tariff::Decision;

/// Stable reason with plain English Display text, without peer IDs or task data.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Reason {
    /// Invalid owner configuration; never silently repair it while admitting work.
    #[error("The owner policy is invalid: {0}")]
    InvalidPolicy(ValidationError),
    /// Owner consent has not been given or was withdrawn.
    #[error("The owner has disabled helper work.")]
    PolicyWithdrawn,
    /// Explicit block overrides any trust entry.
    #[error("The task's peer is blocked by the owner.")]
    BlockedPeer,
    /// A key is missing from the owner's explicit trust list.
    #[error("The task's peer is not trusted by the owner.")]
    UntrustedPeer,
    /// Requested or running execution profile is not explicitly permitted.
    #[error("This execution profile is not allowed by the owner.")]
    ExecutionProfileNotAllowed,
    /// No room for another task, or a revised policy allows fewer current tasks.
    #[error("The maximum number of simultaneous tasks has been reached.")]
    ConcurrencyLimit,
    /// Memory cap would be exceeded or is already exceeded.
    #[error("The task memory limit would be exceeded.")]
    MemoryLimit,
    /// Disk cap would be exceeded or is already exceeded.
    #[error("The task disk limit would be exceeded.")]
    DiskLimit,
    /// Offer estimate is zero, so no meaningful duration was supplied.
    #[error("The task must provide a positive estimated duration.")]
    InvalidTaskDuration,
    /// Estimate exceeds the cap, or a running task has reached its deadline.
    #[error("The task has reached or would exceed its maximum duration.")]
    TaskDurationLimit,
    /// Future task start or unrepresentable deadline cannot be safely evaluated.
    #[error("The task's start time or deadline is inconsistent with the current clock.")]
    TaskClockInvalid,
    /// Charging-only work needs external power.
    #[error("Helper work requires external power.")]
    ChargingRequired,
    /// Battery observation required by a positive minimum is unavailable.
    #[error("The battery level is unknown.")]
    BatteryUnknown,
    /// An adapter supplied an invalid percentage.
    #[error("The reported battery percentage is invalid.")]
    InvalidBatteryReading,
    /// Known battery level is below the inclusive minimum.
    #[error("The battery level is below the owner's minimum.")]
    BatteryTooLow,
    /// Thermal checks are always required and the reading is unavailable.
    #[error("The device temperature level is unknown.")]
    ThermalUnknown,
    /// Noncritical temperature reached the configured inclusive threshold.
    #[error("The device temperature has reached the owner's pause threshold.")]
    ThermalThreshold,
    /// Critical temperature is always an immediate hard stop.
    #[error("The device temperature is critical; stop helper work.")]
    CriticalThermal,
    /// User activity must be known when the owner requires idle time.
    #[error("The time since the user's last activity is unknown.")]
    UserActivityUnknown,
    /// Known idle time is shorter than the configured minimum.
    #[error("Helper work is paused while the user is active.")]
    UserActive,
    /// No configured working interval contains the current instant.
    #[error("The current time is outside the owner's working hours.")]
    OutsideHours,
    /// Calendar range or shared calendar failure; no schedule guess is made.
    #[error("Working hours cannot be evaluated safely for this time.")]
    HoursUnavailable,
    /// Real tariff refused a necessary transfer.
    #[error("The network tariff does not allow the required transfer.")]
    TariffDenied,
    /// Real tariff postponed a necessary transfer.
    #[error("The required transfer must wait for the network tariff.")]
    TariffDeferred,
    /// A previously allowed transfer reached its exclusive deadline.
    #[error("The network tariff's transfer permission has expired.")]
    TariffExpired,
    /// An Allow snapshot grants no bytes for a required transfer.
    #[error("The network tariff has no available bytes for this transfer.")]
    TariffCapacityUnavailable,
}

/// New-task decision. Accept is not an atomic resource reservation or a lease.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Admission {
    /// Every evaluated rule currently permits starting this offer.
    Accept,
    /// Device policy refuses the offer regardless of coordinator permission.
    Reject {
        /// Owner-readable explanation of the highest-priority refusal.
        reason: Reason,
    },
}

/// Whole-device decision over every task in the supplied snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunningDecision {
    /// All supplied tasks may continue under the current observations.
    Continue,
    /// Suspend work until policy/device conditions permit it again.
    Pause {
        /// Temporary restriction; all Pause reasons still reject new work.
        reason: Reason,
    },
    /// Revoke work immediately; the supervisor owns cancellation and sandbox teardown.
    Stop {
        /// Hard revocation, critical temperature or expired/invalid task time.
        reason: Reason,
    },
}

fn deadline(policy: &OwnerPolicy, task: &crate::RunningTask) -> Option<DateTime<Utc>> {
    task.started_at
        .checked_add_signed(Duration::from_std(policy.max_task_duration).ok()?)
}

// Hard stops are checked before temporary conditions so battery/hours cannot mask revocation.
fn hard_stop(policy: &OwnerPolicy, state: &DeviceState) -> Option<Reason> {
    if let Err(error) = policy.validate() {
        return Some(Reason::InvalidPolicy(error));
    }
    if !policy.accept_jobs {
        return Some(Reason::PolicyWithdrawn);
    }
    if state
        .running_tasks
        .iter()
        .any(|t| policy.blocked_peers.contains(&t.peer_key))
    {
        return Some(Reason::BlockedPeer);
    }
    if state.thermal_level == Some(ThermalLevel::Critical) {
        return Some(Reason::CriticalThermal);
    }
    for task in &state.running_tasks {
        if !policy.trusted_peers.contains(&task.peer_key) {
            return Some(Reason::UntrustedPeer);
        }
        if !policy.allowed_execution_profiles.contains(&task.profile) {
            return Some(Reason::ExecutionProfileNotAllowed);
        }
        if task.started_at > state.now {
            return Some(Reason::TaskClockInvalid);
        }
        let Some(end) = deadline(policy, task) else {
            return Some(Reason::TaskClockInvalid);
        };
        if state.now >= end {
            return Some(Reason::TaskDurationLimit);
        }
    }
    None
}

fn temporary_restriction(policy: &OwnerPolicy, state: &DeviceState) -> Option<Reason> {
    if state.running_tasks.len() > policy.max_concurrent_tasks as usize {
        return Some(Reason::ConcurrencyLimit);
    }
    if state.memory_in_use_mb > policy.max_memory_mb {
        return Some(Reason::MemoryLimit);
    }
    if state.disk_in_use_mb > policy.max_disk_mb {
        return Some(Reason::DiskLimit);
    }
    if policy.charging_only && !state.on_ac_power {
        return Some(Reason::ChargingRequired);
    }
    match state.battery_percent {
        Some(level) if level > 100 => return Some(Reason::InvalidBatteryReading),
        Some(level) if level < policy.min_battery_percent => return Some(Reason::BatteryTooLow),
        None if policy.min_battery_percent > 0 => return Some(Reason::BatteryUnknown),
        _ => {}
    }
    match state.thermal_level {
        None => return Some(Reason::ThermalUnknown),
        Some(level) if level >= policy.thermal_stop => return Some(Reason::ThermalThreshold),
        _ => {}
    }
    if policy.pause_on_user_activity {
        match state.user_idle_seconds {
            None => return Some(Reason::UserActivityUnknown),
            Some(seconds) if seconds < u64::from(policy.idle_minutes) * 60 => {
                return Some(Reason::UserActive)
            }
            _ => {}
        }
    }
    match hours::decision(policy, state.now) {
        Ok(Decision::Allow { .. }) => {}
        Ok(Decision::Defer { .. }) => return Some(Reason::OutsideHours),
        _ => return Some(Reason::HoursUnavailable),
    }
    match &state.tariff_decision {
        Some(Decision::Deny { .. }) => Some(Reason::TariffDenied),
        Some(Decision::Defer { .. }) => Some(Reason::TariffDeferred),
        Some(Decision::Allow { until, .. }) if *until <= state.now => Some(Reason::TariffExpired),
        Some(Decision::Allow { max_bytes: 0, .. }) => Some(Reason::TariffCapacityUnavailable),
        _ => None,
    }
}

/// Checks whether this device may admit a complete offer right now.
///
/// A blocked key always beats a trusted entry. Missing required sensors reject.
/// Exact memory/disk/battery boundaries are permitted; concurrency includes existing
/// tasks and this new task. Duration estimates are capped but cannot replace watchdogs.
pub fn admit(policy: &OwnerPolicy, state: &DeviceState, offer: &TaskOffer) -> Admission {
    let reject = |reason| Admission::Reject { reason };
    if let Some(reason) = hard_stop(policy, state) {
        return reject(reason);
    }
    if policy.blocked_peers.contains(&offer.peer_key) {
        return reject(Reason::BlockedPeer);
    }
    if !policy.trusted_peers.contains(&offer.peer_key) {
        return reject(Reason::UntrustedPeer);
    }
    if !policy.allowed_execution_profiles.contains(&offer.profile) {
        return reject(Reason::ExecutionProfileNotAllowed);
    }
    if state.running_tasks.len() >= policy.max_concurrent_tasks as usize {
        return reject(Reason::ConcurrencyLimit);
    }
    if state
        .memory_in_use_mb
        .checked_add(offer.est_memory_mb)
        .is_none_or(|n| n > policy.max_memory_mb)
    {
        return reject(Reason::MemoryLimit);
    }
    if state
        .disk_in_use_mb
        .checked_add(offer.est_disk_mb)
        .is_none_or(|n| n > policy.max_disk_mb)
    {
        return reject(Reason::DiskLimit);
    }
    if offer.est_duration.is_zero() {
        return reject(Reason::InvalidTaskDuration);
    }
    if offer.est_duration > policy.max_task_duration {
        return reject(Reason::TaskDurationLimit);
    }
    if let Some(reason) = temporary_restriction(policy, state) {
        return reject(reason);
    }
    Admission::Accept
}

/// Evaluates all supplied running tasks. Hard Stop rules take precedence over Pause.
///
/// Unknown thermal always pauses; unknown battery pauses only when the minimum is
/// positive. An empty task list is evaluated as device readiness, not implicit consent.
/// The caller must include every active/paused task and recheck on policy revision.
pub fn check_running(policy: &OwnerPolicy, state: &DeviceState) -> RunningDecision {
    if let Some(reason) = hard_stop(policy, state) {
        return RunningDecision::Stop { reason };
    }
    if let Some(reason) = temporary_restriction(policy, state) {
        return RunningDecision::Pause { reason };
    }
    RunningDecision::Continue
}

/// Earliest known future reevaluation time from hours, idle time, task deadlines or tariff.
///
/// Assumes no new user activity and unchanged observations. External events (power,
/// thermal, peer/policy changes, network and clock jumps) require immediate rechecks.
/// None means no predictable timed change, not permission to keep running forever.
/// The shared calendar may return an earlier conservative monthly reevaluation point.
pub fn next_change(policy: &OwnerPolicy, state: &DeviceState) -> Option<DateTime<Utc>> {
    if hard_stop(policy, state).is_some() {
        return None;
    }
    let mut candidates = Vec::new();
    match hours::decision(policy, state.now).ok()? {
        Decision::Allow { until, .. } => candidates.push(until),
        Decision::Defer {
            next_open: Some(next),
        } => candidates.push(next),
        _ => {}
    }
    if policy.pause_on_user_activity {
        if let Some(idle) = state.user_idle_seconds {
            let threshold = u64::from(policy.idle_minutes) * 60;
            if idle < threshold {
                let remaining = i64::try_from(threshold - idle).ok()?;
                if let Some(next) = state.now.checked_add_signed(Duration::seconds(remaining)) {
                    candidates.push(next);
                }
            }
        }
    }
    for task in &state.running_tasks {
        if let Some(end) = deadline(policy, task) {
            candidates.push(end);
        }
    }
    match &state.tariff_decision {
        Some(Decision::Allow { until, .. }) => candidates.push(*until),
        Some(Decision::Defer {
            next_open: Some(next),
        }) => candidates.push(*next),
        _ => {}
    }
    candidates
        .into_iter()
        .filter(|time| *time > state.now)
        .min()
}
