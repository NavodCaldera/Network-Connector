//! Requester flow for the app (laptop-gap G6, 1.L18): submit a job, check it, fetch its output.
//!
//! Thin wrapper over [`nc_agent::jobs::JobStore`] (1.L7), exactly as `nc submit` / `nc job` use it: the job is
//! signed by this device's key (the device is the publisher), recorded under `<app dir>/agent/requests/<id>/`
//! before anything is sent, and resumed with retries on every later status check. The bridge takes file
//! paths, never job bytes (AGENT.md §3), and never logs paths or contents.

use crate::api::{FetchedOutput, JobPhase, RequestedJob, SubmitJobRequest};
use crate::enrol;
use nc_agent::coord::{CoordError, Coordinator, HttpCoordinator};
use nc_agent::jobs::{self, JobError, JobRecord, JobStore, Phase, Retry};
use nc_agent::requester::{self, JobSpec, RequesterError};
use nc_identity::{DeviceKeyStore, RootSigner};
use nc_protocol::pb::v1::TaskStatus;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Largest module file the app reads.
const MAX_MODULE_BYTES: u64 = 64 * 1024 * 1024;
/// Largest input file the app reads (held in memory while it is staged).
const MAX_INPUT_BYTES: u64 = 256 * 1024 * 1024;
const SUBMIT_TIMEOUT: Duration = Duration::from_secs(30);
const STATUS_TIMEOUT: Duration = Duration::from_secs(10);
const FETCH_TIMEOUT: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_secs(2);

/// Everything the requester calls need: the store, a coordinator and the signing key.
pub(crate) struct Ctx {
    pub(crate) store: JobStore,
    pub(crate) coord: Arc<dyn Coordinator>,
    pub(crate) key: Arc<dyn DeviceKeyStore>,
    /// Retry policies: submit, status, fetch.
    pub(crate) retry: [Retry; 3],
}

/// The app's retry policies (same backoff as the CLI, shorter limits so the UI stays responsive).
pub(crate) fn app_retry() -> [Retry; 3] {
    [
        Retry::cli(SUBMIT_TIMEOUT),
        Retry::cli(STATUS_TIMEOUT),
        Retry::cli(FETCH_TIMEOUT),
    ]
}

/// The job store of the app folder `dir` (same place the agent keeps its data).
pub(crate) fn store(dir: &Path) -> JobStore {
    JobStore::new(&enrol::agent_dir(dir))
}

/// Context from the saved enrolment. Plain-English error when the device is not enrolled.
pub(crate) fn ctx(dir: &Path, dev: bool) -> Result<Ctx, String> {
    let file = enrol::load(dir, dev)?.ok_or_else(|| {
        "Enrol this device with a coordinator first (Coordinator page).".to_string()
    })?;
    let key = enrol::open_key(dir)?;
    let coord = Arc::new(HttpCoordinator::new(
        &file.coordinator_url,
        key.clone(),
        None,
    ));
    Ok(Ctx {
        store: store(dir),
        coord,
        key,
        retry: app_retry(),
    })
}

fn read_file(path: &str, max: u64, what: &str) -> Result<Vec<u8>, String> {
    let p = PathBuf::from(path);
    let meta = std::fs::metadata(&p).map_err(|_| format!("Could not open the {what} file."))?;
    if !meta.is_file() {
        return Err(format!("Choose a {what} file."));
    }
    if meta.len() == 0 {
        return Err(format!("The {what} file is empty."));
    }
    if meta.len() > max {
        return Err(format!(
            "The {what} file is too big (at most {} MB).",
            max / 1_000_000
        ));
    }
    std::fs::read(&p).map_err(|_| format!("Could not read the {what} file."))
}

/// Plain English for a job-store error. Never includes paths or job data.
pub(crate) fn plain(e: &JobError) -> String {
    match e {
        JobError::BadId | JobError::Unknown => "This job is not on this device.".into(),
        JobError::Io => "Could not read or write the job files on this device.".into(),
        JobError::Corrupt => "The saved job is damaged and cannot be used.".into(),
        JobError::OutputChanged => {
            "The result changed after it was first reported. It was not saved.".into()
        }
        JobError::Requester(r) => match r {
            RequesterError::Invalid(_) => "The module or input cannot be used for a job.".into(),
            RequesterError::Sign => "Could not sign the job with this device's key.".into(),
            RequesterError::Timeout | RequesterError::Coord(CoordError::Network(_)) => {
                "Cannot reach the coordinator. It will try again when you refresh.".into()
            }
            RequesterError::Coord(CoordError::Status { status, .. }) => {
                format!("The coordinator refused the request (error {status}).")
            }
            RequesterError::Coord(_) => {
                "The coordinator sent an answer this app does not understand.".into()
            }
            RequesterError::Ended(state) => format!("The job ended without a result ({state})."),
            RequesterError::Output => {
                "The downloaded result did not match its checksum. It was not saved.".into()
            }
        },
    }
}

