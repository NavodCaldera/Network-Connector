//! Requester job records that survive the `nc` process exiting or the PC restarting (laptop-gap G9, 1.L7).
//!
//! `nc submit` writes a record under `<data_dir>/requests/<job id>/` **before** it uploads anything: the signed
//! manifest and a copy of each blob. If the process dies before the coordinator has the job, any later
//! `nc job status|wait|fetch <id>` finishes the upload and submit (the coordinator accepts the same signed
//! manifest again, so this is idempotent). Once the coordinator has the job the staged blobs are removed. The
//! committed output's hash and size are pinned in the record the first time they are seen; every fetch checks
//! the downloaded bytes against them.
//!
//! Records hold job data (module and input copies): never log their contents or paths.

use crate::coord::{CoordError, Coordinator};
use crate::requester::{self, BuiltJob, RequesterError};
use nc_protocol::pb::v1::{SignedJobManifest, SubmitJobRequest, TaskStatus, TransferRef};
use prost::Message;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Job store failure.
#[derive(Debug, thiserror::Error)]
pub enum JobError {
    /// Not a job id this store could have written.
    #[error("not a valid job id")]
    BadId,
    /// No local record for that id.
    #[error("no local record for this job")]
    Unknown,
    /// Disk read/write failed.
    #[error("job store I/O failed")]
    Io,
    /// The record or a staged blob is corrupt.
    #[error("job record is corrupt")]
    Corrupt,
    /// The coordinator's committed output differs from the one pinned earlier.
    #[error("committed output changed since it was first seen")]
    OutputChanged,
    /// Requester failure.
    #[error(transparent)]
    Requester(#[from] RequesterError),
}

impl From<CoordError> for JobError {
    fn from(e: CoordError) -> Self {
        Self::Requester(e.into())
    }
}

/// Where a job is, as far as this device knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Built and staged locally; the coordinator may not have it yet.
    Staged,
    /// The coordinator accepted it.
    Submitted,
}

/// On-disk record (`record.json`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRecord {
    /// Job id (UUID).
    pub job_id: String,
    /// Signed manifest, protobuf bytes as hex.
    pub manifest_hex: String,
    /// Blob hashes (hex) in upload order, module first.
    pub blobs: Vec<String>,
    /// Local phase.
    pub phase: Phase,
    /// Committed output, pinned the first time the coordinator reports it.
    pub output_sha256: Option<String>,
    /// Committed output size.
    pub output_size: Option<u64>,
    /// When the record was made (unix ms).
    pub created_ms: i64,
}

/// Retry policy for transient coordinator failures (same rule as [`requester::wait`]: network errors, 5xx
/// and 429 are retried with backoff until the deadline; anything else ends at once).
#[derive(Clone, Copy, Debug)]
pub struct Retry {
    /// Give up after this long.
    pub timeout: Duration,
    /// First backoff; doubles up to `max`.
    pub first: Duration,
    /// Largest backoff.
    pub max: Duration,
}

impl Retry {
    /// Default for the CLI.
    pub fn cli(timeout: Duration) -> Self {
        Self {
            timeout,
            first: Duration::from_millis(500),
            max: Duration::from_secs(15),
        }
    }

    fn run<T>(&self, mut f: impl FnMut() -> Result<T, CoordError>) -> Result<T, JobError> {
        let t0 = Instant::now();
        let mut d = self.first;
        loop {
            match f() {
                Ok(v) => return Ok(v),
                Err(e) if e.is_transient() && t0.elapsed() + d < self.timeout => {
                    tracing::warn!(error = %e, "coordinator unavailable; retrying");
                    std::thread::sleep(d);
                    d = (d * 2).min(self.max);
                }
                Err(e) if e.is_transient() => return Err(RequesterError::Timeout.into()),
                Err(e) => return Err(e.into()),
            }
        }
    }
}

/// Requester job records under one directory.
#[derive(Clone, Debug)]
pub struct JobStore {
    root: PathBuf,
}

