//! `nc-coordinator`: authenticated job scheduling and opaque blob staging.
//!
//! Control requests use signed JSON; blob uploads stream signed opaque bytes (see [`auth`]).
//! - `/v1/devices/register`  — enrol a device key (needs the owner's enrolment secret in Phase 0)
//! - `/v1/devices/heartbeat` — liveness + status
//! - `/v1/leases/request`    — lease the next uploaded real job
//! - `/v1/leases/complete`   — commit a result; idempotent and attempt-checked (AGENT.md §1 rule 8)
//! - `/v1/relay/token`       — short-lived relay token for this device's peer id
//!
//! plus unauthenticated `GET /healthz`. Bodies are capped at [`MAX_BODY`] bytes, parsed with
//! `deny_unknown_fields` and validated; every error uses the format in [`error`].

pub mod api;
pub mod auth;
mod blobs;
pub mod error;
mod jobs;

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
/// JSON byte arrays expand the 64 KiB canonical job envelope.
pub const MAX_JOB_BODY: usize = 384 * 1024;
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
    /// Private server-owned blob directory; never logged.
    pub blob_dir: std::path::PathBuf,
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
            get(|| async { Json(HealthResponse { ok: true }) }),
        )
        .route("/v1/devices/register", post(register))
        .route("/v1/devices/heartbeat", post(heartbeat))
        .route("/v1/leases/request", post(lease_request))
        .route("/v1/leases/complete", post(jobs::complete))
        .route("/v1/jobs", post(jobs::submit))
        .route("/v1/jobs/{id}", get(jobs::status))
        .route("/v1/leases/fail", post(jobs::fail))
        .route("/v1/blobs/{hash}", get(blobs::download).put(blobs::upload))
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
    if method == Method::POST && !ct.starts_with("application/json") {
        return Err(ApiError::unsupported_media());
    }
    let cap = if uri.path() == "/v1/jobs" && method == Method::POST {
        MAX_JOB_BODY
    } else {
        MAX_BODY
    };
    let bytes = axum::body::to_bytes(body, cap)
        .await
        .map_err(|_| ApiError::too_large())?;
    let t = now();
    let caller = auth::verify(method, uri.path(), headers, &bytes, t)?;
    record_nonce(s, &caller, t).await?;
    Ok((caller, bytes))
}

async fn record_nonce(s: &AppState, caller: &Caller, t: i64) -> Result<(), ApiError> {
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
    Ok(())
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
        device_id: device_id.to_string(),
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
    sqlx::query("UPDATE devices SET last_seen = $1, heartbeat_at = $1, status = $2 WHERE id = $3")
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
    jobs::lease(&s, &d)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("no queued task"))
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
        expires_at: claims.exp.ok_or_else(ApiError::internal)?,
    }))
}
