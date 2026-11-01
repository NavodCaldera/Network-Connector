//! Real signed HTTP/PostgreSQL cycle; infra/verify scripts run this against a fresh Docker stack.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ed25519_dalek::{Signer, SigningKey};
use nc_coordinator::auth::signing_string;
use nc_protocol::{jobs::DOMAIN_JOB, pb::v1::*};
use prost::Message;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;

struct Dev {
    base: String,
    sk: SigningKey,
    http: reqwest::Client,
}
impl Dev {
    fn new(base: &str) -> Self {
        Self {
            base: base.into(),
            sk: SigningKey::generate(&mut rand::rngs::OsRng),
            http: reqwest::Client::new(),
        }
    }
    async fn request(
        &self,
        method: &str,
        path: &str,
        body: Vec<u8>,
        extra: Option<(&str, &str)>,
    ) -> reqwest::Response {
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let sig = self
            .sk
            .sign(signing_string(method, path, ts, &nonce, &body).as_bytes());
        let mut r = self
            .http
            .request(method.parse().unwrap(), format!("{}{path}", self.base))
            .header(
                "x-nc-key",
                URL_SAFE_NO_PAD.encode(self.sk.verifying_key().to_bytes()),
            )
            .header("x-nc-ts", ts.to_string())
            .header("x-nc-nonce", nonce)
            .header("x-nc-sig", URL_SAFE_NO_PAD.encode(sig.to_bytes()))
            .header(
                "content-type",
                if method == "PUT" {
                    "application/octet-stream"
                } else {
                    "application/json"
                },
            )
            .body(body);
        if let Some((k, v)) = extra {
            r = r.header(k, v)
        }
        r.send().await.unwrap()
    }
    async fn raw_upload(
        &self,
        expected: &[u8],
        actual: Vec<u8>,
        chunked: bool,
    ) -> reqwest::Response {
        let hash = hex::encode(Sha256::digest(expected));
        let path = format!("/v1/blobs/{hash}");
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let sig = self
            .sk
            .sign(signing_string("PUT", &path, ts, &nonce, expected).as_bytes());
        let body = if chunked {
            reqwest::Body::wrap_stream(tokio_util::io::ReaderStream::new(std::io::Cursor::new(
                actual,
            )))
        } else {
            reqwest::Body::from(actual)
        };
        self.http
            .put(format!("{}{path}", self.base))
            .header(
                "x-nc-key",
                URL_SAFE_NO_PAD.encode(self.sk.verifying_key().to_bytes()),
            )
            .header("x-nc-ts", ts.to_string())
            .header("x-nc-nonce", nonce)
            .header("x-nc-sig", URL_SAFE_NO_PAD.encode(sig.to_bytes()))
            .body(body)
            .send()
            .await
            .unwrap()
    }
    async fn post(&self, path: &str, v: Value) -> (u16, Value) {
        let r = self
            .request("POST", path, serde_json::to_vec(&v).unwrap(), None)
            .await;
        let s = r.status().as_u16();
        (s, r.json().await.unwrap())
    }
    async fn register(&self, enroll: &str) {
        let r = self
            .request(
                "POST",
                "/v1/devices/register",
                serde_json::to_vec(&json!({"name":"job-test","platform":"linux"})).unwrap(),
                Some(("x-nc-enroll", enroll)),
            )
            .await;
        assert_eq!(r.status(), 200);
        assert_eq!(
            self.post("/v1/devices/heartbeat", json!({"status":"idle"}))
                .await
                .0,
            200
        );
    }
    fn manifest(&self, replicas: u32) -> SignedJobManifest {
        let mh = Sha256::digest(b"module").to_vec();
        let ih = Sha256::digest(b"input").to_vec();
        let mut msg = b"nc.v1.module\0".to_vec();
        msg.extend(&mh);
        let deadline = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
            + 300_000;
        let j = JobManifest {
            task: Some(TaskManifest {
                version: Some(ProtocolVersion { major: 1, minor: 0 }),
                task_id: uuid::Uuid::new_v4().as_bytes().to_vec(),
                profile: "cpu-core-v1".into(),
                module_sha256: mh.clone(),
                publisher_key: self.sk.verifying_key().to_bytes().to_vec(),
                module_signature: self.sk.sign(&msg).to_bytes().to_vec(),
                inputs: vec![InputRef {
                    sha256: ih.clone(),
                    size: 5,
                }],
                limits: Some(Limits {
                    fuel: 1000000,
                    wall_ms: 1000,
                    memory_bytes: 1048576,
                    stack_bytes: 65536,
                    output_bytes: 1024,
                    checkpoint_bytes: 0,
                }),
                verifier: Some(Verifier { kind: 1, replicas }),
                expires_at_unix_ms: deadline,
            }),
            total_input_size: 5,
            publisher_key_id: hex::encode(self.sk.verifying_key().to_bytes()),
            min_helpers: 1,
            input_transfers: vec![InputRef {
                sha256: ih,
                size: 5,
            }],
            module_transfer: Some(InputRef {
                sha256: mh,
                size: 6,
            }),
        };
        self.sign(&j)
    }
    fn sign(&self, j: &JobManifest) -> SignedJobManifest {
        let bytes = j.encode_to_vec();
        let mut msg = DOMAIN_JOB.as_bytes().to_vec();
        msg.push(0);
        msg.extend(&bytes);
        SignedJobManifest {
            manifest: bytes,
            signature: self.sk.sign(&msg).to_bytes().to_vec(),
        }
    }
    async fn submit(&self, s: SignedJobManifest) -> String {
        let (status, v) = self
            .post(
                "/v1/jobs",
                serde_json::to_value(SubmitJobRequest { manifest: Some(s) }).unwrap(),
            )
            .await;
        assert_eq!(status, 200, "{v}");
        v["job_id"].as_str().unwrap().into()
    }
    async fn upload(&self, b: &[u8]) -> String {
        let hash = hex::encode(Sha256::digest(b));
        let r = self
            .request("PUT", &format!("/v1/blobs/{hash}"), b.to_vec(), None)
            .await;
        assert_eq!(r.status(), 200);
        hash
    }
    async fn lease(&self) -> Value {
        let (s, v) = self.post("/v1/leases/request", json!({})).await;
        assert_eq!(s, 200, "{v}");
        assert_eq!(v["version"], 2);
        v
    }
    async fn complete(&self, l: &Value, b: &[u8]) -> (u16, Value) {
        let hash = self.upload(b).await;
        self.post("/v1/leases/complete",json!({"lease_id":l["lease_id"],"attempt_id":l["attempt_id"],"version":2,"output_sha256":hash,"output_size":b.len()})).await
    }
    async fn status(&self, id: &str) -> Value {
        let r = self
            .request("GET", &format!("/v1/jobs/{id}"), vec![], None)
            .await;
        assert_eq!(r.status(), 200);
        r.json().await.unwrap()
    }
}

