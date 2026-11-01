#![allow(clippy::unwrap_used, clippy::expect_used)]

use ed25519_dalek::{Signer as _, SigningKey};
use nc_protocol::pb::v1::{verifier, InputRef, Limits, TaskManifest, Verifier};
use nc_protocol::{
    sign_manifest, sign_module, verify_domain, verify_manifest, ManifestPolicy, ProtocolError,
    Signer, PROFILE_CPU_CORE_V1, PROTOCOL_VERSION,
};
use proptest::prelude::*;

struct Key(SigningKey);

impl Key {
    fn random() -> Self {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).unwrap();
        Self(SigningKey::from_bytes(&seed))
    }
}

impl Signer for Key {
    fn public_key(&self) -> [u8; 32] {
        self.0.verifying_key().to_bytes()
    }
    fn sign(&self, msg: &[u8]) -> Result<[u8; 64], ProtocolError> {
        Ok(self.0.sign(msg).to_bytes())
    }
}

const NOW: i64 = 1_800_000_000_000;

fn manifest(publisher: &Key) -> TaskManifest {
    let module_sha256 = [9u8; 32];
    TaskManifest {
        version: Some(PROTOCOL_VERSION),
        task_id: vec![1; 16],
        profile: PROFILE_CPU_CORE_V1.into(),
        module_sha256: module_sha256.to_vec(),
        publisher_key: publisher.public_key().to_vec(),
        module_signature: sign_module(publisher, &module_sha256).unwrap().to_vec(),
        inputs: vec![InputRef {
            sha256: vec![3; 32],
            size: 10,
        }],
        limits: Some(Limits {
            fuel: 1_000,
            wall_ms: 1_000,
            memory_bytes: 1 << 20,
            stack_bytes: 1 << 16,
            output_bytes: 1 << 10,
            checkpoint_bytes: 0,
        }),
        verifier: Some(Verifier {
            kind: verifier::Kind::ReplicateExact as i32,
            replicas: 2,
        }),
        expires_at_unix_ms: NOW + 60_000,
    }
}

struct Fixture {
    issuer: Key,
    publisher: Key,
}

impl Fixture {
    fn new() -> Self {
        Self {
            issuer: Key::random(),
            publisher: Key::random(),
        }
    }
    fn check(
        &self,
        s: &nc_protocol::pb::v1::SignedManifest,
    ) -> Result<TaskManifest, ProtocolError> {
        verify_manifest(
            s,
            &ManifestPolicy {
                trusted_issuers: &[self.issuer.public_key()],
                trusted_publishers: &[self.publisher.public_key()],
                now_unix_ms: NOW,
            },
        )
    }
}

#[test]
fn signed_manifest_roundtrips() {
    let f = Fixture::new();
    let m = manifest(&f.publisher);
    let s = sign_manifest(&m, &f.issuer).unwrap();
    assert_eq!(f.check(&s).unwrap(), m);
}

#[test]
fn untrusted_issuer_refused() {
    let f = Fixture::new();
    let s = sign_manifest(&manifest(&f.publisher), &Key::random()).unwrap();
    assert_eq!(f.check(&s), Err(ProtocolError::UntrustedSigner));
}

#[test]
fn signer_key_swapped_refused() {
    let f = Fixture::new();
    let mut s = sign_manifest(&manifest(&f.publisher), &Key::random()).unwrap();
    s.signer_key = f.issuer.public_key().to_vec();
    assert_eq!(f.check(&s), Err(ProtocolError::BadSignature));
}

#[test]
fn untrusted_publisher_refused() {
    let f = Fixture::new();
    let s = sign_manifest(&manifest(&Key::random()), &f.issuer).unwrap();
    assert_eq!(f.check(&s), Err(ProtocolError::UntrustedSigner));
}

#[test]
fn module_signature_for_other_hash_refused() {
    let f = Fixture::new();
    let mut m = manifest(&f.publisher);
    m.module_sha256 = vec![8; 32];
    let s = sign_manifest(&m, &f.issuer).unwrap();
    assert_eq!(f.check(&s), Err(ProtocolError::BadSignature));
}

#[test]
fn expired_manifest_refused() {
    let f = Fixture::new();
    let mut m = manifest(&f.publisher);
    m.expires_at_unix_ms = NOW;
    let s = sign_manifest(&m, &f.issuer).unwrap();
    assert_eq!(f.check(&s), Err(ProtocolError::Expired));
}

#[test]
fn wrong_profile_and_version_refused() {
    let f = Fixture::new();
    let mut m = manifest(&f.publisher);
    m.profile = "native-x86".into();
    assert!(matches!(
        f.check(&sign_manifest(&m, &f.issuer).unwrap()),
        Err(ProtocolError::Invalid(_))
    ));
    let mut m = manifest(&f.publisher);
    m.version = Some(nc_protocol::pb::v1::ProtocolVersion { major: 2, minor: 0 });
    assert_eq!(
        f.check(&sign_manifest(&m, &f.issuer).unwrap()),
        Err(ProtocolError::Version)
    );
}

#[test]
fn oversized_manifest_refused_before_parsing() {
    let f = Fixture::new();
    let mut s = sign_manifest(&manifest(&f.publisher), &f.issuer).unwrap();
    s.manifest = vec![0; nc_protocol::sign::MAX_MANIFEST_BYTES + 1];
    assert_eq!(f.check(&s), Err(ProtocolError::TooLarge));
}

#[test]
fn domains_are_separated() {
    let k = Key::random();
    let hash = [9u8; 32];
    let sig = sign_module(&k, &hash).unwrap();
    assert!(verify_domain(&k.public_key(), "nc.v1.module", &hash, &sig).is_ok());
    assert_eq!(
        verify_domain(&k.public_key(), "nc.v1.TaskManifest", &hash, &sig),
        Err(ProtocolError::BadSignature)
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Flipping any bit of the manifest bytes or the signature makes verification fail.
    #[test]
    fn any_bit_flip_is_refused(pos in any::<prop::sample::Index>(), bit in 0u8..8, in_sig in any::<bool>()) {
        let f = Fixture::new();
        let mut s = sign_manifest(&manifest(&f.publisher), &f.issuer).unwrap();
        let target = if in_sig { &mut s.signature } else { &mut s.manifest };
        let i = pos.index(target.len());
        target[i] ^= 1 << bit;
        prop_assert!(f.check(&s).is_err());
    }

    /// Arbitrary bytes never verify and never panic.
    #[test]
    fn garbage_never_verifies(manifest in prop::collection::vec(any::<u8>(), 0..512),
                              sig in prop::collection::vec(any::<u8>(), 0..80)) {
        let f = Fixture::new();
        let s = nc_protocol::pb::v1::SignedManifest {
            manifest,
            signer_key: f.issuer.public_key().to_vec(),
            signature: sig,
        };
        prop_assert!(f.check(&s).is_err());
    }
}
