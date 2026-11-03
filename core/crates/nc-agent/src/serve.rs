//! Requester side of direct transfers (1.L15b): grant the helpers the coordinator lists, and nobody else.
//!
//! While a submitted job is queued or leased, [`serve_job`] polls the signed `GET /v1/jobs/{id}/leases`
//! (every [`POLL`], backing off on errors; each call charged as control traffic). For each live lease that
//! carries a `helper_peer_id` it grants that PeerId exactly this job's blobs ([`PeerNet::grant_raw`]); a lease
//! without one grants nothing. A grant is withdrawn when its lease leaves the list, when its `expires_at`
//! passes, and when the job ends, the wait ends or Stop is requested ([`JobGrants`] also revokes on drop).
//!
//! The PeerId is only a connection detail: the nc-net gate authenticates the remote key, and the blob server
//! checks the grant again per request. Served bytes go through the node's [`crate::peer::ServeGate`], which
//! production wiring connects to the requester's own budget gate (status 3 when refused).
//!
//! Nothing here logs PeerIds, job ids, hashes or bytes (AGENT.md §1 rule 10).

use crate::coord::{CoordError, Coordinator};
use crate::peer::PeerNet;
use nc_net::PeerId;
use nc_protocol::pb::v1::JobLeasesResponse;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Poll interval while the job waits. Short, because a helper only keeps re-dialling for 10 s after its lease.
pub const POLL: Duration = Duration::from_secs(2);
/// Longest backoff after coordinator errors or a closed tariff window.
pub const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// With no live lease, the job status is checked every this many polls to notice the end of the job.
const STATUS_EVERY: u32 = 5;
/// Largest number of helpers granted at once (the server lists at most 16 leases).
const MAX_GRANTS: usize = 16;

/// This job's blobs and the helpers currently granted them.
pub struct JobGrants<'a> {
    net: &'a PeerNet,
    set: HashMap<[u8; 32], Arc<Vec<u8>>>,
    shas: Vec<[u8; 32]>,
    /// Granted helper -> latest lease expiry (Unix seconds).
    granted: HashMap<PeerId, u64>,
}

impl<'a> JobGrants<'a> {
    /// Nothing granted yet.
    pub fn new(net: &'a PeerNet, blobs: Vec<Vec<u8>>) -> Self {
        let set: HashMap<[u8; 32], Arc<Vec<u8>>> = blobs
            .into_iter()
            .map(|b| (Sha256::digest(&b).into(), Arc::new(b)))
            .collect();
        let shas = set.keys().copied().collect();
        Self {
            net,
            set,
            shas,
            granted: HashMap::new(),
        }
    }

    /// Make the grants match `resp` (the server's list for `job_id`) at `now` (Unix seconds).
    /// A response for another job id is ignored.
    pub fn apply(&mut self, job_id: &str, resp: &JobLeasesResponse, now: u64) {
        if resp.job_id != job_id {
            return;
        }
        let own = self.net.peer_id();
        let mut want: HashMap<PeerId, u64> = HashMap::new();
        for l in &resp.leases {
            if l.expires_at <= now || l.helper_peer_id.is_empty() {
                continue;
            }
            let Ok(p) = PeerId::from_str(&l.helper_peer_id) else {
                continue;
            };
            if p == own || (want.len() >= MAX_GRANTS && !want.contains_key(&p)) {
                continue;
            }
            let e = want.entry(p).or_insert(0);
            *e = (*e).max(l.expires_at);
        }
        let gone: Vec<PeerId> = self
            .granted
            .keys()
            .filter(|p| !want.contains_key(p))
            .copied()
            .collect();
        for p in gone {
            self.revoke(&p);
        }
        for (p, exp) in want {
            // Re-granted on every poll: idempotent, and it restores blobs another job's revoke removed.
            self.net.grant_raw(p, self.set.clone());
            self.granted.insert(p, exp);
        }
    }

    /// Revoke grants whose lease expired by `now` (Unix seconds).
    pub fn expire(&mut self, now: u64) {
        let gone: Vec<PeerId> = self
            .granted
            .iter()
            .filter(|(_, &e)| e <= now)
            .map(|(p, _)| *p)
            .collect();
        for p in gone {
            self.revoke(&p);
        }
    }

    fn revoke(&mut self, p: &PeerId) {
        self.granted.remove(p);
        self.net.revoke_blobs(p, &self.shas);
    }

    /// Helpers granted now.
    pub fn granted(&self) -> Vec<PeerId> {
        self.granted.keys().copied().collect()
    }

    /// Revoke everything this job granted.
    pub fn clear(&mut self) {
        let all: Vec<PeerId> = self.granted.keys().copied().collect();
        for p in all {
            self.revoke(&p);
        }
    }
}

impl Drop for JobGrants<'_> {
    fn drop(&mut self) {
        self.clear();
    }
}

/// Charge `bytes` about to be served to the requester's own budget gate (bulk class, current network and
/// tariff window). `false` makes the blob server answer status 3.
pub fn charge_served(
    budget: &crate::budget::BudgetGate,
    probe: &dyn crate::probe::PlatformProbe,
    clock: &dyn crate::probe::Clock,
    bytes: u64,
) -> bool {
    budget.charge(
        nc_tariff::TrafficClass::Bulk,
        bytes,
        clock.now(),
        &probe.network(),
    )
}

