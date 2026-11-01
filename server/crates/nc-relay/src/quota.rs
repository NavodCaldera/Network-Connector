//! Who may use the relay, and how much: grants from verified tokens, per-device (peer) and per-account
//! limits on reservations, circuits and bytes.
//!
//! Byte quota: libp2p's relay caps every circuit at `max_circuit_bytes` but does not report bytes used, so
//! each accepted circuit is charged its full cap up front. This is conservative (the quota can run out
//! before that many bytes really moved) and never lets a peer exceed its byte budget.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use libp2p::PeerId;

/// Quota settings.
#[derive(Clone, Debug)]
pub struct Limits {
    /// Reservation requests per peer per window.
    pub reservations_per_peer: u32,
    /// Reservation requests per account per window.
    pub reservations_per_account: u32,
    /// Circuit bytes per peer per window (charged per circuit at `circuit_bytes`).
    pub bytes_per_peer: u64,
    /// Circuit bytes per account per window.
    pub bytes_per_account: u64,
    /// Cap of one circuit (same value as the relay config's `max_circuit_bytes`).
    pub circuit_bytes: u64,
    /// Accounting window.
    pub window: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            reservations_per_peer: 30,
            reservations_per_account: 120,
            bytes_per_peer: 2 * 1024 * 1024 * 1024,
            bytes_per_account: 8 * 1024 * 1024 * 1024,
            circuit_bytes: 128 * 1024 * 1024,
            window: Duration::from_secs(24 * 3600),
        }
    }
}

#[derive(Clone, Debug)]
struct Grant {
    account: String,
    expires: Instant,
}

#[derive(Default, Debug, Clone, Copy)]
struct Usage {
    reservations: u32,
    bytes: u64,
}

#[derive(Debug)]
struct Inner {
    grants: HashMap<PeerId, Grant>,
    peer_use: HashMap<PeerId, Usage>,
    account_use: HashMap<String, Usage>,
    window_start: Instant,
}

/// Shared quota state. Cheap to clone.
#[derive(Clone, Debug)]
pub struct Quotas {
    limits: Limits,
    inner: Arc<Mutex<Inner>>,
}

/// Why a request was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum Deny {
    /// No valid token presented (or it expired).
    NoGrant,
    /// Over a quota.
    OverQuota,
}

impl Quotas {
    /// New state with `limits`.
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            inner: Arc::new(Mutex::new(Inner {
                grants: HashMap::new(),
                peer_use: HashMap::new(),
                account_use: HashMap::new(),
                window_start: Instant::now(),
            })),
        }
    }

    /// Limits in force.
    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    /// Records a verified token for `peer`, valid for `ttl`.
    pub fn grant(&self, peer: PeerId, account: String, ttl: Duration, now: Instant) {
        if let Ok(mut g) = self.inner.lock() {
            g.grants.insert(
                peer,
                Grant {
                    account,
                    expires: now + ttl,
                },
            );
        }
    }

    fn with<T>(&self, now: Instant, f: impl FnOnce(&mut Inner, &Limits) -> T) -> Option<T> {
        let mut g = self.inner.lock().ok()?;
        if now.duration_since(g.window_start) >= self.limits.window {
            g.peer_use.clear();
            g.account_use.clear();
            g.window_start = now;
        }
        g.grants.retain(|_, gr| gr.expires > now);
        Some(f(&mut g, &self.limits))
    }

    /// Reservation check (destination side). Counts the request if allowed.
    pub fn try_reserve(&self, peer: PeerId, now: Instant) -> Result<(), Deny> {
        self.with(now, |g, l| {
            let account = g.grants.get(&peer).ok_or(Deny::NoGrant)?.account.clone();
            let pu = g.peer_use.get(&peer).copied().unwrap_or_default();
            let au = g.account_use.get(&account).copied().unwrap_or_default();
            if pu.reservations >= l.reservations_per_peer
                || au.reservations >= l.reservations_per_account
            {
                return Err(Deny::OverQuota);
            }
            g.peer_use.entry(peer).or_default().reservations += 1;
            g.account_use.entry(account).or_default().reservations += 1;
            Ok(())
        })
        .unwrap_or(Err(Deny::NoGrant))
    }

    /// Circuit check (source side). Charges one circuit's byte cap if allowed.
    pub fn try_circuit(&self, peer: PeerId, now: Instant) -> Result<(), Deny> {
        self.with(now, |g, l| {
            let account = g.grants.get(&peer).ok_or(Deny::NoGrant)?.account.clone();
            let pu = g.peer_use.get(&peer).copied().unwrap_or_default();
            let au = g.account_use.get(&account).copied().unwrap_or_default();
            if pu.bytes + l.circuit_bytes > l.bytes_per_peer
                || au.bytes + l.circuit_bytes > l.bytes_per_account
            {
                return Err(Deny::OverQuota);
            }
            g.peer_use.entry(peer).or_default().bytes += l.circuit_bytes;
            g.account_use.entry(account).or_default().bytes += l.circuit_bytes;
            Ok(())
        })
        .unwrap_or(Err(Deny::NoGrant))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn no_grant_no_service() {
        let q = Quotas::new(Limits::default());
        let p = PeerId::random();
        let now = Instant::now();
        assert_eq!(q.try_reserve(p, now), Err(Deny::NoGrant));
        assert_eq!(q.try_circuit(p, now), Err(Deny::NoGrant));
    }

    #[test]
    fn grants_expire() {
        let q = Quotas::new(Limits::default());
        let p = PeerId::random();
        let now = Instant::now();
        q.grant(p, "a".into(), Duration::from_secs(10), now);
        assert!(q.try_reserve(p, now).is_ok());
        assert_eq!(
            q.try_reserve(p, now + Duration::from_secs(11)),
            Err(Deny::NoGrant)
        );
    }

    #[test]
    fn per_peer_and_per_account_limits() {
        let l = Limits {
            reservations_per_peer: 2,
            reservations_per_account: 3,
            bytes_per_peer: 300,
            bytes_per_account: 400,
            circuit_bytes: 100,
            window: Duration::from_secs(60),
        };
        let q = Quotas::new(l);
        let now = Instant::now();
        let (p1, p2) = (PeerId::random(), PeerId::random());
        q.grant(p1, "acct".into(), Duration::from_secs(600), now);
        q.grant(p2, "acct".into(), Duration::from_secs(600), now);
        assert!(q.try_reserve(p1, now).is_ok());
        assert!(q.try_reserve(p1, now).is_ok());
        assert_eq!(q.try_reserve(p1, now), Err(Deny::OverQuota)); // per peer
        assert!(q.try_reserve(p2, now).is_ok());
        assert_eq!(q.try_reserve(p2, now), Err(Deny::OverQuota)); // per account (3)

        for _ in 0..3 {
            assert!(q.try_circuit(p1, now).is_ok());
        }
        assert_eq!(q.try_circuit(p1, now), Err(Deny::OverQuota)); // 300 per peer
        assert!(q.try_circuit(p2, now).is_ok());
        assert_eq!(q.try_circuit(p2, now), Err(Deny::OverQuota)); // 400 per account

        // New window resets usage.
        assert!(q.try_circuit(p2, now + Duration::from_secs(61)).is_ok());
    }
}
