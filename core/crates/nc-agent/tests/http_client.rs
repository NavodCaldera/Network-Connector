#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The real HTTP client against a minimal loopback HTTP/1.1 server: signed headers verify, Range resume
//! requests carry the right header, a mismatched Content-Range is refused, 404 on lease means "no work".

mod common;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use common::key;
use ed25519_dalek::{Signature, VerifyingKey};
use nc_agent::coord::{signing_string, CoordError, Coordinator, HttpCoordinator};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

/// Serve `n` connections; `respond` maps a request to (status line, extra headers, body).
fn server(
    n: usize,
    respond: impl Fn(&Seen) -> (String, Vec<(String, String)>, Vec<u8>) + Send + 'static,
) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    std::thread::spawn(move || {
        for _ in 0..n {
            let (s, _) = l.accept().unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            let mut parts = line.split_whitespace();
            let method = parts.next().unwrap().to_string();
            let path = parts.next().unwrap().to_string();
            let mut headers = HashMap::new();
            loop {
                let mut h = String::new();
                r.read_line(&mut h).unwrap();
                let h = h.trim_end();
                if h.is_empty() {
                    break;
                }
                let (k, v) = h.split_once(':').unwrap();
                headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
            }
            let len: usize = headers
                .get("content-length")
                .map(|v| v.parse().unwrap())
                .unwrap_or(0);
            let mut body = vec![0u8; len];
            r.read_exact(&mut body).unwrap();
            let req = Seen {
                method,
                path,
                headers,
                body,
            };
            let (status, extra, out) = respond(&req);
            seen2.lock().unwrap().push(req);
            let mut w = s;
            let mut head = format!(
                "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n",
                out.len()
            );
            for (k, v) in extra {
                head.push_str(&format!("{k}: {v}\r\n"));
            }
            head.push_str("\r\n");
            w.write_all(head.as_bytes()).unwrap();
            w.write_all(&out).unwrap();
        }
    });
    (base, seen)
}

fn verify(seen: &Seen) {
    let pk: [u8; 32] = URL_SAFE_NO_PAD
        .decode(&seen.headers["x-nc-key"])
        .unwrap()
        .try_into()
        .unwrap();
    let sig: [u8; 64] = URL_SAFE_NO_PAD
        .decode(&seen.headers["x-nc-sig"])
        .unwrap()
        .try_into()
        .unwrap();
    let ts: i64 = seen.headers["x-nc-ts"].parse().unwrap();
    let nonce = &seen.headers["x-nc-nonce"];
    assert!((16..=64).contains(&nonce.len()));
    let msg = signing_string(&seen.method, &seen.path, ts, nonce, &seen.body);
    VerifyingKey::from_bytes(&pk)
        .unwrap()
        .verify_strict(msg.as_bytes(), &Signature::from_bytes(&sig))
        .expect("request signature verifies");
}

#[test]
fn signed_requests_range_and_errors() {
    let (base, seen) = server(5, |r| match (r.method.as_str(), r.path.as_str()) {
        ("POST", "/v1/devices/register") => (
            "200 OK".into(),
            vec![],
            br#"{"device_id":"00000000-0000-0000-0000-000000000001","created":true}"#.to_vec(),
        ),
        ("POST", "/v1/leases/request") => (
            "404 Not Found".into(),
            vec![],
            br#"{"error":{"code":"no_work","message":"none"}}"#.to_vec(),
        ),
        ("GET", _) if r.headers.get("range").map(String::as_str) == Some("bytes=2-") => (
            "206 Partial Content".into(),
            vec![("content-range".into(), "bytes 2-5/6".into())],
            b"dule".to_vec(),
        ),
        ("GET", _) if r.headers.contains_key("range") => (
            "206 Partial Content".into(),
            vec![("content-range".into(), "bytes 0-5/6".into())],
            b"module".to_vec(),
        ),
        _ => ("500 Internal Server Error".into(), vec![], b"{}".to_vec()),
    });
    let c = HttpCoordinator::new(&base, Arc::new(key(1)), Some("enroll".into()));
    c.register(&nc_protocol::pb::v1::RegisterRequest {
        name: "t".into(),
        platform: "windows".into(),
    })
    .unwrap();
    assert!(c.lease().unwrap().is_none(), "404 = no work");
    let mut body = Vec::new();
    c.get_blob("/v1/blobs/abc", 2)
        .unwrap()
        .read_to_end(&mut body)
        .unwrap();
    assert_eq!(body, b"dule");
    // Server ignored our range start: refused, never mis-appended.
    assert!(matches!(
        c.get_blob("/v1/blobs/abc", 3).err(),
        Some(CoordError::Decode)
    ));
    // Full URLs or traversal are never requested.
    assert!(c.get_blob("http://evil/x", 0).is_err());
    assert!(c.get_blob("/v1/blobs/../x", 0).is_err());
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 4);
    for s in seen.iter() {
        verify(s);
    }
    assert_eq!(seen[0].headers["x-nc-enroll"], "enroll");
    assert!(
        !seen[1].headers.contains_key("x-nc-enroll"),
        "secret only on register"
    );
    assert_eq!(seen[2].body, b"", "GET signs an empty body");
}
