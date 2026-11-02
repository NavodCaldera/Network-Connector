//! Enrolment against a fake coordinator on 127.0.0.1 (plain HTTP, dev mode), then a coordinator-backed
//! agent built from the saved file.

use super::*;
use crate::api::{EnrolRequest, NetworkKind};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use std::time::Instant;

const SECRET: &str = "test-enrol-secret-123";
const PUBLISHER: &str = "0101010101010101010101010101010101010101010101010101010101010101";

#[derive(Clone, Debug)]
struct Seen {
    method: String,
    path: String,
    headers: HashMap<String, String>,
}

#[derive(Clone, Default)]
struct Fake {
    seen: Arc<Mutex<Vec<Seen>>>,
    /// Whether the fake coordinator knows the device key (set by register, cleared by deregister).
    registered: Arc<std::sync::atomic::AtomicBool>,
}

impl Fake {
    fn start() -> (Self, String) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let fake = Self::default();
        let f = fake.clone();
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                let f = f.clone();
                std::thread::spawn(move || f.serve(s));
            }
        });
        (fake, url)
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    fn serve(&self, s: TcpStream) {
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut w = s;
        loop {
            let mut line = String::new();
            if r.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            let mut parts = line.split_whitespace();
            let method = parts.next().unwrap_or_default().to_string();
            let path = parts.next().unwrap_or_default().to_string();
            let mut headers = HashMap::new();
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).unwrap_or(0) == 0 {
                    return;
                }
                let h = h.trim_end();
                if h.is_empty() {
                    break;
                }
                if let Some((k, v)) = h.split_once(':') {
                    headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
                }
            }
            let len: usize = headers
                .get("content-length")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let mut body = vec![0; len];
            if r.read_exact(&mut body).is_err() {
                return;
            }
            let (status, reply) = match path.as_str() {
                "/v1/devices/register" => {
                    if headers.get("x-nc-enroll").map(String::as_str) == Some(SECRET) {
                        self.registered.store(true, Ordering::SeqCst);
                        (200, r#"{"device_id":"d1","created":true}"#)
                    } else {
                        (
                            401,
                            r#"{"error":{"code":"unauthorized","message":"enrolment secret required"}}"#,
                        )
                    }
                }
                "/v1/devices/heartbeat" => (200, "{}"),
                "/v1/devices/me" if self.registered.load(Ordering::SeqCst) => (
                    200,
                    r#"{"registered":true,"device_id":"d1","name":"test pc","platform":"windows"}"#,
                ),
                "/v1/devices/me" => (
                    200,
                    r#"{"registered":false,"device_id":"","name":"","platform":""}"#,
                ),
                "/v1/devices/deregister" if self.registered.swap(false, Ordering::SeqCst) => {
                    (200, r#"{"device_id":"d1","released_leases":0}"#)
                }
                "/v1/devices/deregister" => (
                    401,
                    r#"{"error":{"code":"unauthorized","message":"unknown device"}}"#,
                ),
                _ => (
                    404,
                    r#"{"error":{"code":"not_found","message":"no queued task"}}"#,
                ),
            };
            self.seen.lock().unwrap().push(Seen {
                method,
                path,
                headers,
            });
            let resp = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{reply}",
                reply.len()
            );
            if w.write_all(resp.as_bytes()).is_err() {
                return;
            }
        }
    }
}

fn request(url: &str, secret: &str) -> EnrolRequest {
    EnrolRequest {
        coordinator_url: url.into(),
        enrol_secret: secret.into(),
        device_name: "test pc".into(),
        network: NetworkKind::Ethernet,
        connection_name: "home".into(),
        speed_mbps: 8,
        unlimited_data: true,
        cool_device: true,
        trusted_publishers: vec![PUBLISHER.into()],
    }
}

#[test]
fn url_rules() {
    assert_eq!(
        check_url("https://c.example/", false).unwrap(),
        "https://c.example"
    );
    assert!(check_url("https://c.example:8443", false).is_ok());
    assert!(
        check_url("http://127.0.0.1:8080", false).is_err(),
        "dev flag off"
    );
    assert!(check_url("http://127.0.0.1:8080", true).is_ok());
    assert!(check_url("http://localhost", true).is_ok());
    assert!(check_url("http://192.168.1.20:8080", true).is_ok());
    assert!(check_url("http://10.0.0.5", true).is_ok());
    assert!(check_url("http://[::1]:80", true).is_ok());
    assert!(check_url("http://c.example", true).is_err(), "public host");
    assert!(check_url("http://8.8.8.8", true).is_err());
    assert!(check_url("ftp://c.example", true).is_err());
    assert!(check_url("https://user@c.example", false).is_err());
    assert!(check_url("https://c.example/path", false).is_err());
    assert!(check_url("https://c.example?x=1", false).is_err());
    assert!(check_url("c.example", false).is_err());
}

