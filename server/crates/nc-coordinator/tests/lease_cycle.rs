//! Full lease cycle over HTTP against a running coordinator (the `infra/docker-compose.yml` stack).
//!
//! Ignored by default because it needs the stack. Run:
//! ```text
//! docker compose -f infra/docker-compose.yml up -d --build
//! NC_TEST_URL=http://127.0.0.1:8080 NC_TEST_ENROLL=<secret from infra/.env> \
//!   cargo test -p nc-coordinator --test lease_cycle -- --ignored --nocapture
//! ```
#![allow(clippy::unwrap_used, clippy::expect_used)]

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use nc_coordinator::auth::signing_string;
use rand::RngCore;
use serde_json::{json, Value};

struct Dev {
    base: String,
    sk: SigningKey,
    http: reqwest::Client,
}

fn nonce() -> String {
    let mut b = [0u8; 18];
    rand::rngs::OsRng.fill_bytes(&mut b);
    URL_SAFE_NO_PAD.encode(b)
}

impl Dev {
    fn new(base: &str) -> Self {
        Self {
            base: base.into(),
            sk: SigningKey::generate(&mut rand::rngs::OsRng),
            http: reqwest::Client::new(),
        }
    }

    async fn post_with(
        &self,
        path: &str,
        body: &Value,
        nonce: &str,
        enroll: Option<&str>,
    ) -> (u16, Value) {
        let body = serde_json::to_vec(body).unwrap();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let sig = self
            .sk
            .sign(signing_string("POST", path, ts, nonce, &body).as_bytes());
        let mut rb = self
            .http
            .post(format!("{}{path}", self.base))
            .header("content-type", "application/json")
            .header(
                "x-nc-key",
                URL_SAFE_NO_PAD.encode(self.sk.verifying_key().to_bytes()),
            )
            .header("x-nc-ts", ts.to_string())
            .header("x-nc-nonce", nonce)
            .header("x-nc-sig", URL_SAFE_NO_PAD.encode(sig.to_bytes()))
            .body(body);
        if let Some(e) = enroll {
            rb = rb.header("x-nc-enroll", e);
        }
        let res = rb.send().await.unwrap();
        let status = res.status().as_u16();
        (status, res.json().await.unwrap())
    }

    async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        self.post_with(path, &body, &nonce(), None).await
    }
}

#[tokio::test]
#[ignore = "needs the docker compose stack; see file header"]
async fn dummy_lease_cycle_over_http() {
    let base = std::env::var("NC_TEST_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".into());
    let enroll = std::env::var("NC_TEST_ENROLL").expect("NC_TEST_ENROLL");
    let dev = Dev::new(&base);

    // Unregistered device is refused.
    let (s, v) = dev
        .post("/v1/devices/heartbeat", json!({"status":"idle"}))
        .await;
    assert_eq!(s, 401, "{v}");

    // Register (idempotent).
    let reg = json!({"name":"test-pc","platform":"windows"});
    let (s, v) = dev
        .post_with("/v1/devices/register", &reg, &nonce(), Some(&enroll))
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["created"], true);
    let device_id = v["device_id"].as_str().unwrap().to_owned();
    let (s, v) = dev
        .post_with("/v1/devices/register", &reg, &nonce(), Some(&enroll))
        .await;
    assert_eq!(s, 200);
    assert_eq!(v["created"], false);
    assert_eq!(v["device_id"], device_id.as_str());
    println!("registered device {device_id}");

    // Heartbeat; replaying the same signed request is refused.
    let n = nonce();
    let (s, v) = dev
        .post_with("/v1/devices/heartbeat", &json!({"status":"idle"}), &n, None)
        .await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = dev
        .post_with("/v1/devices/heartbeat", &json!({"status":"idle"}), &n, None)
        .await;
    assert_eq!(s, 401, "replay must fail: {v}");

    // Schema violation.
    let (s, v) = dev
        .post(
            "/v1/devices/heartbeat",
            json!({"status":"idle","extra":true}),
        )
        .await;
    assert_eq!(s, 400, "{v}");
    assert_eq!(v["error"]["code"], "bad_request");

    // Lease -> complete -> duplicate complete.
    let (s, lease) = dev.post("/v1/leases/request", json!({})).await;
    assert_eq!(s, 200, "{lease}");
    println!("lease {lease}");
    let (lease_id, attempt_id) = (lease["lease_id"].clone(), lease["attempt_id"].clone());

    let (s, v) = dev
        .post(
            "/v1/leases/complete",
            json!({"lease_id": lease_id, "attempt_id": "00000000-0000-4000-8000-000000000000", "result":"x"}),
        )
        .await;
    assert_eq!(s, 409, "wrong attempt: {v}");
    assert_eq!(v["error"]["code"], "attempt_mismatch");

    let done = json!({"lease_id": lease_id, "attempt_id": attempt_id, "result":"done"});
    let (s, v) = dev.post("/v1/leases/complete", done.clone()).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["state"], "committed");
    assert_eq!(v["duplicate"], false);
    let (s, v) = dev.post("/v1/leases/complete", done).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        v["duplicate"], true,
        "second commit of same attempt is a no-op"
    );
    println!("committed task {}", v["task_id"]);

    // Another device cannot complete our lease.
    let other = Dev::new(&base);
    let (s, _) = other
        .post_with("/v1/devices/register", &reg, &nonce(), Some(&enroll))
        .await;
    assert_eq!(s, 200);
    let (s, v) = other
        .post(
            "/v1/leases/complete",
            json!({"lease_id": lease_id, "attempt_id": attempt_id, "result":"steal"}),
        )
        .await;
    assert_eq!(s, 404, "{v}");

    // A second lease gets a different task (first one is committed).
    let (s, l2) = dev.post("/v1/leases/request", json!({})).await;
    assert_eq!(s, 200);
    assert_ne!(l2["task_id"], lease["task_id"]);

    // Relay token for our peer id (if the stack enables relay tokens).
    let (s, v) = dev
        .post(
            "/v1/relay/token",
            json!({"peer_id":"12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN"}),
        )
        .await;
    assert!(s == 200 || s == 404, "{v}");
    if s == 200 {
        assert!(v["token"].as_str().unwrap().starts_with("v1."));
        println!("relay token issued, expires_at {}", v["expires_at"]);
    }
}
