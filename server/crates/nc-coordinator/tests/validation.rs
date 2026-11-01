//! Request handling that is decided before any database access: error format, size cap, content type,
//! authentication. Uses a lazy pool that is never connected.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use nc_coordinator::{app, AppState, MAX_BODY};
use tower::ServiceExt;

fn router() -> axum::Router {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://nobody:nothing@127.0.0.1:1/none")
        .unwrap();
    app(AppState {
        pool,
        enroll_secret: "0123456789abcdef".into(),
        relay_key: None,
    })
}

async fn call(req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let res = router().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).expect("every response is JSON");
    (status, v)
}

fn assert_error(v: &serde_json::Value, code: &str) {
    assert_eq!(v["error"]["code"], code, "{v}");
    assert!(v["error"]["message"].is_string());
    assert_eq!(
        v.as_object().unwrap().len(),
        1,
        "only the error object: {v}"
    );
}

fn post(path: &str, ct: &str, body: Vec<u8>) -> Request<Body> {
    Request::post(path)
        .header("content-type", ct)
        .body(Body::from(body))
        .unwrap()
}

#[tokio::test]
async fn unknown_route_and_method_use_error_format() {
    let (s, v) = call(Request::get("/nope").body(Body::empty()).unwrap()).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_error(&v, "not_found");
    let (s, v) = call(
        Request::get("/v1/leases/request")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::METHOD_NOT_ALLOWED);
    assert_error(&v, "method_not_allowed");
}

#[tokio::test]
async fn health_is_open() {
    let (s, v) = call(Request::get("/healthz").body(Body::empty()).unwrap()).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["ok"], true);
}

#[tokio::test]
async fn oversized_body_is_rejected() {
    let (s, v) = call(post(
        "/v1/devices/heartbeat",
        "application/json",
        vec![b' '; MAX_BODY + 1],
    ))
    .await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
    assert_error(&v, "payload_too_large");
}

#[tokio::test]
async fn wrong_content_type_is_rejected() {
    let (s, v) = call(post("/v1/devices/heartbeat", "text/plain", b"{}".to_vec())).await;
    assert_eq!(s, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_error(&v, "unsupported_media_type");
}

#[tokio::test]
async fn unsigned_requests_are_rejected() {
    for path in [
        "/v1/devices/heartbeat",
        "/v1/leases/request",
        "/v1/leases/complete",
        "/v1/relay/token",
    ] {
        let (s, v) = call(post(path, "application/json", b"{}".to_vec())).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{path}");
        assert_error(&v, "unauthorized");
    }
}

#[tokio::test]
async fn register_needs_enrolment_secret() {
    let (s, v) = call(post(
        "/v1/devices/register",
        "application/json",
        b"{}".to_vec(),
    ))
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_error(&v, "unauthorized");
    let req = Request::post("/v1/devices/register")
        .header("content-type", "application/json")
        .header("x-nc-enroll", "wrong-secret-0000")
        .body(Body::from("{}"))
        .unwrap();
    let (s, _) = call(req).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[test]
fn schemas_reject_unknown_fields_and_bad_values() {
    use nc_coordinator::api::*;
    assert!(serde_json::from_str::<HeartbeatRequest>(r#"{"status":"idle","x":1}"#).is_err());
    let h: HeartbeatRequest = serde_json::from_str(r#"{"status":"sleeping"}"#).unwrap();
    assert!(h.validate().is_err());
    let r: RegisterRequest = serde_json::from_str(r#"{"name":"","platform":"windows"}"#).unwrap();
    assert!(r.validate().is_err());
    let r: RegisterRequest = serde_json::from_str(r#"{"name":"pc","platform":"plan9"}"#).unwrap();
    assert!(r.validate().is_err());
    let r: RegisterRequest = serde_json::from_str(r#"{"name":"pc","platform":"linux"}"#).unwrap();
    assert!(r.validate().is_ok());
    let t: RelayTokenRequest = serde_json::from_str(r#"{"peer_id":"../../etc"}"#).unwrap();
    assert!(t.validate().is_err());
}