fn check_id(id: &str) -> Result<String, JobError> {
    uuid::Uuid::parse_str(id)
        .map(|u| u.hyphenated().to_string())
        .map_err(|_| JobError::BadId)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), JobError> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).map_err(|_| JobError::Io)?;
    std::fs::rename(&tmp, path).map_err(|_| JobError::Io)
}

impl JobStore {
    /// Store rooted at `<data_dir>/requests`.
    pub fn new(data_dir: &Path) -> Self {
        Self {
            root: data_dir.join("requests"),
        }
    }

    fn dir(&self, id: &str) -> Result<PathBuf, JobError> {
        Ok(self.root.join(check_id(id)?))
    }

    /// Write the record and stage the blobs. Nothing is sent.
    pub fn stage(&self, job: &BuiltJob, now_ms: i64) -> Result<JobRecord, JobError> {
        let dir = self.dir(&job.job_id)?;
        std::fs::create_dir_all(&dir).map_err(|_| JobError::Io)?;
        for (hash, bytes) in &job.blobs {
            write_atomic(&dir.join(format!("{hash}.blob")), bytes)?;
        }
        let rec = JobRecord {
            job_id: check_id(&job.job_id)?,
            manifest_hex: hex::encode(job.signed.encode_to_vec()),
            blobs: job.blobs.iter().map(|(h, _)| h.clone()).collect(),
            phase: Phase::Staged,
            output_sha256: None,
            output_size: None,
            created_ms: now_ms,
        };
        self.save(&rec)?;
        Ok(rec)
    }

    /// Load a record.
    pub fn load(&self, id: &str) -> Result<JobRecord, JobError> {
        let p = self.dir(id)?.join("record.json");
        let bytes = match std::fs::read(&p) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(JobError::Unknown),
            Err(_) => return Err(JobError::Io),
        };
        let rec: JobRecord = serde_json::from_slice(&bytes).map_err(|_| JobError::Corrupt)?;
        if check_id(&rec.job_id)? != check_id(id)? {
            return Err(JobError::Corrupt);
        }
        Ok(rec)
    }

    fn save(&self, rec: &JobRecord) -> Result<(), JobError> {
        let dir = self.dir(&rec.job_id)?;
        std::fs::create_dir_all(&dir).map_err(|_| JobError::Io)?;
        let bytes = serde_json::to_vec_pretty(rec).map_err(|_| JobError::Corrupt)?;
        write_atomic(&dir.join("record.json"), &bytes)
    }

    /// Make sure the coordinator has the job: upload staged blobs and submit if the record is still staged.
    /// Safe to repeat after a crash at any point.
    pub fn ensure_submitted(
        &self,
        coord: &dyn Coordinator,
        id: &str,
        retry: Retry,
    ) -> Result<JobRecord, JobError> {
        let mut rec = self.load(id)?;
        if rec.phase == Phase::Submitted {
            return Ok(rec);
        }
        let dir = self.dir(id)?;
        let manifest = hex::decode(&rec.manifest_hex).map_err(|_| JobError::Corrupt)?;
        let signed =
            SignedJobManifest::decode(manifest.as_slice()).map_err(|_| JobError::Corrupt)?;
        for hash in &rec.blobs {
            let bytes =
                std::fs::read(dir.join(format!("{hash}.blob"))).map_err(|_| JobError::Io)?;
            if hex::encode(Sha256::digest(&bytes)) != *hash {
                return Err(JobError::Corrupt);
            }
            retry.run(|| coord.put_blob(hash, &bytes))?;
        }
        let req = SubmitJobRequest {
            manifest: Some(signed),
        };
        let resp = retry.run(|| coord.submit(&req))?;
        if check_id(&resp.job_id)? != rec.job_id {
            return Err(JobError::Corrupt);
        }
        rec.phase = Phase::Submitted;
        self.save(&rec)?;
        for hash in &rec.blobs {
            let _ = std::fs::remove_file(dir.join(format!("{hash}.blob")));
        }
        Ok(rec)
    }

    /// One status check (submitting first if needed). Returns the task status; a committed output is pinned.
    pub fn status(
        &self,
        coord: &dyn Coordinator,
        id: &str,
        retry: Retry,
    ) -> Result<TaskStatus, JobError> {
        let rec = self.ensure_submitted(coord, id, retry)?;
        let s = retry.run(|| coord.job_status(&rec.job_id))?;
        let t = s.tasks.into_iter().next().unwrap_or_default();
        self.pin(rec, &t)?;
        Ok(t)
    }

    /// Wait until committed (submitting first if needed), riding out coordinator outages until `timeout`.
    pub fn wait(
        &self,
        coord: &dyn Coordinator,
        id: &str,
        retry: Retry,
        poll: Duration,
    ) -> Result<TaskStatus, JobError> {
        let t0 = Instant::now();
        let rec = self.ensure_submitted(coord, id, retry)?;
        let left = retry.timeout.saturating_sub(t0.elapsed());
        let t = requester::wait(coord, &rec.job_id, left, poll)?;
        self.pin(rec, &t)?;
        Ok(t)
    }

    /// Wait for the committed output, download it and check it against the pinned hash and size.
    pub fn fetch(
        &self,
        coord: &dyn Coordinator,
        id: &str,
        retry: Retry,
        poll: Duration,
    ) -> Result<Vec<u8>, JobError> {
        let t = self.wait(coord, id, retry, poll)?;
        let out = t.output.ok_or(RequesterError::Output)?;
        let bytes = retry_fetch(coord, &out, retry)?;
        Ok(bytes)
    }

    fn pin(&self, mut rec: JobRecord, t: &TaskStatus) -> Result<(), JobError> {
        if t.state != "committed" {
            return Ok(());
        }
        let Some(out) = &t.output else {
            return Ok(());
        };
        match (&rec.output_sha256, rec.output_size) {
            (Some(h), Some(n)) => {
                if *h != out.sha256 || n != out.size {
                    return Err(JobError::OutputChanged);
                }
                Ok(())
            }
            _ => {
                rec.output_sha256 = Some(out.sha256.clone());
                rec.output_size = Some(out.size);
                self.save(&rec)
            }
        }
    }
}

