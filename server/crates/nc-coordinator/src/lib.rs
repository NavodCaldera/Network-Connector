//! `nc-coordinator`: Axum control plane (Phase 0 minimum, task 0.7).
//!
//! Endpoints (all `POST`, JSON, signed by the device key, see [`auth`]):
//! - `/v1/devices/register`  — enrol a device key (needs the owner's enrolment secret in Phase 0)
//! - `/v1/devices/heartbeat` — liveness + status
//! - `/v1/leases/request`    — lease the next (dummy) task
//! - `/v1/leases/complete`   — commit a result; idempotent and attempt-checked (AGENT.md §1 rule 8)
//! - `/v1/relay/token`       — short-lived relay token for this device's peer id
//!
//! plus unauthenticated `GET /healthz`. Bodies are capped at [`MAX_BODY`] bytes, parsed with
//! `deny_unknown_fields` and validated; every error uses the format in [`error`].

pub mod api;
pub mod auth;
pub mod error;

use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, HeaderMap, Method, Uri};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::de::DeserializeOwned;
use sqlx::{PgPool, Row};
use subtle_eq::ct_eq;
use uuid::Uuid;

use crate::api::*;
use crate::auth::Caller;
use crate::error::ApiError;

/// Largest accepted request body.
pub const MAX_BODY: usize = 16 * 1024;
/// Lease lifetime.
pub const LEASE_TTL_SECS: i64 = 60;
/// Relay token lifetime.
pub const RELAY_TOKEN_TTL_SECS: u64 = 600;
/// Phase 0 account for every enrolled device (OIDC accounts come later).
pub const PHASE0_ACCOUNT: &str = "owner";

/// Shared state.
#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    /// Owner enrolment secret for `register`. Never logged.
    pub enroll_secret: String,
    /// Relay token key; `None` disables `/v1/relay/token`.
    pub relay_key: Option<nc_relay::TokenKey>,
}

