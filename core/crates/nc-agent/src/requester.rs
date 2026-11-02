//! Requester side: build and sign a job (proto/README.md "Real jobs"), upload its blobs, submit, wait,
//! download the output. Used by `nc submit` and [`crate::Agent::submit_job`].

use crate::coord::{CoordError, Coordinator};
use crate::transfer::{read_capped, UploadError, Uploader, UPLOAD_CHUNK};
use nc_protocol::jobs::DOMAIN_JOB;
use nc_protocol::pb::v1::{
    verifier, InputRef, JobManifest, Limits, ProtocolVersion, SignedJobManifest, SubmitJobRequest,
    TaskManifest, TaskStatus, TransferRef, Verifier,
};
use nc_protocol::{sign_domain, sign_module, Signer, PROFILE_CPU_CORE_V1};
use prost::Message;
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

/// Largest plaintext input chunk (matches nc-blob and the coordinator).
pub const INPUT_CHUNK: usize = 8 * 1024 * 1024;

/// Requester failure.
#[derive(Debug, thiserror::Error)]
pub enum RequesterError {
    /// Bad job description (empty input, too big, ...).
    #[error("invalid job: {0}")]
    Invalid(&'static str),
    /// Signing failed.
    #[error("signing failed")]
    Sign,
    /// Coordinator failure.
    #[error(transparent)]
    Coord(#[from] CoordError),
    /// The job did not finish in time.
    #[error("timed out waiting for the job")]
    Timeout,
    /// The job ended without a committed result.
    #[error("job ended in state {0}")]
    Ended(String),
    /// Output hash or size did not match the status.
    #[error("output did not verify")]
    Output,
}

/// What to run.
#[derive(Clone, Debug)]
pub struct JobSpec {
    /// Wasm module bytes.
    pub module: Vec<u8>,
    /// Input bytes (split into 8 MiB chunks).
    pub input: Vec<u8>,
    /// Resource limits asked for.
    pub limits: Limits,
    /// Independent helpers that must agree (1 or 2).
    pub replicas: u32,
    /// How long the job may wait before it expires.
    pub ttl: Duration,
}

impl JobSpec {
    /// Defaults suitable for small CPU jobs.
    pub fn new(module: Vec<u8>, input: Vec<u8>) -> Self {
        Self {
            module,
            input,
            limits: Limits {
                fuel: 1_000_000_000,
                wall_ms: 30_000,
                memory_bytes: 64 * 1024 * 1024,
                stack_bytes: 1024 * 1024,
                output_bytes: 1024 * 1024,
                checkpoint_bytes: 0,
            },
            replicas: 1,
            ttl: Duration::from_secs(3600),
        }
    }
}

/// A signed job plus the blobs (hash hex, bytes) to upload, module first.
#[derive(Debug)]
pub struct BuiltJob {
    /// Signed manifest.
    pub signed: SignedJobManifest,
    /// Job id (UUID of the task id).
    pub job_id: String,
    /// Blobs to upload.
    pub blobs: Vec<(String, Vec<u8>)>,
}

/// Build and sign a job with `publisher` (the publisher signs both the module hash and the whole job).
pub fn build(
    spec: &JobSpec,
    publisher: &dyn Signer,
    now_ms: i64,
) -> Result<BuiltJob, RequesterError> {
    if spec.module.is_empty() || spec.input.is_empty() {
        return Err(RequesterError::Invalid(
            "module and input must not be empty",
        ));
    }
    let mut id = [0u8; 16];
    getrandom::fill(&mut id).map_err(|_| RequesterError::Sign)?;
    let task_uuid = uuid::Builder::from_random_bytes(id).into_uuid();
    let module_sha: [u8; 32] = Sha256::digest(&spec.module).into();
    let module_sig = sign_module(publisher, &module_sha).map_err(|_| RequesterError::Sign)?;
    let mut blobs = vec![(hex::encode(module_sha), spec.module.clone())];
    let mut inputs = Vec::new();
    for chunk in spec.input.chunks(INPUT_CHUNK) {
        let sha: [u8; 32] = Sha256::digest(chunk).into();
        inputs.push(InputRef {
            sha256: sha.to_vec(),
            size: chunk.len() as u64,
        });
        blobs.push((hex::encode(sha), chunk.to_vec()));
    }
    let ttl_ms = i64::try_from(spec.ttl.as_millis()).map_err(|_| RequesterError::Invalid("ttl"))?;
    let job = JobManifest {
        task: Some(TaskManifest {
            version: Some(ProtocolVersion { major: 1, minor: 0 }),
            task_id: task_uuid.as_bytes().to_vec(),
            profile: PROFILE_CPU_CORE_V1.into(),
            module_sha256: module_sha.to_vec(),
            publisher_key: publisher.public_key().to_vec(),
            module_signature: module_sig.to_vec(),
            inputs: inputs.clone(),
            limits: Some(spec.limits),
            verifier: Some(Verifier {
                kind: verifier::Kind::ReplicateExact as i32,
                replicas: spec.replicas,
            }),
            expires_at_unix_ms: now_ms.saturating_add(ttl_ms),
        }),
        total_input_size: spec.input.len() as u64,
        publisher_key_id: hex::encode(publisher.public_key()),
        min_helpers: spec.replicas,
        input_transfers: inputs,
        module_transfer: Some(InputRef {
            sha256: module_sha.to_vec(),
            size: spec.module.len() as u64,
        }),
    };
    let bytes = job.encode_to_vec();
    // Reject locally what the coordinator would reject.
    let sig = sign_domain(publisher, DOMAIN_JOB, &bytes).map_err(|_| RequesterError::Sign)?;
    let signed = SignedJobManifest {
        manifest: bytes,
        signature: sig.to_vec(),
    };
    nc_protocol::jobs::verify_job(&signed, now_ms)
        .map_err(|_| RequesterError::Invalid("job exceeds coordinator limits"))?;
    Ok(BuiltJob {
        signed,
        job_id: task_uuid.to_string(),
        blobs,
    })
}

/// Upload one blob with the resumable protocol (verified bytes already on the server are not sent again).
/// The requester side is not tariff-gated. A blob that keeps failing its hash check is reported as
/// `400 hash_mismatch`.
pub fn upload_blob(coord: &dyn Coordinator, hash: &str, bytes: &[u8]) -> Result<(), CoordError> {
    let up = Uploader {
        coord,
        gate: None,
        stopped: &|| false,
        wait: &|d| {
            std::thread::sleep(d);
            false
        },
        chunk: UPLOAD_CHUNK,
        retry_base: Duration::from_millis(50),
    };
    match up.upload(hash, bytes) {
        Ok(()) => Ok(()),
        Err(UploadError::Coord(e)) => Err(e),
        Err(UploadError::Verify | UploadError::NotNow(_) | UploadError::Stopped) => {
            Err(CoordError::Status {
                status: 400,
                code: "hash_mismatch".into(),
            })
        }
    }
}

/// Upload blobs and submit. Returns the coordinator's job id.
pub fn submit(coord: &dyn Coordinator, job: &BuiltJob) -> Result<String, RequesterError> {
    for (hash, bytes) in &job.blobs {
        upload_blob(coord, hash, bytes)?;
    }
    let resp = coord.submit(&SubmitJobRequest {
        manifest: Some(job.signed.clone()),
    })?;
    Ok(resp.job_id)
}

/// Poll `GET /v1/jobs/{id}` until the task is committed (returns its status) or ends otherwise.
///
/// Transient coordinator failures (unreachable, 5xx, 429) are retried until `timeout`: the job keeps running
/// on the helpers while the requester's link is down, so a disconnect must not lose the result. Permanent
/// errors (4xx, bad response) end the wait at once.
pub fn wait(
    coord: &dyn Coordinator,
    job_id: &str,
    timeout: Duration,
    poll: Duration,
) -> Result<TaskStatus, RequesterError> {
    let t0 = Instant::now();
    loop {
        match coord.job_status(job_id) {
            Ok(s) => {
                if let Some(t) = s.tasks.into_iter().next() {
                    match t.state.as_str() {
                        "committed" => return Ok(t),
                        "failed" | "disputed" | "expired" => {
                            return Err(RequesterError::Ended(t.state))
                        }
                        _ => {}
                    }
                }
            }
            Err(e) if e.is_transient() => {
                tracing::warn!(error = %e, "job status unavailable; retrying");
            }
            Err(e) => return Err(e.into()),
        }
        if t0.elapsed() >= timeout {
            return Err(RequesterError::Timeout);
        }
        std::thread::sleep(poll);
    }
}

/// Download and verify a committed output.
pub fn fetch_output(coord: &dyn Coordinator, out: &TransferRef) -> Result<Vec<u8>, RequesterError> {
    if out.size > nc_protocol::jobs::MAX_TRANSFER || !out.path.starts_with("/v1/blobs/") {
        return Err(RequesterError::Output);
    }
    let r = coord.get_blob(&out.path, 0)?;
    let bytes = read_capped(r, out.size).map_err(|_| RequesterError::Output)?;
    if bytes.len() as u64 != out.size || hex::encode(Sha256::digest(&bytes)) != out.sha256 {
        return Err(RequesterError::Output);
    }
    Ok(bytes)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use nc_protocol::pb::v1::{
        CompleteRequest, CompleteResponse, HeartbeatRequest, JobStatusResponse, LeaseFailRequest,
        LeaseFailResponse, LeaseRenewRequest, LeaseRenewResponse, LeaseResponse, RegisterRequest,
        SubmitJobResponse,
    };
    use std::sync::Mutex;

    /// Answers `job_status` from a script; everything else is unused.
    struct Script(Mutex<Vec<Result<JobStatusResponse, CoordError>>>);

    fn status(state: &str) -> Result<JobStatusResponse, CoordError> {
        Ok(JobStatusResponse {
            tasks: vec![TaskStatus {
                state: state.into(),
                ..Default::default()
            }],
            ..Default::default()
        })
    }

    impl Coordinator for Script {
        fn register(&self, _: &RegisterRequest) -> Result<(), CoordError> {
            unimplemented!()
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
        fn get_blob(&self, _: &str, _: u64) -> Result<Box<dyn std::io::Read + Send>, CoordError> {
            unimplemented!()
        }
        fn put_blob(&self, _: &str, _: &[u8]) -> Result<(), CoordError> {
            unimplemented!()
        }
        fn submit(&self, _: &SubmitJobRequest) -> Result<SubmitJobResponse, CoordError> {
            unimplemented!()
        }
        fn job_status(&self, _: &str) -> Result<JobStatusResponse, CoordError> {
            let mut s = self.0.lock().unwrap();
            if s.is_empty() {
                status("leased")
            } else {
                s.remove(0)
            }
        }
    }

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn wait_rides_out_a_coordinator_outage() {
        let c = Script(Mutex::new(vec![
            status("leased"),
            Err(CoordError::Network("refused".into())),
            Err(CoordError::Status {
                status: 503,
                code: String::new(),
            }),
            status("committed"),
        ]));
        let t = wait(&c, "j", Duration::from_secs(5), MS).unwrap();
        assert_eq!(t.state, "committed");
    }

    #[test]
    fn wait_stops_on_permanent_error_and_terminal_state() {
        let c = Script(Mutex::new(vec![Err(CoordError::Status {
            status: 403,
            code: "forbidden".into(),
        })]));
        assert!(matches!(
            wait(&c, "j", Duration::from_secs(5), MS),
            Err(RequesterError::Coord(_))
        ));
        let c = Script(Mutex::new(vec![status("disputed")]));
        assert!(matches!(
            wait(&c, "j", Duration::from_secs(5), MS),
            Err(RequesterError::Ended(s)) if s == "disputed"
        ));
    }

    #[test]
    fn wait_times_out_while_the_coordinator_stays_down() {
        let c = Script(Mutex::new(
            (0..10_000)
                .map(|_| Err(CoordError::Network("down".into())))
                .collect(),
        ));
        assert!(matches!(
            wait(&c, "j", Duration::from_millis(30), MS),
            Err(RequesterError::Timeout)
        ));
    }
}
