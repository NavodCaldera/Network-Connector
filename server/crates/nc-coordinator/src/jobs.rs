//! Transactional real-job routes. Task row is always the first state lock.
use super::*;
use axum::extract::Path;
use nc_protocol::{jobs::verify_job, pb::v1::*, wire::decode_strict};
use prost::Message;

fn invalid() -> ApiError {
    ApiError::bad_request("invalid job manifest")
}
fn job_task(j: &JobManifest) -> Result<&TaskManifest, ApiError> {
    j.task.as_ref().ok_or_else(invalid)
}
fn signed(bytes: &[u8]) -> Result<SignedJobManifest, ApiError> {
    decode_strict(bytes).map_err(|_| ApiError::internal())
}
fn transfer(hash: String, size: u64) -> TransferRef {
    TransferRef {
        path: format!("/v1/blobs/{hash}"),
        sha256: hash,
        size,
    }
}

pub async fn submit(
    State(s): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<SubmitJobResponse>, ApiError> {
    let (c, b) = read_signed(&s, &method, &uri, &headers, body).await?;
    let d = device(&s, &c).await?;
    let req: SubmitJobRequest = parse(&b)?;
    let sm = req.manifest.ok_or_else(invalid)?;
    let j = verify_job(&sm, now() * 1000).map_err(|_| invalid())?;
    let t = job_task(&j)?;
    let id = Uuid::from_slice(&t.task_id).map_err(|_| invalid())?;
    let mut tx = s.pool.begin().await?;
    let inserted = sqlx::query("INSERT INTO jobs(id,requester,manifest,deadline,validation_count,output_cap,created_at) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT DO NOTHING")
        .bind(id).bind(d.id).bind(sm.encode_to_vec()).bind(t.expires_at_unix_ms).bind(t.verifier.as_ref().ok_or_else(invalid)?.replicas.max(1) as i32)
        .bind(t.limits.as_ref().ok_or_else(invalid)?.output_bytes as i64).bind(now()).execute(&mut *tx).await?.rows_affected();
    if inserted == 0 {
        let r = sqlx::query("SELECT requester,manifest FROM jobs WHERE id=$1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        if r.try_get::<Uuid, _>("requester")? != d.id
            || r.try_get::<Vec<u8>, _>("manifest")? != sm.encode_to_vec()
        {
            return Err(ApiError::conflict("job_exists", "job id is already in use"));
        }
    } else {
        sqlx::query(
            "INSERT INTO tasks(id,job_id,state,input,created_at) VALUES($1,$1,'queued','',$2)",
        )
        .bind(id)
        .bind(now())
        .execute(&mut *tx)
        .await?;
        for r in j.input_transfers.iter().chain(j.module_transfer.iter()) {
            let hash = hex::encode(&r.sha256);
            // Repeated content must have a consistent size.
            let old = sqlx::query("SELECT size FROM job_blobs WHERE job_id=$1 AND sha256=$2")
                .bind(id)
                .bind(&hash)
                .fetch_optional(&mut *tx)
                .await?;
            if old.is_some_and(|x| x.get::<i64, _>("size") != r.size as i64) {
                return Err(invalid());
            }
            sqlx::query(
                "INSERT INTO job_blobs(job_id,sha256,size) VALUES($1,$2,$3) ON CONFLICT DO NOTHING",
            )
            .bind(id)
            .bind(hash)
            .bind(r.size as i64)
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;
    Ok(Json(SubmitJobResponse {
        job_id: id.to_string(),
        task_ids: vec![id.to_string()],
    }))
}

pub async fn lease(s: &AppState, d: &Device) -> Result<Option<LeaseResponse>, ApiError> {
    let now = now();
    let alive: bool = sqlx::query_scalar(
        "SELECT COALESCE(heartbeat_at > $2,false) AND status='idle' FROM devices WHERE id=$1",
    )
    .bind(d.id)
    .bind(now - 120)
    .fetch_one(&s.pool)
    .await?;
    if !alive {
        return Err(ApiError::conflict(
            "heartbeat_required",
            "recent idle heartbeat required",
        ));
    }
    // Serialize each task, but allow independent tasks to be scheduled concurrently.
    for _ in 0..32 {
        let mut tx = s.pool.begin().await?;
        // Lock the device row first (same order as deregister: device, then leases and tasks) so a
        // concurrent deregister either finishes before us (key detached: refuse) or waits for this lease
        // and then releases it.
        let registered: bool =
            sqlx::query_scalar("SELECT public_key IS NOT NULL FROM devices WHERE id=$1 FOR SHARE")
                .bind(d.id)
                .fetch_one(&mut *tx)
                .await?;
        if !registered {
            return Err(ApiError::unauthorized("device not registered"));
        }
        let r = sqlx::query("SELECT t.id,t.attempts,j.manifest,j.deadline FROM tasks t JOIN jobs j ON j.id=t.job_id
            WHERE t.state IN ('queued','leased')
            AND NOT EXISTS(SELECT 1 FROM leases l WHERE l.task_id=t.id AND l.state='active' AND l.expires_at>$2)
            AND NOT EXISTS(SELECT 1 FROM task_results r WHERE r.task_id=t.id AND r.device_id=$1)
            AND NOT EXISTS(SELECT 1 FROM task_results r JOIN devices od ON od.id=r.device_id WHERE r.task_id=t.id AND od.account_id=$3 AND $3<>$4)
            AND NOT EXISTS(SELECT 1 FROM job_blobs jb LEFT JOIN blobs b ON b.sha256=jb.sha256 WHERE jb.job_id=j.id AND (b.sha256 IS NULL OR b.size<>jb.size OR NOT EXISTS(SELECT 1 FROM blob_uploaders u WHERE u.sha256=jb.sha256 AND u.device_id=j.requester)))
            ORDER BY t.created_at,t.id LIMIT 1 FOR UPDATE OF t SKIP LOCKED")
            .bind(d.id).bind(now).bind(&d.account).bind(PHASE0_ACCOUNT).fetch_optional(&mut *tx).await?;
        let Some(r) = r else { return Ok(None) };
        let id: Uuid = r.try_get("id")?;
        // READ COMMITTED: the row lock re-evaluates only the task row, while the
        // NOT EXISTS subqueries used the statement's older snapshot. Re-check with a
        // fresh snapshot now that we hold the task lock, so a lease committed just
        // before our lock was granted is a lost race (try next task), not a double lease.
        let taken: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM leases WHERE task_id=$1 AND state='active' AND expires_at>$2)
             OR EXISTS(SELECT 1 FROM task_results WHERE task_id=$1 AND device_id=$3)
             OR EXISTS(SELECT 1 FROM task_results r JOIN devices od ON od.id=r.device_id WHERE r.task_id=$1 AND od.account_id=$4 AND $4<>$5)
             OR (SELECT state NOT IN ('queued','leased') FROM tasks WHERE id=$1)",
        )
        .bind(id)
        .bind(now)
        .bind(d.id)
        .bind(&d.account)
        .bind(PHASE0_ACCOUNT)
        .fetch_one(&mut *tx)
        .await?;
        if taken {
            tx.rollback().await?;
            continue;
        }
        sqlx::query("UPDATE leases SET state='expired' WHERE task_id=$1 AND state='active' AND expires_at<=$2").bind(id).bind(now).execute(&mut *tx).await?;
        if r.try_get::<i32, _>("attempts")? >= MAX_ATTEMPTS
            || r.try_get::<i64, _>("deadline")? <= now * 1000
        {
            sqlx::query("UPDATE tasks SET state='failed' WHERE id=$1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            continue;
        }
        let sm = signed(&r.try_get::<Vec<u8>, _>("manifest")?)?;
        let j: JobManifest = decode_strict(&sm.manifest).map_err(|_| ApiError::internal())?;
        let expiry = (now + LEASE_TTL_SECS).min(r.try_get::<i64, _>("deadline")? / 1000);
        let lease = Uuid::new_v4();
        let attempt = Uuid::new_v4();
        sqlx::query("INSERT INTO leases(id,task_id,attempt_id,device_id,issued_at,expires_at,state) VALUES($1,$2,$3,$4,$5,$6,'active')")
            .bind(lease).bind(id).bind(attempt).bind(d.id).bind(now).bind(expiry).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO attempts(id,task_id,device_id) VALUES($1,$2,$3)")
            .bind(attempt)
            .bind(id)
            .bind(d.id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE tasks SET state='leased',attempts=attempts+1 WHERE id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        #[allow(deprecated)]
        let response = LeaseResponse {
            lease_id: lease.to_string(),
            task_id: id.to_string(),
            attempt_id: attempt.to_string(),
            expires_at: expiry as u64,
            input: String::new(),
            manifest: Some(sm),
            transfers: j
                .module_transfer
                .iter()
                .chain(j.input_transfers.iter())
                .map(|r| transfer(hex::encode(&r.sha256), r.size))
                .collect(),
            version: 2,
        };
        return Ok(Some(response));
    }
    Ok(None)
}

async fn lock_attempt(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    lease: &str,
    attempt: &str,
    device: Uuid,
) -> Result<(Uuid, sqlx::postgres::PgRow), ApiError> {
    let lease = Uuid::parse_str(lease).map_err(|_| invalid())?;
    let attempt = Uuid::parse_str(attempt).map_err(|_| invalid())?;
    let id: Option<Uuid> =
        sqlx::query_scalar("SELECT task_id FROM leases WHERE id=$1 AND device_id=$2")
            .bind(lease)
            .bind(device)
            .fetch_optional(&mut **tx)
            .await?;
    let id = id.ok_or_else(|| ApiError::not_found("no such lease"))?;
    sqlx::query("SELECT id FROM tasks WHERE id=$1 FOR UPDATE")
        .bind(id)
        .fetch_one(&mut **tx)
        .await?;
    let r = sqlx::query("SELECT l.*,t.state AS task_state,t.attempts,t.tariff_releases,j.deadline,j.validation_count,j.output_cap FROM leases l JOIN tasks t ON t.id=l.task_id JOIN jobs j ON j.id=t.job_id WHERE l.id=$1 FOR UPDATE OF l")
        .bind(lease).fetch_one(&mut **tx).await?;
    if r.try_get::<Uuid, _>("attempt_id")? != attempt {
        return Err(ApiError::conflict(
            "attempt_mismatch",
            "attempt id does not match this lease",
        ));
    }
    Ok((id, r))
}
fn active(r: &sqlx::postgres::PgRow) -> Result<(), ApiError> {
    if r.try_get::<String, _>("state")? != "active"
        || r.try_get::<i64, _>("expires_at")? <= now()
        || r.try_get::<i64, _>("deadline")? <= now() * 1000
        || !["queued", "leased"].contains(&r.try_get::<String, _>("task_state")?.as_str())
    {
        return Err(ApiError::conflict(
            "lease_expired",
            "lease is no longer active",
        ));
    }
    Ok(())
}

/// Expiry after a renewal at `now`: `now + LEASE_TTL_SECS`, capped by the job deadline, the
/// manifest wall-clock limit plus transfer grace, and [`LEASE_MAX_TOTAL_SECS`]. Never shortens.
pub fn renewed_expiry(
    now: i64,
    issued_at: i64,
    current: i64,
    deadline_ms: i64,
    wall_ms: u64,
) -> (i64, bool) {
    let wall = i64::try_from(wall_ms.div_ceil(1000)).unwrap_or(i64::MAX);
    let cap = (deadline_ms / 1000)
        .min(issued_at.saturating_add(LEASE_MAX_TOTAL_SECS))
        .min(
            issued_at
                .saturating_add(wall)
                .saturating_add(LEASE_TRANSFER_GRACE_SECS),
        );
    let want = now.saturating_add(LEASE_TTL_SECS);
    let next = want.min(cap).max(current);
    (next, want > cap)
}

pub async fn renew(
    State(s): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<LeaseRenewResponse>, ApiError> {
    let (c, b) = read_signed(&s, &method, &uri, &headers, body).await?;
    let d = device(&s, &c).await?;
    let req: LeaseRenewRequest = parse(&b)?;
    let mut tx = s.pool.begin().await?;
    // Same lock order as lease/complete/fail: task row first, then the lease row, with
    // fresh reads, so a renewal and an expiry reclaim of the same task are serialized.
    let (id, r) = lock_attempt(&mut tx, &req.lease_id, &req.attempt_id, d.id).await?;
    active(&r)?;
    let lease: Uuid = r.try_get("id")?;
    let manifest: Vec<u8> = sqlx::query_scalar(
        "SELECT manifest FROM jobs WHERE id=(SELECT job_id FROM tasks WHERE id=$1)",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    let j: JobManifest =
        decode_strict(&signed(&manifest)?.manifest).map_err(|_| ApiError::internal())?;
    let wall_ms = job_task(&j)
        .map_err(|_| ApiError::internal())?
        .limits
        .as_ref()
        .ok_or_else(ApiError::internal)?
        .wall_ms;
    let (expiry, capped) = renewed_expiry(
        now(),
        r.try_get("issued_at")?,
        r.try_get("expires_at")?,
        r.try_get("deadline")?,
        wall_ms,
    );
    sqlx::query("UPDATE leases SET expires_at=$2 WHERE id=$1 AND state='active'")
        .bind(lease)
        .bind(expiry)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(LeaseRenewResponse {
        lease_id: lease.to_string(),
        expires_at: expiry as u64,
        capped,
    }))
}

pub async fn complete(
    State(s): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<CompleteResponse>, ApiError> {
    let (c, b) = read_signed(&s, &method, &uri, &headers, body).await?;
    let d = device(&s, &c).await?;
    let req: CompleteRequest = parse(&b)?;
    if req.version != 2 {
        return Err(ApiError::bad_request("complete requires version 2"));
    }
    if !req.result.is_empty() || !blobs::valid_hash(&req.output_sha256) {
        return Err(ApiError::bad_request("invalid output reference"));
    }
    let mut tx = s.pool.begin().await?;
    let (id, r) = lock_attempt(&mut tx, &req.lease_id, &req.attempt_id, d.id).await?;
    let attempt: Uuid = r.try_get("attempt_id")?;
    if let Some(old) = sqlx::query("SELECT sha256,size FROM task_results WHERE attempt_id=$1")
        .bind(attempt)
        .fetch_optional(&mut *tx)
        .await?
    {
        if old.try_get::<String, _>("sha256")? != req.output_sha256
            || old.try_get::<i64, _>("size")? as u64 != req.output_size
        {
            return Err(ApiError::conflict(
                "result_mismatch",
                "attempt already has a different result",
            ));
        }
        return Ok(Json(CompleteResponse {
            task_id: id.to_string(),
            state: r.try_get("task_state")?,
            duplicate: true,
        }));
    }
    active(&r)?;
    if req.output_size > r.try_get::<i64, _>("output_cap")? as u64 {
        return Err(ApiError::bad_request("output exceeds cap"));
    }
    let uploaded: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM blobs b JOIN blob_uploaders u ON u.sha256=b.sha256 WHERE b.sha256=$1 AND b.size=$2 AND u.device_id=$3)")
        .bind(&req.output_sha256).bind(req.output_size as i64).bind(d.id).fetch_one(&mut *tx).await?;
    if !uploaded {
        return Err(ApiError::bad_request("upload output blob first"));
    }
    sqlx::query(
        "INSERT INTO task_results(attempt_id,task_id,device_id,sha256,size) VALUES($1,$2,$3,$4,$5)",
    )
    .bind(attempt)
    .bind(id)
    .bind(d.id)
    .bind(&req.output_sha256)
    .bind(req.output_size as i64)
    .execute(&mut *tx)
    .await?;
    let results = sqlx::query("SELECT attempt_id,sha256,size FROM task_results WHERE task_id=$1")
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;
    let mut votes: Vec<(Uuid, String, i64)> = Vec::with_capacity(results.len());
    for v in &results {
        votes.push((
            v.try_get("attempt_id")?,
            v.try_get("sha256")?,
            v.try_get("size")?,
        ));
    }
    let need = r.try_get::<i32, _>("validation_count")?.max(1) as usize;
    let (state, winner) = match verdict(&votes, need) {
        Verdict::Commit(a) => ("committed", Some(a)),
        Verdict::Disputed => ("disputed", None),
        Verdict::NeedMore if r.try_get::<i32, _>("attempts")? >= MAX_ATTEMPTS => ("failed", None),
        Verdict::NeedMore => ("queued", None),
    };
    sqlx::query("UPDATE leases SET state='completed' WHERE attempt_id=$1")
        .bind(attempt)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE tasks SET state=$2,committed_attempt=$3 WHERE id=$1")
        .bind(id)
        .bind(state)
        .bind(winner)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(CompleteResponse {
        task_id: id.to_string(),
        state: state.into(),
        duplicate: false,
    }))
}

/// Outcome of the results recorded so far for one task.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// `need` replicas agree; commit this attempt's result.
    Commit(Uuid),
    /// Not enough agreement yet; lease one more replica to another device.
    NeedMore,
    /// No majority is possible within the replica budget.
    Disputed,
}

/// 1.L4 dispute resolution. A task needs `need` agreeing results (same sha256 and size, from
/// distinct devices). On a mismatch one extra replica goes to a third helper; with `need + 1`
/// results and no group of `need` (no 2-of-3 majority) the task is `disputed`.
pub fn verdict(votes: &[(Uuid, String, i64)], need: usize) -> Verdict {
    let need = need.max(1);
    let mut best: Option<(usize, Uuid)> = None;
    for (a, h, z) in votes {
        let n = votes
            .iter()
            .filter(|(_, h2, z2)| h2 == h && z2 == z)
            .count();
        if best.is_none_or(|(m, _)| n > m) {
            best = Some((n, *a));
        }
    }
    match best {
        Some((n, a)) if n >= need => Verdict::Commit(a),
        _ if votes.len() > need => Verdict::Disputed,
        _ => Verdict::NeedMore,
    }
}

pub async fn fail(
    State(s): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<LeaseFailResponse>, ApiError> {
    let (c, b) = read_signed(&s, &method, &uri, &headers, body).await?;
    let d = device(&s, &c).await?;
    let req: LeaseFailRequest = parse(&b)?;
    if !(1..=9).contains(&req.reason)
        || req.code.as_ref().is_some_and(|c| {
            c.is_empty()
                || c.len() > 32
                || !c
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        })
    {
        return Err(ApiError::bad_request("invalid failure reason or code"));
    }
    let mut tx = s.pool.begin().await?;
    let (id, r) = lock_attempt(&mut tx, &req.lease_id, &req.attempt_id, d.id).await?;
    let attempt: Uuid = r.try_get("attempt_id")?;
    let old = sqlx::query("SELECT reason,code FROM attempts WHERE id=$1")
        .bind(attempt)
        .fetch_one(&mut *tx)
        .await?;
    if let Some(reason) = old.try_get::<Option<i32>, _>("reason")? {
        if reason != req.reason || old.try_get::<Option<String>, _>("code")? != req.code {
            return Err(ApiError::conflict(
                "failure_mismatch",
                "attempt already failed differently",
            ));
        }
        return Ok(Json(LeaseFailResponse {
            task_id: id.to_string(),
            state: r.try_get("task_state")?,
            duplicate: true,
        }));
    }
    active(&r)?;
    // 1.L2: a refusal or failure caused only by the helper's tariff window is not the task's fault,
    // so it is released without counting toward the attempt cap (up to MAX_TARIFF_RELEASES).
    let released = req.reason == LeaseFailReason::TariffWindow as i32
        && r.try_get::<i32, _>("tariff_releases")? < MAX_TARIFF_RELEASES;
    let counted = r.try_get::<i32, _>("attempts")? - i32::from(released);
    let state = if counted >= MAX_ATTEMPTS {
        "failed"
    } else {
        "queued"
    };
    sqlx::query("UPDATE attempts SET reason=$2,code=$3 WHERE id=$1")
        .bind(attempt)
        .bind(req.reason)
        .bind(req.code)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE leases SET state='failed' WHERE attempt_id=$1")
        .bind(attempt)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE tasks SET state=$2,attempts=$3,tariff_releases=tariff_releases+$4 WHERE id=$1",
    )
    .bind(id)
    .bind(state)
    .bind(counted)
    .bind(i32::from(released))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(LeaseFailResponse {
        task_id: id.to_string(),
        state: state.into(),
        duplicate: false,
    }))
}

pub async fn status(
    State(s): State<AppState>,
    path: Result<Path<String>, axum::extract::rejection::PathRejection>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<JobStatusResponse>, ApiError> {
    let Path(id) = path.map_err(|_| ApiError::bad_request("invalid job path"))?;
    let (c, b) = read_signed(&s, &method, &uri, &headers, body).await?;
    if !b.is_empty() {
        return Err(ApiError::bad_request("GET body must be empty"));
    }
    let d = device(&s, &c).await?;
    let id = Uuid::parse_str(&id).map_err(|_| invalid())?;
    let r = sqlx::query("SELECT t.id,t.state,j.deadline FROM tasks t JOIN jobs j ON j.id=t.job_id WHERE j.id=$1 AND j.requester=$2").bind(id).bind(d.id).fetch_optional(&s.pool).await?.ok_or_else(||ApiError::not_found("no such job"))?;
    let mut state: String = r.try_get("state")?;
    if matches!(state.as_str(), "queued" | "leased")
        && r.try_get::<i64, _>("deadline")? <= now() * 1000
    {
        state = "failed".into();
    }
    let output = if state == "committed" {
        let r = sqlx::query("SELECT r.sha256,r.size FROM task_results r JOIN tasks t ON t.committed_attempt=r.attempt_id WHERE t.id=$1")
            .bind(id)
            .fetch_one(&s.pool)
            .await?;
        Some(transfer(
            r.try_get("sha256")?,
            r.try_get::<i64, _>("size")? as u64,
        ))
    } else {
        None
    };
    Ok(Json(JobStatusResponse {
        job_id: id.to_string(),
        tasks: vec![TaskStatus {
            task_id: id.to_string(),
            state,
            output,
        }],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: u128, h: &str) -> (Uuid, String, i64) {
        (Uuid::from_u128(a), h.into(), 1)
    }

    #[test]
    fn verdict_majority_and_dispute() {
        let id = Uuid::from_u128;
        assert_eq!(verdict(&[], 2), Verdict::NeedMore);
        assert_eq!(verdict(&[v(1, "a")], 1), Verdict::Commit(id(1)));
        assert_eq!(verdict(&[v(1, "a")], 2), Verdict::NeedMore);
        assert_eq!(verdict(&[v(1, "a"), v(2, "a")], 2), Verdict::Commit(id(1)));
        // Mismatch: schedule a third helper.
        assert_eq!(verdict(&[v(1, "a"), v(2, "b")], 2), Verdict::NeedMore);
        // 2-of-3 commits the majority result, whichever replica came first.
        let three = [v(1, "a"), v(2, "b"), v(3, "b")];
        assert_eq!(verdict(&three, 2), Verdict::Commit(id(2)));
        let three = [v(1, "a"), v(2, "b"), v(3, "a")];
        assert_eq!(verdict(&three, 2), Verdict::Commit(id(1)));
        // No majority.
        let three = [v(1, "a"), v(2, "b"), v(3, "c")];
        assert_eq!(verdict(&three, 2), Verdict::Disputed);
        // Same hash with a different size is a different result.
        let mut w = v(2, "a");
        w.2 = 2;
        assert_eq!(verdict(&[v(1, "a"), w], 2), Verdict::NeedMore);
    }

    #[test]
    fn renewal_extends_by_ttl() {
        let far = 10_000_000 * 1000;
        assert_eq!(
            renewed_expiry(1000, 1000, 1060, far, 3_600_000),
            (1060, false)
        );
        assert_eq!(
            renewed_expiry(1050, 1000, 1060, far, 3_600_000),
            (1110, false)
        );
    }

    #[test]
    fn renewal_is_capped_by_runtime_deadline_and_max_total() {
        let far = 10_000_000 * 1000;
        // wall 1.5 s rounds up to 2 s, plus transfer grace.
        let cap = 1000 + 2 + LEASE_TRANSFER_GRACE_SECS;
        assert_eq!(
            renewed_expiry(cap - 10, 1000, cap - 20, far, 1500),
            (cap, true)
        );
        // Deadline wins.
        assert_eq!(
            renewed_expiry(1030, 1000, 1060, 1070 * 1000, u64::MAX),
            (1070, true)
        );
        // Server max total wins over a huge manifest limit.
        let max = 1000 + LEASE_MAX_TOTAL_SECS;
        assert_eq!(
            renewed_expiry(max - 1, 1000, max - 2, far, u64::MAX),
            (max, true)
        );
    }

    #[test]
    fn renewal_never_shortens() {
        assert_eq!(
            renewed_expiry(5000, 1000, 1700, 10_000_000, 1000),
            (1700, true)
        );
    }
}
