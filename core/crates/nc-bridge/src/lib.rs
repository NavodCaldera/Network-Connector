//! `nc-bridge`: flutter_rust_bridge API surface: wraps the nc-agent API 1:1 for the Flutter app.
//!
//! `api` is hand-written; `frb_generated` is produced by `flutter_rust_bridge_codegen generate`
//! (run from `app/`, config in `app/flutter_rust_bridge.yaml`). Do not edit the generated file.

pub mod api;
#[allow(
    unsafe_code,
    clippy::all,
    clippy::pedantic,
    clippy::unwrap_used,
    clippy::expect_used,
    missing_docs,
    unused_qualifications
)]
mod frb_generated;
mod night;
mod settings;
mod today;
