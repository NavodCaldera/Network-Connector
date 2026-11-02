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
        self.request_nonce(method, path, body, extra, &nonce).await
    }
    async fn request_nonce(
        &self,
        method: &str,
        path: &str,
        body: Vec<u8>,
        extra: Option<(&str, &str)>,
        nonce: &str,
    ) -> reqwest::Response {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let sig = self
            .sk
            .sign(signing_string(method, path, ts, nonce, &body).as_bytes());
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
    async fn renew(&self, l: &Value) -> (u16, Value) {
        self.post(
            "/v1/leases/renew",
            json!({"lease_id":l["lease_id"],"attempt_id":l["attempt_id"]}),
        )
        .await
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
    let mut helper = Arc::new(Dev::new(&base));
    helper.register(&enroll).await;
    let mut other = Arc::new(Dev::new(&base));
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
    let foreign_job = other.submit(other.manifest(1)).await;
    assert_eq!(
        other.status(&foreign_job).await["tasks"][0]["state"],
        "queued"
    );
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
    for i in 0..20 {
        let h = if i % 2 == 0 {
            helper.clone()
        } else {
            other.clone()
        };
        calls.spawn(async move { (i % 2, h.post("/v1/leases/request", json!({})).await) });
    }
    let mut leases = vec![];
    while let Some(r) = calls.join_next().await {
        let (device, (s, v)) = r.unwrap();
        if s == 200 {
            leases.push((device, v))
        } else {
            assert_eq!(s, 404, "{v}")
        }
    }
    assert_eq!(leases.len(), 1);
    if leases[0].0 == 1 {
        std::mem::swap(&mut helper, &mut other);
    }
    let lease = &leases[0].1;
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
    // Dispute resolution and tariff releases: dispute_and_tariff_over_http.
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

    // Lease renewal (1.N1).
    let idle = json!({"status":"idle"});
    assert_eq!(
        helper.post("/v1/devices/heartbeat", idle.clone()).await.0,
        200
    );
    let id = owner.submit(owner.manifest(1)).await;
    let l = helper.lease().await;
    let (s, v) = helper.renew(&l).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["lease_id"], l["lease_id"]);
    assert!(v["expires_at"].as_u64().unwrap() >= l["expires_at"].as_u64().unwrap());
    assert_eq!(v["capped"], false);
    let (s, v) = other.renew(&l).await;
    assert_eq!(s, 404, "another device cannot renew: {v}");
    let stale = json!({"lease_id":l["lease_id"],"attempt_id":uuid::Uuid::new_v4().to_string()});
    let (s, v) = helper.post("/v1/leases/renew", stale).await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (409, Some("attempt_mismatch"))
    );
    let extra = json!({"lease_id":l["lease_id"],"attempt_id":l["attempt_id"],"x":1});
    assert_eq!(
        helper.post("/v1/leases/renew", extra).await.0,
        400,
        "unknown fields rejected"
    );
    assert_eq!(helper.complete(&l, b"renewed").await.0, 200);
    assert_eq!(owner.status(&id).await["tasks"][0]["state"], "committed");
    let (s, v) = helper.renew(&l).await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (409, Some("lease_expired")),
        "completed lease"
    );
    println!("renew ok / other device 404 / stale attempt 409 / completed 409: passed");

    // A renewed lease outlives the original TTL and blocks reclaim; a lapsed lease cannot be
    // renewed; renewals racing an expiry reclaim never leave two live holders.
    let kept = owner.submit(owner.manifest(1)).await;
    let keep = helper.lease().await;
    owner.submit(owner.manifest(1)).await;
    let lapse = helper.lease().await;
    let race_id = owner.submit(owner.manifest(1)).await;
    let race = helper.lease().await;
    for _ in 0..4 {
        tokio::time::sleep(std::time::Duration::from_secs(14)).await;
        let (s, v) = helper.renew(&keep).await;
        assert_eq!(s, 200, "{v}");
    }
    assert_eq!(other.post("/v1/devices/heartbeat", idle).await.0, 200);
    // Storm from 150 ms before to ~330 ms after the race lease expiry: helper renews it
    // (never renewed before), other tries to reclaim it. Either side may win; never both.
    let unix_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let start_ms = race["expires_at"].as_u64().unwrap() * 1000 - 150;
    tokio::time::sleep(std::time::Duration::from_millis(
        start_ms.saturating_sub(unix_ms),
    ))
    .await;
    let mut calls = tokio::task::JoinSet::new();
    for i in 0..48u64 {
        let (h, o, race) = (helper.clone(), other.clone(), race.clone());
        calls.spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20 * (i / 2))).await;
            if i % 2 == 0 {
                (0, h.renew(&race).await)
            } else {
                (1, o.post("/v1/leases/request", json!({})).await)
            }
        });
    }
    let mut stolen = vec![];
    let mut renewed = 0;
    while let Some(r) = calls.join_next().await {
        let (who, (s, v)) = r.unwrap();
        match (who, s) {
            (0, 200) => renewed += 1,
            (0, 409) => assert_eq!(v["error"]["code"], "lease_expired"),
            (1, 200) => stolen.push(v),
            (1, 404) => {}
            unexpected => panic!("unexpected {unexpected:?} {v}"),
        }
    }

    assert!(
        stolen.iter().all(|v| v["task_id"] != kept.as_str()),
        "renewed lease is never reclaimed"
    );
    let (s, v) = helper.renew(&lapse).await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (409, Some("lease_expired")),
        "renew after expiry"
    );
    let race_stolen = stolen.iter().find(|v| v["task_id"] == race_id.as_str());
    let (s, _) = helper.renew(&race).await;
    match race_stolen {
        Some(new) => {
            assert_eq!(s, 409, "old holder cannot renew after reclaim");
            assert_eq!(other.renew(new).await.0, 200);
        }
        None => assert_eq!(s, 200, "renewals kept the race lease alive"),
    }
    assert_eq!(
        helper.complete(&keep, b"kept").await.0,
        200,
        "renewed lease commits after the original TTL"
    );
    assert_eq!(owner.status(&kept).await["tasks"][0]["state"], "committed");
    println!(
        "renew past TTL / renew after expiry 409 / renew vs reclaim race ({} renewals, race lease {}): passed",
        renewed,
        if race_stolen.is_some() { "reclaimed" } else { "kept" }
    );

    device_lifecycle(&base, &enroll, &owner, &helper).await;
}