/// What [`serve_job`] needs. Cheap to clone.
#[derive(Clone)]
pub struct ServeCtx {
    /// Coordinator client of the submitting device.
    pub coord: Arc<dyn Coordinator>,
    /// Listening node.
    pub net: Arc<PeerNet>,
    /// Device public key the node's PeerId is bound to (for the peer registration refresh).
    pub device_public: [u8; 32],
    /// Charges one control call to the budget gate; `false`: not now.
    pub control: Arc<dyn Fn() -> bool + Send + Sync>,
    /// Current time, Unix seconds.
    pub now_unix: Arc<dyn Fn() -> u64 + Send + Sync>,
    /// Poll interval (production: [`POLL`]).
    pub poll: Duration,
}

impl std::fmt::Debug for ServeCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServeCtx").finish_non_exhaustive()
    }
}

/// Why [`serve_job`] returned. Every grant of the job is revoked by then.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServeEnd {
    /// The job reached a terminal state (`committed`, `failed`, `disputed`, `expired`).
    Ended(String),
    /// The wait ended.
    Deadline,
    /// Stop requested.
    Stopped,
    /// The coordinator does not offer the lease list for this job (404 / 401 / 403).
    Unavailable,
}

fn terminal(state: &str) -> bool {
    matches!(state, "committed" | "failed" | "disputed" | "expired")
}

/// Sleep `d` in small steps; `true` when `stop` was set.
fn nap(d: Duration, stop: &AtomicBool) -> bool {
    let end = Instant::now() + d;
    while Instant::now() < end {
        if stop.load(Ordering::SeqCst) {
            return true;
        }
        std::thread::sleep((end - Instant::now()).min(Duration::from_millis(20)));
    }
    stop.load(Ordering::SeqCst)
}

/// Serve `job_id`'s `blobs` to the helpers the coordinator lists, until the job ends, `until`, or `stop`.
pub fn serve_job(
    ctx: &ServeCtx,
    job_id: &str,
    blobs: Vec<Vec<u8>>,
    until: Instant,
    stop: &AtomicBool,
) -> ServeEnd {
    let mut g = JobGrants::new(&ctx.net, blobs);
    let mut delay = ctx.poll;
    let mut idle_polls = 0u32;
    loop {
        if stop.load(Ordering::SeqCst) {
            return ServeEnd::Stopped;
        }
        if Instant::now() >= until {
            return ServeEnd::Deadline;
        }
        // Keep the requester's own PeerId and addresses registered so helpers get a hint.
        ctx.net
            .refresh_if_due(ctx.coord.as_ref(), &ctx.device_public, || (ctx.control)());
        g.expire((ctx.now_unix)());
        if (ctx.control)() {
            match ctx.coord.job_leases(job_id) {
                Ok(r) => {
                    delay = ctx.poll;
                    g.apply(job_id, &r, (ctx.now_unix)());
                    if g.granted().is_empty() {
                        idle_polls += 1;
                    } else {
                        idle_polls = 0;
                    }
                    if idle_polls % STATUS_EVERY == 1 && (ctx.control)() {
                        if let Ok(s) = ctx.coord.job_status(job_id) {
                            if let Some(t) = s.tasks.first().filter(|t| terminal(&t.state)) {
                                return ServeEnd::Ended(t.state.clone());
                            }
                        }
                    }
                }
                Err(CoordError::Status {
                    status: 401 | 403 | 404,
                    ..
                }) => {
                    tracing::info!("coordinator does not list job leases; direct serving off");
                    return ServeEnd::Unavailable;
                }
                Err(e) => {
                    tracing::debug!(error = %e, "job lease list unavailable; backing off");
                    delay = (delay * 2).min(MAX_BACKOFF);
                }
            }
        } else {
            delay = (delay * 2).min(MAX_BACKOFF);
        }
        if nap(delay, stop) {
            return ServeEnd::Stopped;
        }
    }
}

/// A [`serve_job`] running on its own thread. Dropping it stops the loop and waits for it.
#[derive(Debug)]
pub struct Serving {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<ServeEnd>>,
}

impl Serving {
    /// Start serving `job_id` until it ends or `wait` passes.
    pub fn spawn(ctx: ServeCtx, job_id: String, blobs: Vec<Vec<u8>>, wait: Duration) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let until = Instant::now() + wait;
        let s = stop.clone();
        let thread = std::thread::Builder::new()
            .name("nc-requester-serve".into())
            .spawn(move || serve_job(&ctx, &job_id, blobs, until, &s))
            .ok();
        Self { stop, thread }
    }

    /// Stop flag shared with the loop.
    pub fn stop_flag(&self) -> Arc<AtomicBool> {
        self.stop.clone()
    }

    /// Whether the loop has returned.
    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Ask the loop to stop without waiting for it (it revokes its grants within one poll step or after an
    /// in-flight coordinator call).
    pub fn detach(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.thread = None;
    }

    /// Stop and return how it ended (`None` if the thread could not start or panicked).
    pub fn finish(mut self) -> Option<ServeEnd> {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.take().and_then(|t| t.join().ok())
    }
}

impl Drop for Serving {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
