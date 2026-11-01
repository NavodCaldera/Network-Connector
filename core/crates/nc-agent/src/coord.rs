//! Coordinator client: the `proto/` HTTP JSON API with signed device requests (proto/README.md).
//!
//! The trait lets the session loop run against an in-process fake in tests. [`HttpCoordinator`] is the real
//! client. Nothing here logs request or response bodies, signatures or keys (AGENT.md §1 rule 10).

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use nc_identity::DeviceKeyStore;
use nc_protocol::pb::v1::{
    CompleteRequest, CompleteResponse, ErrorResponse, HeartbeatRequest, JobStatusResponse,
    LeaseFailRequest, LeaseFailResponse, LeaseResponse, RegisterRequest, SubmitJobRequest,
    SubmitJobResponse,
};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

/// Largest JSON response read from the coordinator.
const MAX_JSON_RESPONSE: u64 = 1024 * 1024;

/// Coordinator call failure. Never contains bodies, keys or tokens.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CoordError {
    /// Transport failure (connect, TLS, timeout, reset). Transient: retry with backoff.
    #[error("coordinator unreachable: {0}")]
    Network(String),
    /// The coordinator answered with an error status and machine code.
    #[error("coordinator error {status} ({code})")]
    Status {
        /// HTTP status.
        status: u16,
        /// `error.code` from the common error envelope (empty if absent).
        code: String,
    },
    /// The response did not decode.
    #[error("coordinator response did not decode")]
    Decode,
    /// The request could not be signed.
    #[error("request signing failed")]
    Sign,
}

impl CoordError {
    /// Whether retrying later may succeed.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Network(_) => true,
            Self::Status { status, .. } => *status >= 500 || *status == 429,
            _ => false,
        }
    }
}

/// Coordinator API used by the helper session and the requester commands.
pub trait Coordinator: Send + Sync {
    /// `POST /v1/devices/register` (idempotent).
    fn register(&self, req: &RegisterRequest) -> Result<(), CoordError>;
    /// `POST /v1/devices/heartbeat`.
    fn heartbeat(&self, req: &HeartbeatRequest) -> Result<(), CoordError>;
    /// `POST /v1/leases/request`; `None` when there is no work (404).
    fn lease(&self) -> Result<Option<LeaseResponse>, CoordError>;
    /// `POST /v1/leases/fail`.
    fn fail(&self, req: &LeaseFailRequest) -> Result<LeaseFailResponse, CoordError>;
    /// `POST /v1/leases/complete`.
    fn complete(&self, req: &CompleteRequest) -> Result<CompleteResponse, CoordError>;
    /// `GET {path}` from byte `offset` (a `Range: bytes={offset}-` request when nonzero). The reader yields
    /// the bytes from `offset` on; the caller caps how much it reads.
    fn get_blob(&self, path: &str, offset: u64) -> Result<Box<dyn Read + Send>, CoordError>;
    /// `PUT /v1/blobs/{sha256_hex}`.
    fn put_blob(&self, sha256_hex: &str, bytes: &[u8]) -> Result<(), CoordError>;
    /// `POST /v1/jobs`.
    fn submit(&self, req: &SubmitJobRequest) -> Result<SubmitJobResponse, CoordError>;
    /// `GET /v1/jobs/{id}`.
    fn job_status(&self, job_id: &str) -> Result<JobStatusResponse, CoordError>;
}

/// Exact request-signing bytes (proto/README.md "Exact HTTP request-signing bytes").
pub fn signing_string(method: &str, path: &str, ts: i64, nonce: &str, body: &[u8]) -> String {
    let digest = hex::encode(Sha256::digest(body));
    format!("NC-REQ-V1\n{method}\n{path}\n{ts}\n{nonce}\n{digest}")
}

/// Real HTTPS client.
pub struct HttpCoordinator {
    base: String,
    key: Arc<dyn DeviceKeyStore>,
    enroll: Option<String>,
    http: ureq::Agent,
}

impl std::fmt::Debug for HttpCoordinator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // No enrolment secret, no key.
        f.debug_struct("HttpCoordinator")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

fn random_nonce() -> Result<String, CoordError> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(|_| CoordError::Sign)?;
    Ok(hex::encode(b))
}