/// 1.L4 dispute resolution and 1.L2 tariff-window releases over real signed HTTP. Run by
/// infra/verify.ps1 and as fleet-e2e S6 (after the real helpers are stopped, so they cannot take
/// these synthetic tasks).
#[tokio::test]
#[ignore = "needs a fresh infra Docker stack with PostgreSQL; run infra/verify.ps1 or verify.sh"]
async fn dispute_and_tariff_over_http() {
    let base = std::env::var("NC_TEST_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".into());
    let enroll = std::env::var("NC_TEST_ENROLL").expect("NC_TEST_ENROLL");
    let owner = Dev::new(&base);
    owner.register(&enroll).await;
    owner.upload(b"module").await;
    owner.upload(b"input").await;
    let helper = Dev::new(&base);
    helper.register(&enroll).await;
    let other = Dev::new(&base);
    other.register(&enroll).await;
    let third = Dev::new(&base);
    third.register(&enroll).await;
    // (second replica, third replica, final state, committed output)
    type Case = (
        &'static [u8],
        Option<&'static [u8]>,
        &'static str,
        Option<&'static [u8]>,
    );
    let cases: [Case; 4] = [
        (b"a", None, "committed", Some(b"a")),
        (b"b", Some(b"a"), "committed", Some(b"a")),
        (b"b", Some(b"b"), "committed", Some(b"b")),
        (b"b", Some(b"c"), "disputed", None),
    ];
    for (second, tie, end, out) in cases {
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
        let (s, v) = other.complete(&l, second).await;
        assert_eq!(s, 200, "{v}");
        let v = if let Some(tie) = tie {
            assert_eq!(v["state"], "queued", "mismatch schedules a third helper");
            assert_eq!(owner.status(&id).await["tasks"][0]["state"], "queued");
            assert_eq!(helper.post("/v1/leases/request", json!({})).await.0, 404);
            assert_eq!(other.post("/v1/leases/request", json!({})).await.0, 404);
            let l = third.lease().await;
            assert_eq!(l["task_id"], id.as_str());
            let (s, v) = third.complete(&l, tie).await;
            assert_eq!(s, 200, "{v}");
            v
        } else {
            v
        };
        assert_eq!(v["state"], end);
        let st = owner.status(&id).await;
        assert_eq!(st["tasks"][0]["state"], end);
        match out {
            Some(want) => {
                let path = st["tasks"][0]["output"]["path"].as_str().unwrap();
                let got = owner.request("GET", path, vec![], None).await;
                assert_eq!(got.bytes().await.unwrap().as_ref(), want);
            }
            None => assert!(st["tasks"][0]["output"].is_null()),
        }
    }
    println!("2-of-2 match / mismatch -> third helper -> 2-of-3 commit majority / no majority disputed: passed");
    // 1.L2: TARIFF_WINDOW failures do not count toward the six-attempt cap.
    let id = owner.submit(owner.manifest(1)).await;
    for _ in 0..8 {
        let l = third.lease().await;
        let (s, v) = third
            .post(
                "/v1/leases/fail",
                json!({"lease_id":l["lease_id"],"attempt_id":l["attempt_id"],"reason":2,"code":"TARIFF_WINDOW"}),
            )
            .await;
        assert_eq!(s, 200, "{v}");
        assert_eq!(v["state"], "queued");
    }
    for _ in 0..5 {
        let l = third.lease().await;
        let (s, v) = third
            .post(
                "/v1/leases/fail",
                json!({"lease_id":l["lease_id"],"attempt_id":l["attempt_id"],"reason":9}),
            )
            .await;
        assert_eq!((s, v["state"].as_str()), (200, Some("queued")), "{v}");
    }
    let l = third.lease().await;
    assert_eq!(third.complete(&l, b"late-window").await.0, 200);
    assert_eq!(owner.status(&id).await["tasks"][0]["state"], "committed");
    println!("8 tariff-window releases + 5 counted failures still leave the 6th attempt: passed");
}

