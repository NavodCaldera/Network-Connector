//! Requester flow against an in-memory coordinator (no network).

use super::*;
use nc_agent::coord::ChunkReply;
use nc_identity::SoftwareKeyStore;
use nc_protocol::pb::v1::{
    CompleteRequest, CompleteResponse, HeartbeatRequest, JobManifest, JobStatusResponse,
    LeaseFailRequest, LeaseFailResponse, LeaseRenewRequest, LeaseRenewResponse, LeaseResponse,
    RegisterRequest, SignedJobManifest, SubmitJobRequest as PbSubmit, SubmitJobResponse,
    TransferRef,
};
use nc_protocol::Signer;
use prost::Message;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

const WASM: &[u8] = b"\0asm\x01\0\0\0";

#[derive(Default)]
struct Fake {
    blobs: Mutex<HashMap<String, Vec<u8>>>,
    jobs: Mutex<Vec<String>>,
    offline: AtomicBool,
    state: Mutex<String>,
    output: Mutex<Option<TransferRef>>,
    /// Resumable upload sessions (hash -> bytes so far).
    sessions: Mutex<HashMap<String, Vec<u8>>>,
    /// Serve these bytes for every download instead of the stored ones (tampering).
    serve_instead: Mutex<Option<Vec<u8>>>,
}

