#![allow(clippy::unwrap_used, clippy::expect_used)]

use nc_identity::{
    issue_session, verify_session_cert, Assurance, DeviceKeyStore, IdentityError, SoftwareKeyStore,
    MAX_SESSION_TTL_MS,
};
use nc_protocol::{sign_domain, verify_domain, Signer};

const NOW: i64 = 1_800_000_000_000;
const HOUR: i64 = 3_600_000;

#[test]
fn root_key_persists_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("root.key");
    let a = SoftwareKeyStore::open_or_create(&path).unwrap();
    let b = SoftwareKeyStore::open_or_create(&path).unwrap();
    assert_eq!(
        DeviceKeyStore::public_key(&a),
        DeviceKeyStore::public_key(&b)
    );
    assert_eq!(a.assurance(), Assurance::Software);
    let c = SoftwareKeyStore::open_or_create(dir.path().join("other.key")).unwrap();
    assert_ne!(
        DeviceKeyStore::public_key(&a),
        DeviceKeyStore::public_key(&c)
    );
}

#[test]
fn key_file_does_not_contain_raw_seed_on_windows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("root.key");
    let _k = SoftwareKeyStore::open_or_create(&path).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(&bytes[..4], b"NCK1");
    if cfg!(windows) {
        assert_eq!(bytes[4], 1, "DPAPI scheme");
        assert!(bytes.len() > 5 + 32, "sealed blob is larger than the seed");
    }
}

#[test]
fn corrupt_key_file_is_refused_not_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("root.key");
    let _k = SoftwareKeyStore::open_or_create(&path).unwrap();
    let mut bytes = std::fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&path, &bytes).unwrap();
    assert!(matches!(
        SoftwareKeyStore::open_or_create(&path),
        Err(IdentityError::Corrupt)
    ));
    // File left as it was (no silent key rotation).
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn debug_output_has_no_key_or_path() {
    let dir = tempfile::tempdir().unwrap();
    let k = SoftwareKeyStore::open_or_create(dir.path().join("root.key")).unwrap();
    let s = format!("{k:?}");
    assert!(!s.contains("root.key"));
    assert_eq!(s, "SoftwareKeyStore { .. }");
}

fn root() -> (tempfile::TempDir, SoftwareKeyStore) {
    let dir = tempfile::tempdir().unwrap();
    let k = SoftwareKeyStore::open_or_create(dir.path().join("root.key")).unwrap();
    (dir, k)
}

#[test]
fn session_cert_verifies_and_session_key_signs() {
    let (_d, r) = root();
    let s = issue_session(&r, NOW, HOUR).unwrap();
    let cert =
        verify_session_cert(&s.cert, Some(&DeviceKeyStore::public_key(&r)), NOW + 1).unwrap();
    assert_eq!(cert.session_key, s.public_key().to_vec());
    let sig = sign_domain(&s, "test", b"hello").unwrap();
    assert!(verify_domain(&cert.session_key, "test", b"hello", &sig).is_ok());
}

#[test]
fn session_cert_expiry_and_window() {
    let (_d, r) = root();
    let s = issue_session(&r, NOW, HOUR).unwrap();
    assert!(verify_session_cert(&s.cert, None, NOW + HOUR).is_err());
    assert!(verify_session_cert(&s.cert, None, NOW - HOUR).is_err());
    assert!(verify_session_cert(&s.cert, None, NOW + HOUR - 1).is_ok());
}

#[test]
fn session_ttl_capped() {
    let (_d, r) = root();
    assert!(issue_session(&r, NOW, MAX_SESSION_TTL_MS + 1).is_err());
    assert!(issue_session(&r, NOW, 0).is_err());
}

#[test]
fn session_cert_from_other_root_refused() {
    let (_d, r) = root();
    let (_d2, other) = root();
    let s = issue_session(&other, NOW, HOUR).unwrap();
    assert!(verify_session_cert(&s.cert, Some(&DeviceKeyStore::public_key(&r)), NOW).is_err());
}

#[test]
fn tampered_session_cert_refused() {
    let (_d, r) = root();
    let mut s = issue_session(&r, NOW, HOUR).unwrap();
    let i = s.cert.cert.len() - 1;
    s.cert.cert[i] ^= 1;
    assert!(verify_session_cert(&s.cert.clone(), None, NOW).is_err());
}