/// 1.N2: who-am-I, secret-free re-register for known keys, deregister releasing leases, replay kept.
async fn device_lifecycle(base: &str, enroll: &str, owner: &Dev, helper: &Dev) {
    let reg = |name: &str, platform: &str| {
        serde_json::to_vec(&json!({"name": name, "platform": platform})).unwrap()
    };
    async fn json_of(r: reqwest::Response) -> (u16, Value) {
        let s = r.status().as_u16();
        (s, r.json().await.unwrap())
    }
    let unknown = Dev::new(base);
    let (s, v) = unknown.post("/v1/devices/me", json!({})).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["registered"], false);
    assert_eq!(v["device_id"], "");
    let r = unknown
        .request("POST", "/v1/devices/register", reg("x", "linux"), None)
        .await;
    assert_eq!(r.status(), 401, "unknown key without the secret");
    let r = unknown
        .request(
            "POST",
            "/v1/devices/register",
            reg("x", "linux"),
            Some(("x-nc-enroll", "wrong-secret-value")),
        )
        .await;
    assert_eq!(r.status(), 401, "unknown key with a wrong secret");
    assert_eq!(
        unknown.post("/v1/devices/me", json!({})).await.1["registered"],
        false
    );
    assert_eq!(
        unknown.post("/v1/devices/deregister", json!({})).await.0,
        401
    );
    let (s, _) = unknown.post("/v1/devices/me", json!({"x": 1})).await;
    assert_eq!(s, 400, "unknown fields rejected");
    println!("whoami unknown / register without secret 401: passed");

    let d = Dev::new(base);
    d.register(enroll).await;
    let (s, me) = d.post("/v1/devices/me", json!({})).await;
    assert_eq!(s, 200, "{me}");
    assert_eq!(me["registered"], true);
    assert_eq!(me["name"], "job-test");
    assert_eq!(me["platform"], "linux");
    let id = me["device_id"].as_str().unwrap().to_owned();
    let (s, v) = json_of(
        d.request(
            "POST",
            "/v1/devices/register",
            reg("renamed", "windows"),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(s, 200, "known key re-registers without the secret: {v}");
    assert_eq!(v["device_id"], id.as_str());
    assert_eq!(v["created"], false);
    let (_, again) = d.post("/v1/devices/me", json!({})).await;
    assert_eq!(again, me, "re-register changes nothing");
    println!("whoami known / idempotent re-register without secret: passed");

    // Replay protection still applies to the new endpoints and to secret-free register.
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    for (path, body) in [
        ("/v1/devices/me", b"{}".to_vec()),
        ("/v1/devices/register", reg("job-test", "linux")),
    ] {
        let nonce = format!("{nonce}{}", path.len());
        let r = d
            .request_nonce("POST", path, body.clone(), None, &nonce)
            .await;
        assert_eq!(r.status(), 200, "{path}");
        let r = d.request_nonce("POST", path, body, None, &nonce).await;
        assert_eq!(r.status(), 401, "replayed {path}");
    }
    println!("replay of whoami / register rejected: passed");

    // Deregister while holding a lease puts the task back in the queue.
    let job = owner.submit(owner.manifest(1)).await;
    let mut held = None;
    for _ in 0..8 {
        let l = d.lease().await;
        if l["task_id"] == job.as_str() {
            held = Some(l);
            break;
        }
        // Leftover reclaimable task from the earlier section: finish it so the queue drains.
        assert_eq!(d.complete(&l, b"drain").await.0, 200);
    }
    let held = held.expect("lease on the new job");
    assert_eq!(owner.status(&job).await["tasks"][0]["state"], "leased");
    let deregister_nonce = uuid::Uuid::new_v4().simple().to_string();
    let (s, v) = json_of(
        d.request_nonce(
            "POST",
            "/v1/devices/deregister",
            b"{}".to_vec(),
            None,
            &deregister_nonce,
        )
        .await,
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["device_id"], id.as_str());
    assert_eq!(v["released_leases"], 1);
    assert_eq!(owner.status(&job).await["tasks"][0]["state"], "queued");
    let r = d
        .request_nonce(
            "POST",
            "/v1/devices/deregister",
            b"{}".to_vec(),
            None,
            &deregister_nonce,
        )
        .await;
    assert_eq!(r.status(), 401, "replayed deregister");
    for (path, body) in [
        ("/v1/devices/heartbeat", json!({"status": "idle"})),
        ("/v1/leases/request", json!({})),
        (
            "/v1/leases/renew",
            json!({"lease_id": held["lease_id"], "attempt_id": held["attempt_id"]}),
        ),
        ("/v1/devices/deregister", json!({})),
    ] {
        let (s, v) = d.post(path, body).await;
        assert_eq!(s, 401, "{path} after deregister: {v}");
    }
    let r = d
        .request(
            "POST",
            "/v1/devices/register",
            reg("job-test", "linux"),
            None,
        )
        .await;
    assert_eq!(r.status(), 401, "deregistered key needs the secret again");
    assert_eq!(
        d.post("/v1/devices/me", json!({})).await.1["registered"],
        false
    );
    let fail = json!({"lease_id": held["lease_id"], "attempt_id": held["attempt_id"], "reason": 9});
    assert_eq!(d.post("/v1/leases/fail", fail).await.0, 401);
    // Another device picks the released task up and commits it.
    assert_eq!(
        helper
            .post("/v1/devices/heartbeat", json!({"status": "idle"}))
            .await
            .0,
        200
    );
    let mut done = false;
    for _ in 0..8 {
        let l = helper.lease().await;
        assert_eq!(helper.complete(&l, b"after-reset").await.0, 200);
        if l["task_id"] == job.as_str() {
            done = true;
            break;
        }
    }
    assert!(done, "released task leased again");
    assert_eq!(owner.status(&job).await["tasks"][0]["state"], "committed");
    println!("deregister releases lease -> queued -> other device commits; key then 401: passed");
}

impl Dev {
    /// 1.L5 chunk PUT: signs `signed` and sends `actual` (they differ only in tamper tests).
    async fn chunk_put(&self, path: &str, signed: &[u8], actual: Vec<u8>) -> reqwest::Response {
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let sig = self
            .sk
            .sign(signing_string("PUT", path, ts, &nonce, signed).as_bytes());
        self.http
            .put(format!("{}{path}", self.base))
            .header(
                "x-nc-key",
                URL_SAFE_NO_PAD.encode(self.sk.verifying_key().to_bytes()),
            )
            .header("x-nc-ts", ts.to_string())
            .header("x-nc-nonce", nonce)
            .header("x-nc-sig", URL_SAFE_NO_PAD.encode(sig.to_bytes()))
            .header("x-nc-chunk-sha256", hex::encode(Sha256::digest(signed)))
            .header("content-type", "application/octet-stream")
            .body(actual)
            .send()
            .await
            .unwrap()
    }
    async fn chunk(&self, blob: &[u8], offset: usize, len: usize) -> reqwest::Response {
        let path = format!(
            "/v1/blobs/{}/upload/{}/{offset}",
            hex::encode(Sha256::digest(blob)),
            blob.len()
        );
        let part = &blob[offset..offset + len];
        self.chunk_put(&path, part, part.to_vec()).await
    }
    async fn held(&self, blob: &[u8]) -> u64 {
        let path = format!(
            "/v1/blobs/{}/upload/{}",
            hex::encode(Sha256::digest(blob)),
            blob.len()
        );
        let r = self.request("GET", &path, vec![], None).await;
        assert_eq!(r.status(), 200);
        offset_of(&r)
    }
}

fn offset_of(r: &reqwest::Response) -> u64 {
    r.headers()["x-nc-upload-offset"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap()
}

#[tokio::test]
#[ignore = "needs a fresh infra Docker stack with PostgreSQL; run infra/verify.ps1 or verify.sh"]
async fn resumable_upload_over_http() {
    let base = std::env::var("NC_TEST_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".into());
    let enroll = std::env::var("NC_TEST_ENROLL").expect("NC_TEST_ENROLL");
    let owner = Dev::new(&base);
    owner.register(&enroll).await;
    let other = Dev::new(&base);
    other.register(&enroll).await;
    const MIB: usize = 1024 * 1024;
    let big: Vec<u8> = (0..(9 * MIB + 123)).map(|i| (i * 31 % 251) as u8).collect();
    assert_eq!(owner.held(&big).await, 0);
    // Chunk larger than 4 MiB is refused.
    let r = owner.chunk(&big, 0, 4 * MIB + 1).await;
    assert_eq!(r.status(), 413);
    // Unsigned / wrongly signed chunk is refused before anything is stored.
    let hash = hex::encode(Sha256::digest(&big));
    let p0 = format!("/v1/blobs/{hash}/upload/{}/0", big.len());
    let r = other
        .request(
            "PUT",
            &p0,
            big[..MIB].to_vec(),
            Some(("x-nc-chunk-sha256", &hash)),
        )
        .await;
    assert_eq!(r.status(), 401);
    // Body that does not match the signed chunk hash is refused and nothing is held.
    let r = owner
        .chunk_put(&p0, &big[..MIB], big[1..=MIB].to_vec())
        .await;
    assert_eq!(r.status(), 400);
    assert_eq!(owner.held(&big).await, 0);
    // First chunk, then an "interruption": the client forgets its position.
    let r = owner.chunk(&big, 0, 4 * MIB).await;
    assert_eq!(r.status(), 200);
    assert_eq!(offset_of(&r), 4 * MIB as u64);
    assert!(r.headers().get("x-nc-upload-complete").is_none());
    // Replaying the same chunk is an offset conflict that reports where to resume.
    let r = owner.chunk(&big, 0, 4 * MIB).await;
    assert_eq!(r.status(), 409);
    assert_eq!(offset_of(&r), 4 * MIB as u64);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["error"]["code"], "upload_offset", "{v}");
    // Sessions belong to the device: another device sees nothing and cannot append.
    assert_eq!(other.held(&big).await, 0);
    assert_eq!(other.chunk(&big, 4 * MIB, MIB).await.status(), 409);
    // Resume from the server-reported offset.
    let at = owner.held(&big).await as usize;
    assert_eq!(at, 4 * MIB);
    let r = owner.chunk(&big, at, 4 * MIB).await;
    assert_eq!(offset_of(&r), 8 * MIB as u64);
    let r = owner.chunk(&big, 8 * MIB, big.len() - 8 * MIB).await;
    assert_eq!(r.status(), 200);
    assert_eq!(offset_of(&r), big.len() as u64);
    assert_eq!(r.headers()["x-nc-upload-complete"], "true");
    assert_eq!(owner.held(&big).await, 0, "session is gone after publish");

    // A chunk past the declared size is refused.
    let tiny = b"abc".to_vec();
    let th = hex::encode(Sha256::digest(&tiny));
    let r = owner
        .chunk_put(
            &format!("/v1/blobs/{th}/upload/3/0"),
            b"abcd",
            b"abcd".to_vec(),
        )
        .await;
    assert_eq!(r.status(), 413);
    // Final-blob hash mismatch discards the session.
    let r = owner
        .chunk_put(&format!("/v1/blobs/{th}/upload/3/0"), b"ab", b"ab".to_vec())
        .await;
    assert_eq!(offset_of(&r), 2);
    let r = owner
        .chunk_put(&format!("/v1/blobs/{th}/upload/3/2"), b"X", b"X".to_vec())
        .await;
    assert_eq!(r.status(), 400);
    assert_eq!(owner.held(&tiny).await, 0);
    // Bad paths.
    for p in [
        format!("/v1/blobs/{th}/upload/0/0"),
        format!("/v1/blobs/{th}/upload/03/0"),
        format!("/v1/blobs/{th}/upload/3/3"),
        format!("/v1/blobs/{th}/upload/{}/0", 16 * MIB + 1),
    ] {
        let s = owner.chunk_put(&p, b"a", b"a".to_vec()).await.status();
        assert!(s == 400 || s == 413, "{p}: {s}");
    }

    // A resumable upload proves upload ownership exactly like the single PUT: the job's module
    // (two chunks) and input (one chunk) arrive this way and the requester can then GET them.
    let id = owner.submit(owner.manifest(1)).await;
    assert_eq!(owner.chunk(b"module", 0, 3).await.status(), 200);
    assert_eq!(owner.held(b"module").await, 3);
    let r = owner.chunk(b"module", 3, 3).await;
    assert_eq!(r.headers()["x-nc-upload-complete"], "true");
    let r = owner.chunk(b"input", 0, 5).await;
    assert_eq!(r.headers()["x-nc-upload-complete"], "true");
    let module = hex::encode(Sha256::digest(b"module"));
    let r = owner
        .request("GET", &format!("/v1/blobs/{module}"), vec![], None)
        .await;
    assert_eq!(r.status(), 200);
    assert_eq!(&r.bytes().await.unwrap()[..], b"module");
    let _ = id;
}
