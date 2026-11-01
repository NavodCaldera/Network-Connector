//! Generate Rust types from `proto/` (the single source of truth, AGENT.md §1) with a vendored `protoc`,
//! so no system install is needed.

use std::io;
use std::path::PathBuf;

const FILES: &[&str] = &[
    "nc/v1/common.proto",
    "nc/v1/manifest.proto",
    "nc/v1/identity.proto",
    "nc/v1/coordinator.proto",
    "nc/v1/pairing.proto",
    "nc/v1/relay.proto",
];

fn main() -> io::Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("proto");
    let protoc =
        protoc_bin_vendored::protoc_bin_path().map_err(|e| io::Error::other(e.to_string()))?;
    let mut cfg = prost_build::Config::new();
    cfg.protoc_executable(protoc);
    for name in [
        "JobManifest",
        "SignedJobManifest",
        "TaskManifest",
        "ProtocolVersion",
        "Limits",
        "InputRef",
        "Verifier",
        "TransferRef",
        "SubmitJobRequest",
        "SubmitJobResponse",
        "LeaseFailRequest",
        "LeaseFailResponse",
        "TaskStatus",
        "JobStatusResponse",
        "RegisterRequest",
        "RegisterResponse",
        "HeartbeatRequest",
        "HeartbeatResponse",
        "LeaseRequest",
        "LeaseResponse",
        "CompleteRequest",
        "CompleteResponse",
        "RelayTokenRequest",
        "RelayTokenResponse",
        "ErrorEnvelope",
        "ErrorResponse",
        "HealthResponse",
    ] {
        cfg.type_attribute(
            format!("nc.v1.{name}"),
            "#[derive(serde::Serialize, serde::Deserialize)]",
        );
    }
    for name in [
        "RegisterRequest",
        "HeartbeatRequest",
        "LeaseRequest",
        "CompleteRequest",
        "RelayTokenRequest",
    ] {
        cfg.type_attribute(format!("nc.v1.{name}"), "#[serde(deny_unknown_fields)]");
    }
    for field in [
        "RegisterResponse.device_id",
        "LeaseResponse.lease_id",
        "LeaseResponse.task_id",
        "LeaseResponse.attempt_id",
        "CompleteRequest.lease_id",
        "CompleteRequest.attempt_id",
        "CompleteResponse.task_id",
    ] {
        cfg.field_attribute(
            format!("nc.v1.{field}"),
            "#[serde(with = \"crate::json_uuid\")]",
        );
    }
    for name in [
        "JobManifest",
        "SignedJobManifest",
        "TaskManifest",
        "ProtocolVersion",
        "Limits",
        "InputRef",
        "Verifier",
        "SubmitJobRequest",
        "LeaseFailRequest",
    ] {
        cfg.type_attribute(format!("nc.v1.{name}"), "#[serde(deny_unknown_fields)]");
    }
    for field in [
        "LeaseResponse.manifest",
        "LeaseResponse.transfers",
        "LeaseResponse.version",
        "CompleteRequest.output_sha256",
        "CompleteRequest.output_size",
        "CompleteRequest.version",
        "CompleteRequest.result",
    ] {
        cfg.field_attribute(
            format!("nc.v1.{field}"),
            "#[serde(default, skip_serializing_if = \"crate::wire::is_default\")]",
        );
    }
    for field in ["JobManifest.min_helpers", "Verifier.replicas"] {
        cfg.field_attribute(format!("nc.v1.{field}"), "#[serde(default)]");
    }
    cfg.skip_debug([
        "nc.v1.JobManifest",
        "nc.v1.SignedJobManifest",
        "nc.v1.TaskManifest",
        "nc.v1.SignedManifest",
        "nc.v1.PairingOffer",
        "nc.v1.RelayToken",
        "nc.v1.RelayAuthFrame",
        "nc.v1.RelayTokenResponse",
    ]);
    let files: Vec<PathBuf> = FILES.iter().map(|f| root.join(f)).collect();
    for f in &files {
        println!("cargo:rerun-if-changed={}", f.display());
    }
    cfg.compile_protos(&files, &[root])
}
