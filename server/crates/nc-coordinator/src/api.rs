//! Request/response bodies of the coordinator HTTP API (Phase 0).
//!
//! TODO(proto): these are wire types and must move to `proto/` (owned by the Core agent); they are local
//! for task 0.7 only and are listed under "Requests" in the net-agent hand-back. Every request type has
//! `deny_unknown_fields` and a `validate()` that bounds every field.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::ApiError;

/// `POST /v1/devices/register`.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RegisterRequest {
    /// Display name, 1..=64 printable characters. Never used for trust.
    pub name: String,
    /// One of `windows`, `linux`, `macos`, `android`, `ios`.
    pub platform: String,
}

/// Response to register.
#[derive(Debug, Deserialize, Serialize)]
pub struct RegisterResponse {
    pub device_id: Uuid,
    /// False if this key was already registered (idempotent).
    pub created: bool,
}

/// `POST /v1/devices/heartbeat`.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HeartbeatRequest {
    /// `idle` or `busy`.
    pub status: String,
}

/// Response to heartbeat.
#[derive(Debug, Deserialize, Serialize)]
pub struct HeartbeatResponse {
    pub server_time: u64,
}

/// `POST /v1/leases/request` (empty object).
#[derive(Debug, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct LeaseRequest {}

/// A granted lease.
#[derive(Debug, Deserialize, Serialize)]
pub struct LeaseResponse {
    pub lease_id: Uuid,
    pub task_id: Uuid,
    pub attempt_id: Uuid,
    pub expires_at: u64,
    /// Dummy task input (Phase 0).
    pub input: String,
}

/// `POST /v1/leases/complete`.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CompleteRequest {
    pub lease_id: Uuid,
    pub attempt_id: Uuid,
    /// Result, at most 4096 bytes (dummy task).
    pub result: String,
}

/// Response to complete.
#[derive(Debug, Deserialize, Serialize)]
pub struct CompleteResponse {
    pub task_id: Uuid,
    /// Always `committed` on success.
    pub state: String,
    /// True if this exact attempt had already committed (idempotent retry).
    pub duplicate: bool,
}

/// `POST /v1/relay/token`.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RelayTokenRequest {
    /// libp2p peer id the token is for (the device's transport key).
    pub peer_id: String,
}

/// Response with a short-lived relay token. The token is a secret: clients must not log it.
#[derive(Deserialize, Serialize)]
pub struct RelayTokenResponse {
    pub token: String,
    pub expires_at: u64,
}

const PLATFORMS: &[&str] = &["windows", "linux", "macos", "android", "ios"];

impl RegisterRequest {
    pub fn validate(&self) -> Result<(), ApiError> {
        let n = self.name.chars().count();
        if n == 0 || n > 64 || self.name.chars().any(|c| c.is_control()) {
            return Err(ApiError::bad_request(
                "name must be 1-64 printable characters",
            ));
        }
        if !PLATFORMS.contains(&self.platform.as_str()) {
            return Err(ApiError::bad_request("unknown platform"));
        }
        Ok(())
    }
}

impl HeartbeatRequest {
    pub fn validate(&self) -> Result<(), ApiError> {
        match self.status.as_str() {
            "idle" | "busy" => Ok(()),
            _ => Err(ApiError::bad_request("status must be idle or busy")),
        }
    }
}

impl CompleteRequest {
    pub fn validate(&self) -> Result<(), ApiError> {
        if self.result.len() > 4096 {
            return Err(ApiError::bad_request("result too large"));
        }
        Ok(())
    }
}

impl RelayTokenRequest {
    pub fn validate(&self) -> Result<(), ApiError> {
        let ok = (40..=64).contains(&self.peer_id.len())
            && self.peer_id.chars().all(|c| c.is_ascii_alphanumeric());
        if !ok {
            return Err(ApiError::bad_request("peer_id is not a libp2p peer id"));
        }
        Ok(())
    }
}