fn phase_of(state: &str) -> JobPhase {
    match state {
        "queued" => JobPhase::Queued,
        "leased" => JobPhase::Running,
        "committed" => JobPhase::Committed,
        "disputed" => JobPhase::Disputed,
        "failed" | "expired" => JobPhase::Failed,
        _ => JobPhase::Unknown,
    }
}

/// What the local record alone says (no network).
fn local(rec: &JobRecord) -> RequestedJob {
    let phase = match (rec.phase, &rec.output_sha256) {
        (Phase::Staged, _) => JobPhase::Uploading,
        (Phase::Submitted, Some(_)) => JobPhase::Committed,
        (Phase::Submitted, None) => JobPhase::Unknown,
    };
    RequestedJob {
        job_id: rec.job_id.clone(),
        phase,
        created_ms: rec.created_ms,
        output_sha256: rec.output_sha256.clone(),
        output_size: rec.output_size,
        message: None,
    }
}

fn with_status(rec: &JobRecord, t: &TaskStatus) -> RequestedJob {
    let mut j = local(rec);
    j.phase = phase_of(&t.state);
    if let Some(o) = t.output.as_ref().filter(|_| j.phase == JobPhase::Committed) {
        j.output_sha256 = Some(o.sha256.clone());
        j.output_size = Some(o.size);
    }
    j
}

/// Validate, sign with the device key, record, then try to send. A job that is recorded but could not be
/// sent yet is returned as `Uploading` with a message; a later [`status`] sends it.
pub(crate) fn submit(c: &Ctx, req: &SubmitJobRequest, now_ms: i64) -> Result<RequestedJob, String> {
    if !(1..=2).contains(&req.replicas) {
        return Err("Choose 1 or 2 helpers.".into());
    }
    let module = read_file(&req.module_path, MAX_MODULE_BYTES, "module")?;
    if !module.starts_with(b"\0asm") {
        return Err("The module file is not a WebAssembly (.wasm) module.".into());
    }
    let input = read_file(&req.input_path, MAX_INPUT_BYTES, "input")?;
    let mut spec = JobSpec::new(module, input);
    spec.replicas = req.replicas;
    let signer = RootSigner(c.key.as_ref());
    let job =
        requester::build(&spec, &signer, now_ms).map_err(|e| plain(&JobError::Requester(e)))?;
    drop(spec);
    let rec = c.store.stage(&job, now_ms).map_err(|e| plain(&e))?;
    drop(job);
    match c
        .store
        .ensure_submitted(c.coord.as_ref(), &rec.job_id, c.retry[0])
    {
        Ok(rec) => {
            let mut j = local(&rec);
            j.phase = JobPhase::Queued;
            Ok(j)
        }
        Err(e) => {
            let mut j = local(&rec);
            j.message = Some(plain(&e));
            Ok(j)
        }
    }
}

/// Every job recorded on this device, newest first. Local files only; no network.
pub(crate) fn list(store: &JobStore, dir: &Path) -> Vec<RequestedJob> {
    let root = enrol::agent_dir(dir).join("requests");
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out: Vec<RequestedJob> = entries
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|id| store.load(&id).ok())
        .map(|r| local(&r))
        .collect();
    out.sort_by(|a, b| {
        b.created_ms
            .cmp(&a.created_ms)
            .then(a.job_id.cmp(&b.job_id))
    });
    out
}

/// One status check, sending a recorded job first if the coordinator never got it.
pub(crate) fn status(c: &Ctx, id: &str) -> Result<RequestedJob, String> {
    let rec = c.store.load(id).map_err(|e| plain(&e))?;
    match c.store.status(c.coord.as_ref(), id, c.retry[1]) {
        Ok(t) => {
            let rec = c.store.load(id).unwrap_or(rec);
            Ok(with_status(&rec, &t))
        }
        Err(e @ JobError::OutputChanged) => Err(plain(&e)),
        Err(e) => {
            let rec = c.store.load(id).unwrap_or(rec);
            let mut j = local(&rec);
            j.message = Some(plain(&e));
            Ok(j)
        }
    }
}

/// Download the committed output, check it against the hash and size the coordinator reported (and pinned
/// the first time), then write it to `out` via a temporary file.
pub(crate) fn fetch(c: &Ctx, id: &str, out: &str) -> Result<FetchedOutput, String> {
    if out.trim().is_empty() {
        return Err("Choose where to save the result.".into());
    }
    c.store.load(id).map_err(|e| plain(&e))?;
    let bytes = c
        .store
        .fetch(c.coord.as_ref(), id, c.retry[2], POLL)
        .map_err(|e| plain(&e))?;
    let rec = c.store.load(id).map_err(|e| plain(&e))?;
    let (Some(sha), Some(size)) = (rec.output_sha256, rec.output_size) else {
        return Err(plain(&JobError::Corrupt));
    };
    jobs::write_output(Path::new(out), &bytes)
        .map_err(|_| "Could not save the result to that file.".to_string())?;
    Ok(FetchedOutput { sha256: sha, size })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;
