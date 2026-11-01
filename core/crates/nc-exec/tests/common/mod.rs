#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]
//! Shared test helpers: WAT modules for malicious jobs, a test-only verifier.

use nc_exec::{sha256, Job, Limits, ModuleVerifier, PublisherSignature};

/// Test-only verifier: accepts exactly the key `b"test-publisher"` with signature `b"ok"`.
/// The real Ed25519 verifier comes from `nc-identity` (task 0.3).
pub struct TestVerifier;

impl ModuleVerifier for TestVerifier {
    fn verify(&self, _h: &[u8; 32], sig: &PublisherSignature) -> Result<(), String> {
        if sig.public_key == b"test-publisher" && sig.signature == b"ok" {
            Ok(())
        } else {
            Err("untrusted publisher".into())
        }
    }
}

pub fn good_sig() -> PublisherSignature {
    PublisherSignature {
        public_key: b"test-publisher".to_vec(),
        signature: b"ok".to_vec(),
    }
}

pub const IMPORTS: &str = r#"
 (import "nc" "input_size" (func $size (result i64)))
 (import "nc" "input_read" (func $read (param i64 i32 i32) (result i32)))
 (import "nc" "output_write" (func $write (param i32 i32) (result i32)))
 (import "nc" "checkpoint" (func $ckpt (param i32 i32) (result i32)))
"#;

pub fn wasm(body: &str) -> Vec<u8> {
    wat::parse_str(format!("(module {IMPORTS} {body})")).expect("valid wat")
}

/// Echoes its input to output and checkpoints it.
pub fn echo() -> Vec<u8> {
    wasm(
        r#"(memory (export "memory") 1)
        (func (export "run") (result i32) (local $n i32)
          (local.set $n (call $read (i64.const 0) (i32.const 0) (i32.const 65536)))
          (drop (call $ckpt (i32.const 0) (local.get $n)))
          (drop (call $write (i32.const 0) (local.get $n)))
          (i32.const 0))"#,
    )
}

pub fn infinite_loop() -> Vec<u8> {
    wasm(
        r#"(memory (export "memory") 1) (func (export "run") (result i32) (loop $l (br $l)) (i32.const 0))"#,
    )
}

pub fn memory_growth() -> Vec<u8> {
    wasm(
        r#"(memory (export "memory") 1)
        (func (export "run") (result i32)
          (loop $l (drop (memory.grow (i32.const 16))) (br $l)) (i32.const 0))"#,
    )
}

pub fn huge_output() -> Vec<u8> {
    wasm(
        r#"(memory (export "memory") 1)
        (func (export "run") (result i32)
          (loop $l (drop (call $write (i32.const 0) (i32.const 65536))) (br $l)) (i32.const 0))"#,
    )
}

pub fn deep_recursion() -> Vec<u8> {
    wasm(
        r#"(memory (export "memory") 1)
        (func $f (param i32) (result i32) (call $f (i32.add (local.get 0) (i32.const 1))))
        (func (export "run") (result i32) (call $f (i32.const 0)))"#,
    )
}

pub fn wasi_import() -> Vec<u8> {
    wat::parse_str(
        r#"(module (import "wasi_snapshot_preview1" "fd_write" (func (param i32 i32 i32 i32) (result i32)))
          (memory (export "memory") 1) (func (export "run") (result i32) (i32.const 0)))"#,
    )
    .unwrap()
}

pub fn wrong_import_type() -> Vec<u8> {
    wat::parse_str(
        r#"(module (import "nc" "input_size" (func (result i32)))
          (memory (export "memory") 1) (func (export "run") (result i32) (i32.const 0)))"#,
    )
    .unwrap()
}

pub fn huge_initial_memory() -> Vec<u8> {
    wasm(r#"(memory (export "memory") 16384) (func (export "run") (result i32) (i32.const 0))"#)
}

pub fn job(module: Vec<u8>, input: &[u8], limits: Limits) -> Job {
    Job {
        module_sha256: sha256(&module),
        module,
        publisher: good_sig(),
        input: input.to_vec(),
        limits,
    }
}
