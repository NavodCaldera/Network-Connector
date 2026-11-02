//! Signed device lifecycle calls the agent's coordinator client does not offer yet: `POST /v1/devices/me`
//! (does the coordinator still know this key?) and `POST /v1/devices/deregister` (app reset).
//!
//! Same signing bytes as nc-agent ([`nc_agent::coord::signing_string`]), same transport rules: rustls with
//! the OS trust store, no redirects, capped bodies. Only status codes and error codes are surfaced; no
//! headers, bodies or keys are logged.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use nc_agent::coord::{signing_string, CoordError};
use nc_identity::DeviceKeyStore;
use nc_protocol::pb::v1::{
    DeregisterRequest, DeregisterResponse, ErrorResponse, WhoAmIRequest, WhoAmIResponse,
};
use std::sync::Arc;
use std::time::Duration;

const MAX_BODY: u64 = 64 * 1024;

fn os_roots() -> ureq::tls::RootCerts {
    let certs: Vec<ureq::tls::Certificate<'static>> = rustls_native_certs::load_native_certs()
        .certs
        .iter()
        .map(|c| ureq::tls::Certificate::from_der(c.as_ref()).to_owned())
        .collect();
    // Empty store: every HTTPS call fails (fail closed), never "trust anything".
    ureq::tls::RootCerts::Specific(Arc::new(certs))
}

fn http() -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_global(Some(Duration::from_secs(20)))
            .max_redirects(0)
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .provider(ureq::tls::TlsProvider::Rustls)
                    .root_certs(os_roots())
                    .build(),
            )
            .build(),
    )
}

fn net_err(e: &ureq::Error) -> CoordError {
    let kind = match e {
        ureq::Error::Timeout(_) => "timeout",
        ureq::Error::ConnectionFailed => "connect",
        ureq::Error::HostNotFound => "dns",
        ureq::Error::Tls(_) => "tls",
        _ => "transport",
    };
    CoordError::Network(kind.into())
}

/// Signed `POST <base><path>` with a JSON body; `base` must already have passed `check_url`.
fn post<Req: serde::Serialize, Resp: serde::de::DeserializeOwned>(
    base: &str,
    key: &dyn DeviceKeyStore,
    path: &str,
    req: &Req,
) -> Result<Resp, CoordError> {
    let body = serde_json::to_vec(req).map_err(|_| CoordError::Decode)?;
    let ts = chrono::Utc::now().timestamp();
    let mut n = [0u8; 16];
    getrandom::fill(&mut n).map_err(|_| CoordError::Sign)?;
    let nonce = hex::encode(n);
    let sig = key
        .sign(signing_string("POST", path, ts, &nonce, &body).as_bytes())
        .map_err(|_| CoordError::Sign)?;
    let resp = http()
        .post(format!("{}{path}", base.trim_end_matches('/')))
        .header("content-type", "application/json")
        .header("x-nc-key", URL_SAFE_NO_PAD.encode(key.public_key()))
        .header("x-nc-ts", ts.to_string())
        .header("x-nc-nonce", nonce)
        .header("x-nc-sig", URL_SAFE_NO_PAD.encode(sig))
        .send(&body[..])
        .map_err(|e| net_err(&e))?;
    let status = resp.status().as_u16();
    let bytes = resp
        .into_body()
        .with_config()
        .limit(MAX_BODY)
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

/// `POST /v1/devices/me`.
pub(crate) fn whoami(base: &str, key: &dyn DeviceKeyStore) -> Result<WhoAmIResponse, CoordError> {
    post(base, key, "/v1/devices/me", &WhoAmIRequest {})
}

/// `POST /v1/devices/deregister`.
pub(crate) fn deregister(
    base: &str,
    key: &dyn DeviceKeyStore,
) -> Result<DeregisterResponse, CoordError> {
    post(base, key, "/v1/devices/deregister", &DeregisterRequest {})
}