#[tokio::test]
#[ignore = "needs a fresh infra Docker stack with PostgreSQL; run infra/verify.ps1 or verify.sh"]
async fn real_job_cycle_over_http() {
    let base = std::env::var("NC_TEST_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".into());
    let enroll = std::env::var("NC_TEST_ENROLL").expect("NC_TEST_ENROLL");
    let owner = Dev::new(&base);
    owner.register(&enroll).await;
    let helper = Arc::new(Dev::new(&base));
    helper.register(&enroll).await;
    let other = Dev::new(&base);
    other.register(&enroll).await;
    let good = owner.manifest(1);
    let mut bad = good.clone();
    bad.signature[0] ^= 1;
    assert_eq!(owner.post("/v1/jobs", json!({"manifest":bad})).await.0, 400);
    let mut bad = good.clone();
    bad.manifest = vec![0; 65537];
    assert_eq!(owner.post("/v1/jobs", json!({"manifest":bad})).await.0, 400);
    let job: JobManifest = nc_protocol::wire::decode_strict(&good.manifest).unwrap();
    for case in 0..5 {
        let mut j = job.clone();
        let t = j.task.as_mut().unwrap();
        match case {
            0 => {
                t.inputs = vec![
                    InputRef {
                        sha256: vec![1; 32],
                        size: 1
                    };
                    1025
                ];
            }
            1 => t.limits.as_mut().unwrap().fuel = u64::MAX,
            2 => t.limits.as_mut().unwrap().memory_bytes = u64::MAX,
            3 => t.limits.as_mut().unwrap().wall_ms = u64::MAX,
            _ => t.limits.as_mut().unwrap().output_bytes = u64::MAX,
        };
        assert_eq!(
            owner
                .post("/v1/jobs", json!({"manifest":owner.sign(&j)}))
                .await
                .0,
            400
        );
    }
    let id = owner.submit(good.clone()).await;
    assert_eq!(owner.submit(good).await, id);
    assert_eq!(
        helper.post("/v1/leases/request", json!({})).await.0,
        404,
        "wait for input uploads"
    );
    let module = owner.upload(b"module").await;
    owner.upload(b"input").await;
    owner.upload(b"module").await;
    let r = other
        .request("GET", &format!("/v1/blobs/{module}"), vec![], None)
        .await;
    assert_eq!(r.status(), 403);
    let r = owner
        .request(
            "PUT",
            &format!("/v1/blobs/{module}"),
            b"wrong".to_vec(),
            None,
        )
        .await;
    assert_eq!(
        r.status(),
        401,
        "signature body hash must equal path before streaming"
    );
    assert_eq!(
        owner
            .raw_upload(b"module", b"wrong".to_vec(), false)
            .await
            .status(),
        400,
        "authenticated hash mismatch"
    );
    let oversize = vec![7u8; 16 * 1024 * 1024 + 1];
    assert_eq!(
        owner
            .raw_upload(&oversize, oversize.clone(), true)
            .await
            .status(),
        413,
        "chunked stream is capped mid-upload"
    );
    let mut calls = tokio::task::JoinSet::new();
    for _ in 0..20 {
        let h = helper.clone();
        calls.spawn(async move { h.post("/v1/leases/request", json!({})).await });
    }
    let mut leases = vec![];
    while let Some(r) = calls.join_next().await {
        let (s, v) = r.unwrap();
        if s == 200 {
            leases.push(v)
        } else {
            assert_eq!(s, 404, "{v}")
        }
    }
    assert_eq!(leases.len(), 1);
    let lease = &leases[0];
    let r = helper
        .request(
            "GET",
            &format!("/v1/blobs/{module}"),
            vec![],
            Some(("range", "bytes=2-")),
        )
        .await;
    assert_eq!(r.status(), 206);
    assert_eq!(r.headers()["content-range"], "bytes 2-5/6");
    assert_eq!(r.bytes().await.unwrap().as_ref(), b"dule");
    assert_eq!(
        other
            .request("GET", &format!("/v1/jobs/{id}"), vec![], None)
            .await
            .status(),
        404
    );
    let (s, v) = helper.complete(lease, b"answer").await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["state"], "committed");
    let (s, v) = helper.complete(lease, b"answer").await;
    assert_eq!(s, 200);
    assert_eq!(v["duplicate"], true);
    assert_eq!(helper.complete(lease, b"changed").await.0, 409);
    let status = owner.status(&id).await;
    assert_eq!(status["tasks"][0]["state"], "committed");
    let path = status["tasks"][0]["output"]["path"].as_str().unwrap();
    assert_eq!(
        owner
            .request("GET", path, vec![], None)
            .await
            .bytes()
            .await
            .unwrap()
            .as_ref(),
        b"answer"
    );
    assert_eq!(
        helper
            .request("GET", &format!("/v1/blobs/{module}"), vec![], None)
            .await
            .status(),
        403,
        "completed lease no longer authorizes download"
    );
    println!("submit -> upload -> 20 concurrent lease calls -> Range -> complete -> status committed: passed");
    for mismatch in [false, true] {
        let id = owner.submit(owner.manifest(2)).await;
        let l = helper.lease().await;
        let (s, v) = helper.complete(&l, b"a").await;
        assert_eq!(s, 200, "{v}");
        assert_eq!(v["state"], "queued");
        assert_eq!(
            helper.post("/v1/leases/request", json!({})).await.0,
            404,
            "distinct devices required"
        );
        let l = other.lease().await;
        let (s, v) = other.complete(&l, if mismatch { b"b" } else { b"a" }).await;
        assert_eq!(s, 200, "{v}");
        assert_eq!(v["state"], if mismatch { "disputed" } else { "committed" });
        assert_eq!(
            owner.status(&id).await["tasks"][0]["state"],
            if mismatch { "disputed" } else { "committed" }
        );
    }
    println!("2-of-2 validation match/mismatch: passed");
    let id = owner.submit(owner.manifest(1)).await;
    let expired = helper.lease().await;
    // No database shortcuts: exercise the real lease clock over HTTP.
    tokio::time::sleep(std::time::Duration::from_secs(61)).await;
    assert_eq!(
        helper
            .post("/v1/devices/heartbeat", json!({"status":"idle"}))
            .await
            .0,
        200
    );
    let fresh = helper.lease().await;
    assert_ne!(fresh["attempt_id"], expired["attempt_id"]);
    assert_eq!(helper.complete(&expired, b"late").await.0, 409);
    let failed =
        json!({"lease_id":expired["lease_id"],"attempt_id":expired["attempt_id"],"reason":6});
    assert_eq!(helper.post("/v1/leases/fail", failed).await.0, 409);
    let failed = json!({"lease_id":fresh["lease_id"],"attempt_id":fresh["attempt_id"],"reason":1,"code":"OWNER_POLICY"});
    let (s, v) = helper.post("/v1/leases/fail", failed.clone()).await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = helper.post("/v1/leases/fail", failed).await;
    assert_eq!(s, 200);
    assert_eq!(v["duplicate"], true);
    let last = helper.lease().await;
    assert_eq!(helper.complete(&last, b"final").await.0, 200);
    assert_eq!(owner.status(&id).await["tasks"][0]["state"], "committed");
    println!("expiry -> new attempt; stale complete/fail; failure retry/idempotence: passed");
    let id = owner.submit(owner.manifest(1)).await;
    for n in 1..=6 {
        let l = helper.lease().await;
        let (s, v) = helper
            .post(
                "/v1/leases/fail",
                json!({"lease_id":l["lease_id"],"attempt_id":l["attempt_id"],"reason":9}),
            )
            .await;
        assert_eq!(s, 200, "{v}");
        assert_eq!(v["state"], if n == 6 { "failed" } else { "queued" });
    }
    assert_eq!(owner.status(&id).await["tasks"][0]["state"], "failed");
    println!("six-attempt limit: passed");
}
