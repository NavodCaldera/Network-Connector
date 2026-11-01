//! Generated coordinator JSON bodies plus endpoint validation.
use crate::error::ApiError;
pub use nc_protocol::pb::v1::{
    CompleteRequest, CompleteResponse, HealthResponse, HeartbeatRequest, HeartbeatResponse,
    LeaseRequest, LeaseResponse, RegisterRequest, RegisterResponse, RelayTokenRequest,
    RelayTokenResponse,
};

/// Validate an HTTP request after strict JSON deserialization.
pub trait Validate {
    /// Reject fields outside the existing endpoint's permitted bounds.
    fn validate(&self) -> Result<(), ApiError>;
}

const PLATFORMS: &[&str] = &["windows", "linux", "macos", "android", "ios"];

impl Validate for RegisterRequest {
    fn validate(&self) -> Result<(), ApiError> {
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

impl Validate for HeartbeatRequest {
    fn validate(&self) -> Result<(), ApiError> {
        match self.status.as_str() {
            "idle" | "busy" => Ok(()),
            _ => Err(ApiError::bad_request("status must be idle or busy")),
        }
    }
}

impl Validate for CompleteRequest {
    fn validate(&self) -> Result<(), ApiError> {
        if self.result.len() > 4096 {
            return Err(ApiError::bad_request("result too large"));
        }
        Ok(())
    }
}

impl Validate for RelayTokenRequest {
    fn validate(&self) -> Result<(), ApiError> {
        let ok = (40..=64).contains(&self.peer_id.len())
            && self.peer_id.chars().all(|c| c.is_ascii_alphanumeric());
        if !ok {
            return Err(ApiError::bad_request("peer_id is not a libp2p peer id"));
        }
        Ok(())
    }
}