#[test]
fn wrong_secret_is_refused_and_nothing_is_saved() {
    let (fake, url) = Fake::start();
    let dir = tempfile::tempdir().unwrap();
    let e = enrol(dir.path(), request(&url, "wrong"), true).unwrap_err();
    assert_eq!(e, "The coordinator did not accept the enrolment secret.");
    assert!(!dir.path().join(CONFIG_NAME).exists());
    assert_eq!(fake.seen().len(), 1);
    // Plain HTTP without the dev flag never reaches the network.
    assert!(enrol(dir.path(), request(&url, SECRET), false).is_err());
    assert_eq!(fake.seen().len(), 1);
}

#[test]
fn unreachable_coordinator_has_a_plain_message() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    drop(l);
    let dir = tempfile::tempdir().unwrap();
    let e = enrol(dir.path(), request(&url, SECRET), true).unwrap_err();
    assert!(e.starts_with("Cannot reach the coordinator"), "{e}");
}

#[test]
fn enrol_then_backend_agent_heartbeats_without_the_secret_and_stops_fast() {
    let (fake, url) = Fake::start();
    let dir = tempfile::tempdir().unwrap();
    let e = enrol(dir.path(), request(&url, SECRET), true).unwrap();
    assert!(!e.active);
    assert_eq!(e.coordinator_url, url);

    // Register was signed and carried the secret.
    let reg = &fake.seen()[0];
    assert_eq!(
        (reg.method.as_str(), reg.path.as_str()),
        ("POST", "/v1/devices/register")
    );
    for h in ["x-nc-key", "x-nc-ts", "x-nc-nonce", "x-nc-sig"] {
        assert!(reg.headers.contains_key(h), "{h}");
    }

    // The saved file and the whole app folder hold no secret.
    for entry in walk(dir.path()) {
        let bytes = fs::read(&entry).unwrap();
        assert!(
            !bytes.windows(SECRET.len()).any(|w| w == SECRET.as_bytes()),
            "secret found in {}",
            entry.display()
        );
    }
    assert!(dir.path().join("agent").join("device.key").exists());

    // Next start: Agent::with_backend from the saved file.
    let f = load(dir.path(), true).unwrap().unwrap();
    assert_eq!(f.network.bytes_per_second, 1_000_000);
    let b = budget(&f, &crate::night::default_night_data()).unwrap();
    assert_eq!(b, NetworkBudget::Unmetered);
    let agent = build_agent(dir.path(), &f, &b).unwrap();
    let base = crate::settings::to_policy(&crate::settings::default_settings().policy)
        .expect("defaults are valid");
    let mut policy = f.apply_trust(base);
    assert_eq!(policy.trusted_peers, vec![PeerKey::from_bytes([1; 32])]);
    assert_eq!(policy.allowed_execution_profiles, vec![PROFILE_CPU_CORE_V1]);
    let all = vec![
        chrono::Weekday::Mon,
        chrono::Weekday::Tue,
        chrono::Weekday::Wed,
        chrono::Weekday::Thu,
        chrono::Weekday::Fri,
        chrono::Weekday::Sat,
        chrono::Weekday::Sun,
    ];
    // The whole day but the last second (nc-policy refuses equal endpoints).
    policy.time_windows = vec![nc_policy::WorkingWindow {
        days: all,
        start: chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
        end: chrono::NaiveTime::from_hms_opt(23, 59, 59).unwrap(),
    }];
    policy.accept_jobs = true;
    policy.charging_only = false;
    policy.min_battery_percent = 0;
    policy.pause_on_user_activity = false;
    agent.start_helper_session(policy).unwrap();
    let t = Instant::now();
    while !fake
        .seen()
        .iter()
        .any(|s| s.path == "/v1/devices/heartbeat")
    {
        assert!(
            t.elapsed() < Duration::from_secs(15),
            "no heartbeat: {:?} {:?}",
            agent.activity(),
            fake.seen().len()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let stop = Instant::now();
    agent.stop_helper_session();
    assert!(
        stop.elapsed() < Duration::from_secs(1),
        "{:?}",
        stop.elapsed()
    );

    let later = &fake.seen()[1..];
    assert!(later.iter().all(|s| s.path != "/v1/devices/register"));
    assert!(later.iter().all(|s| !s.headers.contains_key("x-nc-enroll")));
    assert!(later.iter().all(|s| s.headers.contains_key("x-nc-sig")));
    drop(agent);
    // The ledger is only in the app folder; the agent did not make its default one.
    assert!(dir.path().join("budget.sqlite").exists());
    assert!(!dir.path().join("agent").join("budget.sqlite").exists());

    // Reset removes the enrolment and identity but keeps the data budget ledger.
    unenrol(dir.path()).unwrap();
    assert!(load(dir.path(), true).unwrap().is_none());
    assert!(!dir.path().join("agent").exists());
    assert!(dir.path().join("budget.sqlite").exists());
}

#[test]
fn publisher_keys_are_checked() {
    assert_eq!(parse_key(PUBLISHER).unwrap(), [1; 32]);
    let b64 = URL_SAFE_NO_PAD.encode([7u8; 32]);
    assert_eq!(parse_key(&b64).unwrap(), [7; 32]);
    assert!(parse_key("abc").is_err());
    assert!(parse_key(&URL_SAFE_NO_PAD.encode([7u8; 31])).is_err());
    let dir = tempfile::tempdir().unwrap();
    let mut r = request("https://c.example", SECRET);
    r.trusted_publishers.clear();
    assert!(enrol(dir.path(), r, false)
        .unwrap_err()
        .contains("trusted publisher"));
    let mut r = request("https://c.example", SECRET);
    r.trusted_publishers = vec!["nope".into()];
    assert!(enrol(dir.path(), r, false).is_err());
}

#[test]
fn metered_connection_binds_the_night_profile() {
    let (_fake, url) = Fake::start();
    let dir = tempfile::tempdir().unwrap();
    let mut r = request(&url, SECRET);
    r.unlimited_data = false;
    r.network = NetworkKind::Wifi;
    enrol(dir.path(), r, true).unwrap();
    let f = load(dir.path(), true).unwrap().unwrap();
    match budget(&f, &crate::night::default_night_data()).unwrap() {
        NetworkBudget::Metered(p) => {
            assert_eq!(p.binding.connection_id, "home");
            assert_eq!(p.binding.interface, InterfaceClass::Wifi);
        }
        NetworkBudget::Unmetered => panic!("expected metered"),
    }
    // A saved http:// URL is refused once the dev flag is off.
    assert!(load(dir.path(), false).is_err());
}

fn walk(d: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in fs::read_dir(d).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

#[test]
fn whoami_and_deregister_follow_the_coordinator() {
    let (fake, url) = Fake::start();
    let dir = tempfile::tempdir().unwrap();
    enrol(dir.path(), request(&url, SECRET), true).unwrap();
    let f = load(dir.path(), true).unwrap().unwrap();
    assert_eq!(
        check_registration(dir.path(), &f, true),
        Registration::Registered
    );

    deregister(dir.path(), true).unwrap();
    assert!(!fake.registered.load(Ordering::SeqCst));
    // Signed, without the enrolment secret.
    for s in fake
        .seen()
        .iter()
        .filter(|s| s.path != "/v1/devices/register")
    {
        assert_eq!(s.method, "POST");
        assert!(!s.headers.contains_key("x-nc-enroll"));
        for h in ["x-nc-key", "x-nc-ts", "x-nc-nonce", "x-nc-sig"] {
            assert!(s.headers.contains_key(h), "{h} on {}", s.path);
        }
    }
    assert_eq!(
        check_registration(dir.path(), &f, true),
        Registration::NotRegistered
    );
    // Already forgotten (401) still counts as deregistered.
    deregister(dir.path(), true).unwrap();
    unenrol(dir.path()).unwrap();
    // Nothing saved: nothing to tell the coordinator.
    deregister(dir.path(), true).unwrap();
}

#[test]
fn offline_coordinator_still_allows_a_local_reset() {
    let (_fake, url) = Fake::start();
    let dir = tempfile::tempdir().unwrap();
    enrol(dir.path(), request(&url, SECRET), true).unwrap();
    // Point the saved enrolment at a closed port.
    let closed = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    let mut f = load(dir.path(), true).unwrap().unwrap();
    f.coordinator_url = closed;
    save(dir.path(), &f).unwrap();
    assert!(matches!(
        check_registration(dir.path(), &f, true),
        Registration::Unknown(m) if m.starts_with("Cannot reach")
    ));
    let e = deregister(dir.path(), true).unwrap_err();
    assert!(e.contains("reset this device anyway"), "{e}");
    unenrol(dir.path()).unwrap();
    assert!(load(dir.path(), true).unwrap().is_none());
    assert!(!dir.path().join("agent").exists());
}

#[test]
fn missing_key_is_not_registered() {
    let (_fake, url) = Fake::start();
    let dir = tempfile::tempdir().unwrap();
    enrol(dir.path(), request(&url, SECRET), true).unwrap();
    let f = load(dir.path(), true).unwrap().unwrap();
    fs::remove_dir_all(dir.path().join("agent")).unwrap();
    assert_eq!(
        check_registration(dir.path(), &f, true),
        Registration::NotRegistered
    );
    // No key was created by the check.
    assert!(!dir.path().join("agent").join("device.key").exists());
}