fn net_err(e: &ureq::Error) -> CoordError {
    // Only the error kind; ureq errors do not carry bodies.
    let kind = match e {
        ureq::Error::Timeout(_) => "timeout",
        ureq::Error::Io(_) => "io",
        ureq::Error::ConnectionFailed => "connect",
        ureq::Error::HostNotFound => "dns",
        ureq::Error::Tls(_) => "tls",
        _ => "transport",
    };
    CoordError::Network(kind.into())
}

/// Trust anchors from the OS store (Windows: system certificate store). Unreadable entries are skipped; an
/// empty store means every HTTPS connection fails (fail closed), never "trust anything".
fn os_roots() -> ureq::tls::RootCerts {
    let found = rustls_native_certs::load_native_certs();
    let certs: Vec<ureq::tls::Certificate<'static>> = found
        .certs
        .iter()
        .map(|c| ureq::tls::Certificate::from_der(c.as_ref()).to_owned())
        .collect();
    if certs.is_empty() {
        tracing::warn!("no OS trust anchors found; HTTPS to the coordinator will fail");
    }
    ureq::tls::RootCerts::Specific(Arc::new(certs))
}

impl HttpCoordinator {
    /// Client for `base_url` (e.g. `https://coordinator.example`), signing with `key`. `enroll` is the
    /// enrolment secret sent only on register.
    pub fn new(base_url: &str, key: Arc<dyn DeviceKeyStore>, enroll: Option<String>) -> Self {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .timeout_recv_body(Some(Duration::from_secs(300)))
            .max_redirects(0)
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .provider(ureq::tls::TlsProvider::Rustls)
                    .root_certs(os_roots())
                    .build(),
            )
            .build();
        Self {
            base: base_url.trim_end_matches('/').to_string(),
            key,
            enroll,
            http: ureq::Agent::new_with_config(config),
        }
    }

    fn headers(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> Result<Vec<(&'static str, String)>, CoordError> {
        let ts = chrono::Utc::now().timestamp();
        let nonce = random_nonce()?;
        let sig = self
            .key
            .sign(signing_string(method, path, ts, &nonce, body).as_bytes())
            .map_err(|_| CoordError::Sign)?;
        Ok(vec![
            ("x-nc-key", URL_SAFE_NO_PAD.encode(self.key.public_key())),
            ("x-nc-ts", ts.to_string()),
            ("x-nc-nonce", nonce),
            ("x-nc-sig", URL_SAFE_NO_PAD.encode(sig)),
        ])
    }

    fn send(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
        extra: &[(&'static str, String)],
    ) -> Result<ureq::http::Response<ureq::Body>, CoordError> {
        // Paths are always ours or a server-supplied relative path checked by the caller; never a full URL.
        if !path.starts_with("/v1/") || path.contains("..") || path.contains('?') {
            return Err(CoordError::Decode);
        }
        let url = format!("{}{path}", self.base);
        let mut headers = self.headers(method, path, body)?;
        headers.extend(extra.iter().cloned());
        let ct = if method == "PUT" {
            "application/octet-stream"
        } else {
            "application/json"
        };
        let r = match method {
            "GET" => {
                let mut rb = self.http.get(&url);
                for (k, v) in &headers {
                    rb = rb.header(*k, v);
                }
                rb.call()
            }
            "PUT" => {
                let mut rb = self.http.put(&url).header("content-type", ct);
                for (k, v) in &headers {
                    rb = rb.header(*k, v);
                }
                rb.send(body)
            }
            _ => {
                let mut rb = self.http.post(&url).header("content-type", ct);
                for (k, v) in &headers {
                    rb = rb.header(*k, v);
                }
                rb.send(body)
            }
        };
        r.map_err(|e| net_err(&e))
    }

    fn json<T: serde::de::DeserializeOwned>(
        resp: ureq::http::Response<ureq::Body>,
    ) -> Result<T, CoordError> {
        let status = resp.status().as_u16();
        let bytes = resp
            .into_body()
            .with_config()
            .limit(MAX_JSON_RESPONSE)
            .read_to_vec()
            .map_err(|e| net_err(&e))?;
        if !(200..300).contains(&status) {
            let code = serde_json::from_slice::<ErrorResponse>(&bytes)
                .ok()
                .and_then(|e| e.error)
                .map(|e| e.code)
                .unwrap_or_default();
            return Err(CoordError::Status { status, code });
        }
        serde_json::from_slice(&bytes).map_err(|_| CoordError::Decode)
    }

    fn post_json<Req: serde::Serialize, Resp: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        req: &Req,
        extra: &[(&'static str, String)],
    ) -> Result<Resp, CoordError> {
        let body = serde_json::to_vec(req).map_err(|_| CoordError::Decode)?;
        Self::json(self.send("POST", path, &body, extra)?)
    }
}

/// Parse `Content-Range: bytes a-b/n` and return `a`.
pub(crate) fn content_range_start(v: &str) -> Option<u64> {
    let rest = v.strip_prefix("bytes ")?;
    let (range, _) = rest.split_once('/')?;
    let (a, _) = range.split_once('-')?;
    a.parse().ok()
}

impl Coordinator for HttpCoordinator {
    fn register(&self, req: &RegisterRequest) -> Result<(), CoordError> {
        let extra: Vec<(&'static str, String)> = self
            .enroll
            .iter()
            .map(|s| ("x-nc-enroll", s.clone()))
            .collect();
        let _: serde_json::Value = self.post_json("/v1/devices/register", req, &extra)?;
        Ok(())
    }
    fn heartbeat(&self, req: &HeartbeatRequest) -> Result<(), CoordError> {
        let _: serde_json::Value = self.post_json("/v1/devices/heartbeat", req, &[])?;
        Ok(())
    }
    fn lease(&self) -> Result<Option<LeaseResponse>, CoordError> {
        match self.post_json("/v1/leases/request", &serde_json::json!({}), &[]) {
            Ok(l) => Ok(Some(l)),
            Err(CoordError::Status { status: 404, .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }
    fn fail(&self, req: &LeaseFailRequest) -> Result<LeaseFailResponse, CoordError> {
        self.post_json("/v1/leases/fail", req, &[])
    }
    fn complete(&self, req: &CompleteRequest) -> Result<CompleteResponse, CoordError> {
        self.post_json("/v1/leases/complete", req, &[])
    }
    fn get_blob(&self, path: &str, offset: u64) -> Result<Box<dyn Read + Send>, CoordError> {
        let extra = if offset > 0 {
            vec![("range", format!("bytes={offset}-"))]
        } else {
            vec![]
        };
        let resp = self.send("GET", path, &[], &extra)?;
        let status = resp.status().as_u16();
        let ok = match status {
            200 => offset == 0,
            206 => {
                resp.headers()
                    .get("content-range")
                    .and_then(|v| v.to_str().ok())
                    .and_then(content_range_start)
                    == Some(offset)
            }
            _ => false,
        };
        if !ok {
            if (200..300).contains(&status) {
                return Err(CoordError::Decode);
            }
            return Err(Self::json::<serde_json::Value>(resp)
                .err()
                .unwrap_or(CoordError::Decode));
        }
        Ok(Box::new(resp.into_body().into_reader()))
    }
    fn put_blob(&self, sha256_hex: &str, bytes: &[u8]) -> Result<(), CoordError> {
        let resp = self.send("PUT", &format!("/v1/blobs/{sha256_hex}"), bytes, &[])?;
        let _: serde_json::Value = match Self::json(resp) {
            Ok(v) => v,
            // Some servers answer an empty 200; only the status matters here.
            Err(CoordError::Decode) => serde_json::Value::Null,
            Err(e) => return Err(e),
        };
        Ok(())
    }
    fn submit(&self, req: &SubmitJobRequest) -> Result<SubmitJobResponse, CoordError> {
        self.post_json("/v1/jobs", req, &[])
    }
    fn job_status(&self, job_id: &str) -> Result<JobStatusResponse, CoordError> {
        if !job_id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
            return Err(CoordError::Decode);
        }
        Self::json(self.send("GET", &format!("/v1/jobs/{job_id}"), &[], &[])?)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn signing_string_matches_readme_vector() {
        let s = signing_string(
            "POST",
            "/v1/leases/request",
            1_700_000_000,
            "abcdefghijklmnop",
            b"{}",
        );
        assert_eq!(
            s,
            "NC-REQ-V1\nPOST\n/v1/leases/request\n1700000000\nabcdefghijklmnop\n\
             44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
        );
        assert!(!s.contains('\r'));
    }

    #[test]
    fn content_range_parse() {
        assert_eq!(content_range_start("bytes 2-5/6"), Some(2));
        assert_eq!(content_range_start("bytes */6"), None);
        assert_eq!(content_range_start("items 1-2/3"), None);
    }
}
