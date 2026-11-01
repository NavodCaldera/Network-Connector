//! Real-job validation shared by the coordinator and future device clients.
use crate::{pb::v1::*, sign::*, wire::decode_strict, PROFILE_CPU_CORE_V1};

/// Domain for the publisher's complete job signature.
pub const DOMAIN_JOB: &str = "nc.v1.JobManifest";
/// Coordinator maxima; helpers may enforce stricter owner limits.
pub const MAX_TRANSFER: u64 = 16 * 1024 * 1024;
/// Maximum total plaintext input bytes.
pub const MAX_TOTAL_INPUT: u64 = 1024 * 1024 * 1024;

/// Verify canonical bytes, publisher signatures, structure and resource maxima.
pub fn verify_job(s: &SignedJobManifest, now_ms: i64) -> Result<JobManifest, ProtocolError> {
    if s.manifest.len() > MAX_MANIFEST_BYTES {
        return Err(ProtocolError::TooLarge);
    }
    let job: JobManifest = decode_strict(&s.manifest).map_err(|_| ProtocolError::Decode)?;
    let t = job
        .task
        .as_ref()
        .ok_or(ProtocolError::Invalid("task missing"))?;
    verify_domain(&t.publisher_key, DOMAIN_JOB, &s.manifest, &s.signature)?;
    verify_module_signature(&t.publisher_key, &t.module_sha256, &t.module_signature)?;
    let key_id: String = t.publisher_key.iter().map(|b| format!("{b:02x}")).collect();
    if job.publisher_key_id != key_id
        || t.publisher_key.len() != 32
        || t.task_id.len() != 16
        || t.version
            .as_ref()
            .is_none_or(|v| v.major != 1 || v.minor != 0)
        || t.profile != PROFILE_CPU_CORE_V1
    {
        return Err(ProtocolError::Invalid("identity or profile"));
    }
    if t.expires_at_unix_ms <= now_ms {
        return Err(ProtocolError::Expired);
    }
    if t.expires_at_unix_ms.saturating_sub(now_ms) > 7 * 24 * 3600 * 1000 {
        return Err(ProtocolError::Invalid("deadline exceeds maximum"));
    }
    if t.inputs.len() > MAX_INPUTS || job.input_transfers.len() != t.inputs.len() {
        return Err(ProtocolError::Invalid("chunk count"));
    }
    let mut total = 0u64;
    for i in &t.inputs {
        if i.sha256.len() != 32 || i.size == 0 || i.size > 8 * 1024 * 1024 {
            return Err(ProtocolError::Invalid("input chunk"));
        }
        total = total.checked_add(i.size).ok_or(ProtocolError::TooLarge)?;
    }
    if total != job.total_input_size || total > MAX_TOTAL_INPUT {
        return Err(ProtocolError::Invalid("input total"));
    }
    let module = job
        .module_transfer
        .as_ref()
        .ok_or(ProtocolError::Invalid("module transfer missing"))?;
    for r in job.input_transfers.iter().chain(std::iter::once(module)) {
        if r.sha256.len() != 32 || r.size == 0 || r.size > MAX_TRANSFER {
            return Err(ProtocolError::Invalid("transfer"));
        }
    }
    let l = t
        .limits
        .as_ref()
        .ok_or(ProtocolError::Invalid("limits missing"))?;
    if l.fuel == 0
        || l.fuel > 10_000_000_000
        || l.memory_bytes == 0
        || l.memory_bytes > 512 * 1024 * 1024
        || l.wall_ms == 0
        || l.wall_ms > 300_000
        || l.stack_bytes == 0
        || l.stack_bytes > 8 * 1024 * 1024
        || l.output_bytes == 0
        || l.output_bytes > MAX_TRANSFER
        || l.checkpoint_bytes > MAX_TRANSFER
    {
        return Err(ProtocolError::Invalid("resource limits"));
    }
    let v = t
        .verifier
        .as_ref()
        .ok_or(ProtocolError::Invalid("verifier missing"))?;
    if v.kind != verifier::Kind::ReplicateExact as i32
        || !(1..=2).contains(&v.replicas.max(1))
        || !(1..=2).contains(&job.min_helpers.max(1))
        || job.min_helpers.max(1) > v.replicas.max(1)
    {
        return Err(ProtocolError::Invalid("validation count"));
    }
    Ok(job)
}
