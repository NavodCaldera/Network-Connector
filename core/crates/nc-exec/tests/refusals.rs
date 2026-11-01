#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Refusals that happen in the parent before any process starts (every platform).

mod common;

use common::*;
use nc_exec::{CancelToken, ExecError, Executor, ExecutorConfig, Limits, PublisherSignature};

fn executor() -> Executor {
    // The worker path is never reached in these tests.
    Executor::new(
        ExecutorConfig::new("does-not-exist-worker"),
        Box::new(TestVerifier),
    )
}

#[test]
fn wrong_hash_refused_before_spawn() {
    let mut j = job(echo(), b"", Limits::default());
    j.module_sha256[31] ^= 0xff;
    let e = executor().run(&j, &CancelToken::new()).unwrap_err();
    assert!(matches!(e, ExecError::HashMismatch), "{e:?}");
}

#[test]
fn tampered_module_refused_before_spawn() {
    let mut j = job(echo(), b"", Limits::default());
    let last = j.module.len() - 1;
    j.module[last] ^= 1;
    let e = executor().run(&j, &CancelToken::new()).unwrap_err();
    assert!(matches!(e, ExecError::HashMismatch), "{e:?}");
}

#[test]
fn bad_signature_refused_before_spawn() {
    let mut j = job(echo(), b"", Limits::default());
    j.publisher = PublisherSignature {
        public_key: b"test-publisher".to_vec(),
        signature: b"forged".to_vec(),
    };
    let e = executor().run(&j, &CancelToken::new()).unwrap_err();
    assert!(matches!(e, ExecError::SignatureInvalid(_)), "{e:?}");
}

#[test]
fn bad_limits_refused() {
    let lim = Limits {
        stack_bytes: 1,
        ..Limits::default()
    };
    let e = executor()
        .run(&job(echo(), b"", lim), &CancelToken::new())
        .unwrap_err();
    assert!(matches!(e, ExecError::BadLimits(_)), "{e:?}");
}

#[cfg(windows)]
#[test]
fn missing_worker_is_a_sandbox_refusal() {
    let e = executor()
        .run(&job(echo(), b"", Limits::default()), &CancelToken::new())
        .unwrap_err();
    assert!(matches!(e, ExecError::Sandbox(_)), "{e:?}");
}

#[cfg(not(windows))]
#[test]
fn no_os_sandbox_means_refusal() {
    let e = executor()
        .run(&job(echo(), b"", Limits::default()), &CancelToken::new())
        .unwrap_err();
    assert!(matches!(e, ExecError::Sandbox(_)), "{e:?}");
}

// --- Real Ed25519 publisher signatures (nc-protocol, task 0.3) ---

mod ed25519 {
    use super::common::*;
    use ed25519_dalek::{Signer as _, SigningKey};
    use nc_exec::{
        sha256, CancelToken, Ed25519Publishers, ExecError, Executor, ExecutorConfig, Job,
        PublisherSignature,
    };
    use nc_protocol::pb::v1::{Limits as PbLimits, TaskManifest};
    use nc_protocol::{sign_module, ProtocolError, Signer};

    struct Key(SigningKey);
    impl Signer for Key {
        fn public_key(&self) -> [u8; 32] {
            self.0.verifying_key().to_bytes()
        }
        fn sign(&self, msg: &[u8]) -> Result<[u8; 64], ProtocolError> {
            Ok(self.0.sign(msg).to_bytes())
        }
    }
    fn key() -> Key {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).unwrap();
        Key(SigningKey::from_bytes(&seed))
    }

    fn signed_job(publisher: &Key) -> Job {
        let module = echo();
        let h = sha256(&module);
        let m = TaskManifest {
            module_sha256: h.to_vec(),
            publisher_key: publisher.public_key().to_vec(),
            module_signature: sign_module(publisher, &h).unwrap().to_vec(),
            limits: Some(PbLimits {
                fuel: 1_000_000,
                wall_ms: 5_000,
                memory_bytes: 1 << 20,
                stack_bytes: 1 << 16,
                output_bytes: 1 << 10,
                checkpoint_bytes: 1 << 10,
            }),
            ..TaskManifest::default()
        };
        Job::from_manifest(&m, module, b"in".to_vec()).unwrap()
    }

    fn executor(trusted: &Key) -> Executor {
        Executor::new(
            ExecutorConfig::new("does-not-exist-worker"),
            Box::new(Ed25519Publishers::new([trusted.public_key()])),
        )
    }

    #[test]
    fn valid_signature_passes_to_sandbox_stage() {
        let p = key();
        let j = signed_job(&p);
        assert_eq!(j.limits.fuel, 1_000_000);
        // Passing the signature check means the next step (sandbox) is reached; the worker path is bogus.
        let e = executor(&p).run(&j, &CancelToken::new()).unwrap_err();
        assert!(matches!(e, ExecError::Sandbox(_)), "{e:?}");
    }

    #[test]
    fn untrusted_publisher_refused() {
        let j = signed_job(&key());
        let e = executor(&key()).run(&j, &CancelToken::new()).unwrap_err();
        assert!(matches!(e, ExecError::SignatureInvalid(_)), "{e:?}");
    }

    #[test]
    fn forged_signature_refused() {
        let p = key();
        let mut j = signed_job(&p);
        j.publisher = PublisherSignature {
            public_key: p.public_key().to_vec(),
            signature: vec![0; 64],
        };
        let e = executor(&p).run(&j, &CancelToken::new()).unwrap_err();
        assert!(matches!(e, ExecError::SignatureInvalid(_)), "{e:?}");
    }

    #[test]
    fn signature_over_other_module_refused() {
        let p = key();
        let mut j = signed_job(&p);
        j.module = huge_output();
        j.module_sha256 = sha256(&j.module);
        let e = executor(&p).run(&j, &CancelToken::new()).unwrap_err();
        assert!(matches!(e, ExecError::SignatureInvalid(_)), "{e:?}");
    }
}
