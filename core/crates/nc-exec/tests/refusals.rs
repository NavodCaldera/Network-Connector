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