fn retry_fetch(
    coord: &dyn Coordinator,
    out: &TransferRef,
    retry: Retry,
) -> Result<Vec<u8>, JobError> {
    let t0 = Instant::now();
    let mut d = retry.first;
    loop {
        match requester::fetch_output(coord, out) {
            Ok(b) => return Ok(b),
            Err(RequesterError::Coord(e))
                if e.is_transient() && t0.elapsed() + d < retry.timeout =>
            {
                tracing::warn!(error = %e, "output download unavailable; retrying");
                std::thread::sleep(d);
                d = (d * 2).min(retry.max);
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// Write `bytes` to `path` via a temporary file and rename, so a crash never leaves a partial output.
pub fn write_output(path: &Path, bytes: &[u8]) -> Result<(), JobError> {
    let mut name = path.file_name().ok_or(JobError::Io)?.to_os_string();
    name.push(".nc-partial");
    let tmp = path.with_file_name(name);
    std::fs::write(&tmp, bytes).map_err(|_| JobError::Io)?;
    std::fs::rename(&tmp, path).map_err(|_| {
        let _ = std::fs::remove_file(&tmp);
        JobError::Io
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::requester::JobSpec;
    use nc_protocol::pb::v1::{
        CompleteRequest, CompleteResponse, HeartbeatRequest, JobStatusResponse, LeaseFailRequest,
        LeaseFailResponse, LeaseRenewRequest, LeaseRenewResponse, LeaseResponse, RegisterRequest,
        SubmitJobResponse,
    };
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// In-memory coordinator: stores blobs and jobs; `fail_next` makes the next N calls fail with 503;
    /// `state` and `output` drive the job status.
    #[derive(Default)]
    struct Fake {
        blobs: Mutex<HashMap<String, Vec<u8>>>,
        jobs: Mutex<HashMap<String, Vec<u8>>>,
        submits: Mutex<u32>,
        fail_next: Mutex<u32>,
        state: Mutex<String>,
        output: Mutex<Option<TransferRef>>,
    }

    impl Fake {
        fn flaky(&self) -> Result<(), CoordError> {
            let mut n = self.fail_next.lock().unwrap();
            if *n > 0 {
                *n -= 1;
                return Err(CoordError::Status {
                    status: 503,
                    code: String::new(),
                });
            }
            Ok(())
        }
        fn commit(&self, bytes: &[u8]) {
            let sha = hex::encode(Sha256::digest(bytes));
            self.blobs
                .lock()
                .unwrap()
                .insert(sha.clone(), bytes.to_vec());
            *self.state.lock().unwrap() = "committed".into();
            *self.output.lock().unwrap() = Some(TransferRef {
                path: format!("/v1/blobs/{sha}"),
                sha256: sha,
                size: bytes.len() as u64,
            });
        }
    }

    impl Coordinator for Fake {
        fn register(&self, _: &RegisterRequest) -> Result<(), CoordError> {
            Ok(())
        }
        fn heartbeat(&self, _: &HeartbeatRequest) -> Result<(), CoordError> {
            unimplemented!()
        }
        fn lease(&self) -> Result<Option<LeaseResponse>, CoordError> {
            unimplemented!()
        }
        fn fail(&self, _: &LeaseFailRequest) -> Result<LeaseFailResponse, CoordError> {
            unimplemented!()
        }
        fn renew(&self, _: &LeaseRenewRequest) -> Result<LeaseRenewResponse, CoordError> {
            unimplemented!()
        }
        fn complete(&self, _: &CompleteRequest) -> Result<CompleteResponse, CoordError> {
            unimplemented!()
        }
        fn get_blob(
            &self,
            path: &str,
            _: u64,
        ) -> Result<Box<dyn std::io::Read + Send>, CoordError> {
            self.flaky()?;
            let sha = path.trim_start_matches("/v1/blobs/");
            let b = self
                .blobs
                .lock()
                .unwrap()
                .get(sha)
                .cloned()
                .ok_or(CoordError::Status {
                    status: 404,
                    code: String::new(),
                })?;
            Ok(Box::new(std::io::Cursor::new(b)))
        }
        fn put_blob(&self, hash: &str, bytes: &[u8]) -> Result<(), CoordError> {
            self.flaky()?;
            assert_eq!(hex::encode(Sha256::digest(bytes)), hash);
            self.blobs
                .lock()
                .unwrap()
                .insert(hash.into(), bytes.to_vec());
            Ok(())
        }
        fn submit(&self, req: &SubmitJobRequest) -> Result<SubmitJobResponse, CoordError> {
            self.flaky()?;
            let sm = req.manifest.clone().unwrap();
            let j = nc_protocol::jobs::verify_job(&sm, chrono::Utc::now().timestamp_millis())
                .map_err(|_| CoordError::Decode)?;
            // Every blob must be present before submit, as on the real coordinator.
            for r in j.input_transfers.iter().chain(j.module_transfer.iter()) {
                if !self
                    .blobs
                    .lock()
                    .unwrap()
                    .contains_key(&hex::encode(&r.sha256))
                {
                    return Err(CoordError::Status {
                        status: 409,
                        code: "blob_missing".into(),
                    });
                }
            }
            let t = j.task.unwrap();
            let id = uuid::Uuid::from_slice(&t.task_id).unwrap().to_string();
            let enc = sm.encode_to_vec();
            let mut jobs = self.jobs.lock().unwrap();
            if let Some(prev) = jobs.get(&id) {
                if *prev != enc {
                    return Err(CoordError::Status {
                        status: 409,
                        code: "job_exists".into(),
                    });
                }
            } else {
                jobs.insert(id.clone(), enc);
                *self.state.lock().unwrap() = "queued".into();
            }
            *self.submits.lock().unwrap() += 1;
            Ok(SubmitJobResponse {
                job_id: id,
                ..Default::default()
            })
        }
        fn job_status(&self, id: &str) -> Result<JobStatusResponse, CoordError> {
            self.flaky()?;
            if !self.jobs.lock().unwrap().contains_key(id) {
                return Err(CoordError::Status {
                    status: 404,
                    code: String::new(),
                });
            }
            Ok(JobStatusResponse {
                job_id: id.into(),
                tasks: vec![TaskStatus {
                    task_id: id.into(),
                    state: self.state.lock().unwrap().clone(),
                    output: self.output.lock().unwrap().clone(),
                }],
            })
        }
    }

    struct Key(ed25519_dalek::SigningKey);
    impl nc_protocol::Signer for Key {
        fn public_key(&self) -> [u8; 32] {
            self.0.verifying_key().to_bytes()
        }
        fn sign(&self, msg: &[u8]) -> Result<[u8; 64], nc_protocol::ProtocolError> {
            use ed25519_dalek::Signer as _;
            Ok(self.0.sign(msg).to_bytes())
        }
    }

    fn built() -> BuiltJob {
        let k = Key(ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]));
        let spec = JobSpec::new(b"\0asm\x01\0\0\0".to_vec(), b"some input".to_vec());
        requester::build(&spec, &k, chrono::Utc::now().timestamp_millis()).unwrap()
    }

    fn fast(timeout_ms: u64) -> Retry {
        Retry {
            timeout: Duration::from_millis(timeout_ms),
            first: Duration::from_millis(1),
            max: Duration::from_millis(4),
        }
    }
    const POLL: Duration = Duration::from_millis(1);

    #[test]
    fn staged_job_is_picked_up_by_a_new_process() {
        let dir = tempfile::tempdir().unwrap();
        let job = built();
        // Process 1 stages the job and "crashes" before uploading.
        JobStore::new(dir.path()).stage(&job, 1).unwrap();
        // Process 2 (fresh store, same data dir) finishes the submit on its first status check.
        let c = Fake::default();
        let s = JobStore::new(dir.path());
        assert_eq!(s.load(&job.job_id).unwrap().phase, Phase::Staged);
        let t = s.status(&c, &job.job_id, fast(1000)).unwrap();
        assert_eq!(t.state, "queued");
        let rec = s.load(&job.job_id).unwrap();
        assert_eq!(rec.phase, Phase::Submitted);
        // Staged blob copies are gone once the coordinator has them.
        for h in &rec.blobs {
            assert!(!dir
                .path()
                .join("requests")
                .join(&job.job_id)
                .join(format!("{h}.blob"))
                .exists());
        }
        // Repeating is idempotent: no second submit.
        s.status(&c, &job.job_id, fast(1000)).unwrap();
        assert_eq!(*c.submits.lock().unwrap(), 1);
    }

    #[test]
    fn crash_after_submit_before_record_update_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let job = built();
        let s = JobStore::new(dir.path());
        s.stage(&job, 1).unwrap();
        let c = Fake::default();
        // The coordinator got the job but the record still says staged.
        for (h, b) in &job.blobs {
            c.put_blob(h, b).unwrap();
        }
        c.submit(&SubmitJobRequest {
            manifest: Some(job.signed.clone()),
        })
        .unwrap();
        let rec = s.ensure_submitted(&c, &job.job_id, fast(1000)).unwrap();
        assert_eq!(rec.phase, Phase::Submitted);
        assert_eq!(c.jobs.lock().unwrap().len(), 1);
    }

    #[test]
    fn submit_and_fetch_ride_out_transient_errors() {
        let dir = tempfile::tempdir().unwrap();
        let job = built();
        let s = JobStore::new(dir.path());
        s.stage(&job, 1).unwrap();
        let c = Fake::default();
        *c.fail_next.lock().unwrap() = 3;
        s.ensure_submitted(&c, &job.job_id, fast(5000)).unwrap();
        c.commit(b"the output");
        *c.fail_next.lock().unwrap() = 3;
        let out = s.fetch(&c, &job.job_id, fast(5000), POLL).unwrap();
        assert_eq!(out, b"the output");
        let rec = s.load(&job.job_id).unwrap();
        assert_eq!(rec.output_size, Some(10));
    }

    #[test]
    fn coordinator_down_until_timeout_gives_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let job = built();
        let s = JobStore::new(dir.path());
        s.stage(&job, 1).unwrap();
        let c = Fake::default();
        *c.fail_next.lock().unwrap() = u32::MAX;
        assert!(matches!(
            s.ensure_submitted(&c, &job.job_id, fast(30)),
            Err(JobError::Requester(RequesterError::Timeout))
        ));
        // Still staged: a later run can finish it.
        assert_eq!(s.load(&job.job_id).unwrap().phase, Phase::Staged);
    }

    #[test]
    fn fetch_rejects_bytes_that_do_not_match_the_committed_hash() {
        let dir = tempfile::tempdir().unwrap();
        let job = built();
        let s = JobStore::new(dir.path());
        s.stage(&job, 1).unwrap();
        let c = Fake::default();
        s.ensure_submitted(&c, &job.job_id, fast(1000)).unwrap();
        c.commit(b"real output");
        // Tamper with the stored blob behind the committed hash.
        let sha = c.output.lock().unwrap().clone().unwrap().sha256;
        c.blobs.lock().unwrap().insert(sha, b"fake output".to_vec());
        assert!(matches!(
            s.fetch(&c, &job.job_id, fast(1000), POLL),
            Err(JobError::Requester(RequesterError::Output))
        ));
    }

    #[test]
    fn committed_output_is_pinned() {
        let dir = tempfile::tempdir().unwrap();
        let job = built();
        let s = JobStore::new(dir.path());
        s.stage(&job, 1).unwrap();
        let c = Fake::default();
        s.ensure_submitted(&c, &job.job_id, fast(1000)).unwrap();
        c.commit(b"first");
        assert_eq!(
            s.fetch(&c, &job.job_id, fast(1000), POLL).unwrap(),
            b"first"
        );
        // The coordinator later reports a different committed output: refuse it.
        c.commit(b"second");
        assert!(matches!(
            s.fetch(&c, &job.job_id, fast(1000), POLL),
            Err(JobError::OutputChanged)
        ));
    }

    #[test]
    fn corrupt_staged_blob_and_bad_ids_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let job = built();
        let s = JobStore::new(dir.path());
        s.stage(&job, 1).unwrap();
        let h = &job.blobs[1].0;
        std::fs::write(
            dir.path()
                .join("requests")
                .join(&job.job_id)
                .join(format!("{h}.blob")),
            b"tampered",
        )
        .unwrap();
        let c = Fake::default();
        assert!(matches!(
            s.ensure_submitted(&c, &job.job_id, fast(1000)),
            Err(JobError::Corrupt)
        ));
        assert!(c.jobs.lock().unwrap().is_empty());
        assert!(matches!(s.load("../../etc"), Err(JobError::BadId)));
        assert!(matches!(
            s.load("00000000-0000-0000-0000-000000000000"),
            Err(JobError::Unknown)
        ));
    }

    #[test]
    fn terminal_state_ends_wait() {
        let dir = tempfile::tempdir().unwrap();
        let job = built();
        let s = JobStore::new(dir.path());
        s.stage(&job, 1).unwrap();
        let c = Fake::default();
        s.ensure_submitted(&c, &job.job_id, fast(1000)).unwrap();
        *c.state.lock().unwrap() = "disputed".into();
        assert!(matches!(
            s.wait(&c, &job.job_id, fast(1000), POLL),
            Err(JobError::Requester(RequesterError::Ended(st))) if st == "disputed"
        ));
    }

    #[test]
    fn write_output_replaces_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("out.bin");
        write_output(&p, b"a").unwrap();
        write_output(&p, b"bc").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"bc");
        assert!(!dir.path().join("out.bin.nc-partial").exists());
    }
}