impl Fake {
    fn net(&self) -> Result<(), CoordError> {
        if self.offline.load(Ordering::SeqCst) {
            return Err(CoordError::Network("down".into()));
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
    fn get_blob(&self, path: &str, _: u64) -> Result<Box<dyn std::io::Read + Send>, CoordError> {
        self.net()?;
        if let Some(b) = self.serve_instead.lock().unwrap().clone() {
            return Ok(Box::new(std::io::Cursor::new(b)));
        }
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
        self.net()?;
        assert_eq!(hex::encode(Sha256::digest(bytes)), hash);
        self.blobs
            .lock()
            .unwrap()
            .insert(hash.into(), bytes.to_vec());
        Ok(())
    }
    fn upload_offset(&self, hash: &str, _: u64) -> Result<u64, CoordError> {
        self.net()?;
        Ok(self
            .sessions
            .lock()
            .unwrap()
            .get(hash)
            .map_or(0, |v| v.len() as u64))
    }
    fn put_chunk(
        &self,
        hash: &str,
        size: u64,
        offset: u64,
        chunk: &[u8],
    ) -> Result<ChunkReply, CoordError> {
        self.net()?;
        let mut sessions = self.sessions.lock().unwrap();
        let held = sessions.entry(hash.into()).or_default();
        if held.len() as u64 != offset {
            return Ok(ChunkReply::Offset(held.len() as u64));
        }
        held.extend_from_slice(chunk);
        if (held.len() as u64) < size {
            return Ok(ChunkReply::Appended(held.len() as u64));
        }
        let all = sessions.remove(hash).unwrap();
        assert_eq!(hex::encode(Sha256::digest(&all)), hash);
        self.blobs.lock().unwrap().insert(hash.into(), all);
        Ok(ChunkReply::Complete)
    }
    fn submit(&self, req: &PbSubmit) -> Result<SubmitJobResponse, CoordError> {
        self.net()?;
        let id = task_of(req.manifest.as_ref().unwrap()).0;
        self.jobs.lock().unwrap().push(id.clone());
        let mut st = self.state.lock().unwrap();
        if st.is_empty() {
            *st = "queued".into();
        }
        Ok(SubmitJobResponse {
            job_id: id.clone(),
            task_ids: vec![id],
        })
    }
    fn job_status(&self, job_id: &str) -> Result<JobStatusResponse, CoordError> {
        self.net()?;
        Ok(JobStatusResponse {
            job_id: job_id.into(),
            tasks: vec![TaskStatus {
                task_id: job_id.into(),
                state: self.state.lock().unwrap().clone(),
                output: self.output.lock().unwrap().clone(),
            }],
        })
    }
}

/// (job id, publisher key, replicas) from a signed manifest.
fn task_of(m: &SignedJobManifest) -> (String, Vec<u8>, u32) {
    let job = JobManifest::decode(m.manifest.as_slice()).unwrap();
    let task = job.task.unwrap();
    let id = uuid::Uuid::from_slice(&task.task_id).unwrap().to_string();
    (id, task.publisher_key, task.verifier.unwrap().replicas)
}

struct Setup {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    fake: Arc<Fake>,
    ctx: Ctx,
}

fn fast() -> Retry {
    Retry {
        timeout: Duration::from_millis(200),
        first: Duration::from_millis(10),
        max: Duration::from_millis(20),
    }
}

fn setup() -> Setup {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    let fake = Arc::new(Fake::default());
    let key = SoftwareKeyStore::open_or_create(dir.join("device.key")).unwrap();
    let ctx = Ctx {
        store: store(&dir),
        coord: fake.clone(),
        key: Arc::new(key),
        retry: [fast(); 3],
    };
    Setup {
        _tmp: tmp,
        dir,
        fake,
        ctx,
    }
}

fn files(s: &Setup, module: &[u8], input: &[u8]) -> SubmitJobRequest {
    let m = s.dir.join("job.wasm");
    let i = s.dir.join("input.bin");
    std::fs::write(&m, module).unwrap();
    std::fs::write(&i, input).unwrap();
    SubmitJobRequest {
        module_path: m.to_string_lossy().into_owned(),
        input_path: i.to_string_lossy().into_owned(),
        replicas: 1,
    }
}

fn path(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

#[test]
fn submit_signs_with_the_device_key_and_lists_the_job() {
    let s = setup();
    let mut req = files(&s, WASM, b"hello");
    req.replicas = 2;
    let j = submit(&s.ctx, &req, 1_000).unwrap();
    assert_eq!(j.phase, JobPhase::Queued);
    assert_eq!(j.message, None);
    assert_eq!(
        s.fake.jobs.lock().unwrap().as_slice(),
        std::slice::from_ref(&j.job_id)
    );
    assert!(s
        .fake
        .blobs
        .lock()
        .unwrap()
        .contains_key(&hex::encode(Sha256::digest(WASM))));
    let rec = s.ctx.store.load(&j.job_id).unwrap();
    let signed =
        SignedJobManifest::decode(hex::decode(&rec.manifest_hex).unwrap().as_slice()).unwrap();
    let (id, publisher, replicas) = task_of(&signed);
    assert_eq!(id, j.job_id);
    let signer = RootSigner(s.ctx.key.as_ref());
    assert_eq!(publisher, signer.public_key().to_vec());
    assert_eq!(replicas, 2);
    let listed = list(&s.ctx.store, &s.dir);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].job_id, j.job_id);
    assert_eq!(listed[0].created_ms, 1_000);
}

#[test]
fn bad_requests_are_refused_in_plain_english() {
    let s = setup();
    let mut req = files(&s, WASM, b"x");
    req.replicas = 3;
    assert_eq!(
        submit(&s.ctx, &req, 0).unwrap_err(),
        "Choose 1 or 2 helpers."
    );
    let req = files(&s, b"not wasm", b"x");
    assert!(submit(&s.ctx, &req, 0).unwrap_err().contains("WebAssembly"));
    let req = files(&s, WASM, b"");
    assert_eq!(
        submit(&s.ctx, &req, 0).unwrap_err(),
        "The input file is empty."
    );
    let mut req = files(&s, WASM, b"x");
    req.module_path = path(&s.dir.join("missing.wasm"));
    assert_eq!(
        submit(&s.ctx, &req, 0).unwrap_err(),
        "Could not open the module file."
    );
    assert!(list(&s.ctx.store, &s.dir).is_empty());
    assert_eq!(
        status(&s.ctx, "not-an-id").unwrap_err(),
        "This job is not on this device."
    );
}

#[test]
fn offline_submit_is_kept_and_sent_by_a_later_status_after_restart() {
    let s = setup();
    s.fake.offline.store(true, Ordering::SeqCst);
    let j = submit(&s.ctx, &files(&s, WASM, b"in"), 5).unwrap();
    assert_eq!(j.phase, JobPhase::Uploading);
    assert!(j.message.unwrap().contains("Cannot reach the coordinator"));
    assert!(s.fake.jobs.lock().unwrap().is_empty());
    // "Restart": a fresh store over the same folder still lists the job.
    let again = list(&store(&s.dir), &s.dir);
    assert_eq!(again[0].phase, JobPhase::Uploading);
    let st = status(&s.ctx, &j.job_id).unwrap();
    assert_eq!(st.phase, JobPhase::Uploading);
    assert!(st.message.is_some());
    s.fake.offline.store(false, Ordering::SeqCst);
    let st = status(&s.ctx, &j.job_id).unwrap();
    assert_eq!(st.phase, JobPhase::Queued);
    assert_eq!(st.message, None);
    assert_eq!(s.fake.jobs.lock().unwrap().len(), 1);
}

#[test]
fn fetch_saves_a_hash_checked_output_and_refuses_a_tampered_one() {
    let s = setup();
    let j = submit(&s.ctx, &files(&s, WASM, b"in"), 5).unwrap();
    *s.fake.state.lock().unwrap() = "leased".into();
    assert_eq!(status(&s.ctx, &j.job_id).unwrap().phase, JobPhase::Running);
    s.fake.commit(b"result bytes");
    let st = status(&s.ctx, &j.job_id).unwrap();
    assert_eq!(st.phase, JobPhase::Committed);
    assert_eq!(st.output_size, Some(12));
    // Still committed after a restart: the output hash is pinned in the record.
    assert_eq!(list(&store(&s.dir), &s.dir)[0].phase, JobPhase::Committed);

    let out = s.dir.join("out.bin");
    let f = fetch(&s.ctx, &j.job_id, &path(&out)).unwrap();
    assert_eq!(f.size, 12);
    assert_eq!(f.sha256, hex::encode(Sha256::digest(b"result bytes")));
    assert_eq!(std::fs::read(&out).unwrap(), b"result bytes");

    let bad = s.dir.join("bad.bin");
    *s.fake.serve_instead.lock().unwrap() = Some(b"other bytes!".to_vec());
    let e = fetch(&s.ctx, &j.job_id, &path(&bad)).unwrap_err();
    assert!(e.contains("did not match its checksum"), "{e}");
    assert!(!bad.exists());
}

#[test]
fn ended_jobs_show_their_phase_and_fetch_explains() {
    let s = setup();
    let j = submit(&s.ctx, &files(&s, WASM, b"in"), 5).unwrap();
    *s.fake.state.lock().unwrap() = "disputed".into();
    assert_eq!(status(&s.ctx, &j.job_id).unwrap().phase, JobPhase::Disputed);
    let out = s.dir.join("o.bin");
    let e = fetch(&s.ctx, &j.job_id, &path(&out)).unwrap_err();
    assert_eq!(e, "The job ended without a result (disputed).");
    assert!(!out.exists());
    assert_eq!(
        fetch(&s.ctx, &j.job_id, " ").unwrap_err(),
        "Choose where to save the result."
    );
}

#[test]
fn not_enrolled_is_a_plain_error() {
    let tmp = tempfile::tempdir().unwrap();
    let e = ctx(tmp.path(), true).err().unwrap();
    assert!(e.starts_with("Enrol this device"), "{e}");
}