mod subtle_eq {
    /// Constant-time byte comparison.
    pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
        if a.len() != b.len() {
            return false;
        }
        a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Builds the router.
pub fn app(state: AppState) -> Router {
    Router::new()
        .route(
            "/healthz",
            get(|| async { Json(serde_json::json!({ "ok": true })) }),
        )
        .route("/v1/devices/register", post(register))
        .route("/v1/devices/heartbeat", post(heartbeat))
        .route("/v1/leases/request", post(lease_request))
        .route("/v1/leases/complete", post(lease_complete))
        .route("/v1/relay/token", post(relay_token))
        .fallback(|| async { ApiError::not_found("no such endpoint") })
        .method_not_allowed_fallback(|| async { ApiError::method_not_allowed() })
        .with_state(state)
}

/// Reads a capped body, checks content type and signature, and records the nonce (replay protection).
async fn read_signed(
    s: &AppState,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: Body,
) -> Result<(Caller, Bytes), ApiError> {
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !ct.starts_with("application/json") {
        return Err(ApiError::unsupported_media());
    }
    let bytes = axum::body::to_bytes(body, MAX_BODY)
        .await
        .map_err(|_| ApiError::too_large())?;
    let t = now();
    let caller = auth::verify(method, uri.path(), headers, &bytes, t)?;
    sqlx::query("DELETE FROM request_nonces WHERE seen_at < $1")
        .bind(t - 2 * auth::MAX_SKEW_SECS)
        .execute(&s.pool)
        .await?;
    let inserted = sqlx::query(
        "INSERT INTO request_nonces (public_key, nonce, seen_at) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
    )
    .bind(caller.public_key.as_slice())
    .bind(&caller.nonce)
    .bind(t)
    .execute(&s.pool)
    .await?
    .rows_affected();
    if inserted == 0 {
        return Err(ApiError::unauthorized("replayed request"));
    }
    Ok((caller, bytes))
}

fn parse<T: DeserializeOwned>(b: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice(b).map_err(|_| ApiError::bad_request("body does not match the schema"))
}

struct Device {
    id: Uuid,
    account: String,
}

async fn device(s: &AppState, c: &Caller) -> Result<Device, ApiError> {
    let row = sqlx::query("SELECT id, account_id FROM devices WHERE public_key = $1")
        .bind(c.public_key.as_slice())
        .fetch_optional(&s.pool)
        .await?
        .ok_or_else(|| ApiError::unauthorized("device not registered"))?;
    Ok(Device {
        id: row.try_get("id")?,
        account: row.try_get("account_id")?,
    })
}

async fn register(
    State(s): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<RegisterResponse>, ApiError> {
    let enroll = headers
        .get("x-nc-enroll")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if s.enroll_secret.is_empty() || !ct_eq(enroll.as_bytes(), s.enroll_secret.as_bytes()) {
        return Err(ApiError::unauthorized("enrolment secret required"));
    }
    let (c, b) = read_signed(&s, &method, &uri, &headers, body).await?;
    let req: RegisterRequest = parse(&b)?;
    req.validate()?;
    let t = now();
    let id = Uuid::new_v4();
    let row = sqlx::query(
        "INSERT INTO devices (id, public_key, account_id, name, platform, created_at, last_seen)
         VALUES ($1, $2, $3, $4, $5, $6, $6)
         ON CONFLICT (public_key) DO UPDATE SET last_seen = EXCLUDED.last_seen
         RETURNING id",
    )
    .bind(id)
    .bind(c.public_key.as_slice())
    .bind(PHASE0_ACCOUNT)
    .bind(&req.name)
    .bind(&req.platform)
    .bind(t)
    .fetch_one(&s.pool)
    .await?;
    let device_id: Uuid = row.try_get("id")?;
    tracing::info!(%device_id, "device registered");
    Ok(Json(RegisterResponse {
        device_id,
        created: device_id == id,
    }))
}

async fn heartbeat(
    State(s): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<HeartbeatResponse>, ApiError> {
    let (c, b) = read_signed(&s, &method, &uri, &headers, body).await?;
    let req: HeartbeatRequest = parse(&b)?;
    req.validate()?;
    let d = device(&s, &c).await?;
    let t = now();
    sqlx::query("UPDATE devices SET last_seen = $1, status = $2 WHERE id = $3")
        .bind(t)
        .bind(&req.status)
        .bind(d.id)
        .execute(&s.pool)
        .await?;
    Ok(Json(HeartbeatResponse {
        server_time: t as u64,
    }))
}

async fn lease_request(
    State(s): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<LeaseResponse>, ApiError> {
    let (c, b) = read_signed(&s, &method, &uri, &headers, body).await?;
    let _req: LeaseRequest = parse(&b)?;
    let d = device(&s, &c).await?;
    let t = now();
    let mut tx = s.pool.begin().await?;
    // Expired leases give their task back to the queue.
    sqlx::query(
        "WITH exp AS (
            UPDATE leases SET state = 'expired' WHERE state = 'active' AND expires_at <= $1 RETURNING task_id)
         UPDATE tasks SET state = 'queued' WHERE state = 'leased' AND id IN (SELECT task_id FROM exp)",
    )
    .bind(t)
    .execute(&mut *tx)
    .await?;
    let row = sqlx::query(
        "SELECT id, input FROM tasks WHERE state = 'queued' ORDER BY created_at LIMIT 1 FOR UPDATE SKIP LOCKED",
    )
    .fetch_optional(&mut *tx)
    .await?;
    let (task_id, input): (Uuid, String) = match row {
        Some(r) => (r.try_get("id")?, r.try_get("input")?),
        None => {
            // Phase 0: no scheduler yet, so make a dummy task on demand.
            let id = Uuid::new_v4();
            let input = format!("phase0-dummy-{}", &id.simple().to_string()[..8]);
            sqlx::query(
                "INSERT INTO tasks (id, state, input, created_at) VALUES ($1, 'queued', $2, $3)",
            )
            .bind(id)
            .bind(&input)
            .bind(t)
            .execute(&mut *tx)
            .await?;
            (id, input)
        }
    };
    let lease_id = Uuid::new_v4();
    let attempt_id = Uuid::new_v4();
    let expires_at = t + LEASE_TTL_SECS;
    sqlx::query("UPDATE tasks SET state = 'leased', attempts = attempts + 1 WHERE id = $1")
        .bind(task_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO leases (id, task_id, attempt_id, device_id, issued_at, expires_at, state)
         VALUES ($1, $2, $3, $4, $5, $6, 'active')",
    )
    .bind(lease_id)
    .bind(task_id)
    .bind(attempt_id)
    .bind(d.id)
    .bind(t)
    .bind(expires_at)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(LeaseResponse {
        lease_id,
        task_id,
        attempt_id,
        expires_at: expires_at as u64,
        input,
    }))
}

async fn lease_complete(
    State(s): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<CompleteResponse>, ApiError> {
    let (c, b) = read_signed(&s, &method, &uri, &headers, body).await?;
    let req: CompleteRequest = parse(&b)?;
    req.validate()?;
    let d = device(&s, &c).await?;
    let t = now();
    let mut tx = s.pool.begin().await?;
    let lease = sqlx::query(
        "SELECT task_id, device_id, attempt_id, state, expires_at FROM leases WHERE id = $1 FOR UPDATE",
    )
    .bind(req.lease_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::not_found("no such lease"))?;
    let device_id: Uuid = lease.try_get("device_id")?;
    if device_id != d.id {
        // Same answer as a missing lease: do not reveal other devices' leases.
        return Err(ApiError::not_found("no such lease"));
    }
    let attempt_id: Uuid = lease.try_get("attempt_id")?;
    if attempt_id != req.attempt_id {
        return Err(ApiError::conflict(
            "attempt_mismatch",
            "attempt id does not match this lease",
        ));
    }
    let task_id: Uuid = lease.try_get("task_id")?;
    let task = sqlx::query("SELECT state, committed_attempt FROM tasks WHERE id = $1 FOR UPDATE")
        .bind(task_id)
        .fetch_one(&mut *tx)
        .await?;
    let task_state: String = task.try_get("state")?;
    if task_state == "committed" {
        let committed: Option<Uuid> = task.try_get("committed_attempt")?;
        return if committed == Some(req.attempt_id) {
            Ok(Json(CompleteResponse {
                task_id,
                state: "committed".into(),
                duplicate: true,
            }))
        } else {
            Err(ApiError::conflict(
                "already_committed",
                "task was already committed by another attempt",
            ))
        };
    }
    let lease_state: String = lease.try_get("state")?;
    let expires_at: i64 = lease.try_get("expires_at")?;
    if lease_state != "active" || expires_at <= t {
        return Err(ApiError::conflict(
            "lease_expired",
            "lease is no longer active",
        ));
    }
    sqlx::query(
        "UPDATE tasks SET state = 'committed', result = $1, committed_attempt = $2 WHERE id = $3",
    )
    .bind(&req.result)
    .bind(req.attempt_id)
    .bind(task_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE leases SET state = 'completed' WHERE id = $1")
        .bind(req.lease_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    tracing::info!(%task_id, "task committed");
    Ok(Json(CompleteResponse {
        task_id,
        state: "committed".into(),
        duplicate: false,
    }))
}

async fn relay_token(
    State(s): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<RelayTokenResponse>, ApiError> {
    let (c, b) = read_signed(&s, &method, &uri, &headers, body).await?;
    let req: RelayTokenRequest = parse(&b)?;
    req.validate()?;
    let d = device(&s, &c).await?;
    let Some(key) = &s.relay_key else {
        return Err(ApiError::not_found("relay tokens are not enabled"));
    };
    let (token, claims) = key.issue(
        &req.peer_id,
        &d.account,
        &d.id.to_string(),
        nc_relay::unix_now(),
        RELAY_TOKEN_TTL_SECS,
    );
    Ok(Json(RelayTokenResponse {
        token,
        expires_at: claims.exp,
    }))
}
