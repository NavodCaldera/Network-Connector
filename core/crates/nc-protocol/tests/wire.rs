//! Each new message has a binary round trip; HTTP bodies also pin existing JSON shapes.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use nc_protocol::pb::v1::*;
use nc_protocol::wire::{decode_strict, relay_token_from_text, relay_token_to_text};
use prost::Message;
mod fixtures;
const UUID: &str = "00000000-0000-4000-8000-000000000001";

macro_rules! http_case {
    ($name:ident, $ty:ty, $value:expr, $json:expr) => {
        #[test]
        fn $name() {
            let value: $ty = $value;
            assert_eq!(decode_strict::<$ty>(&value.encode_to_vec()).unwrap(), value);
            let json = $json;
            assert_eq!(serde_json::to_value(&value).unwrap(), json);
            assert_eq!(serde_json::from_value::<$ty>(json).unwrap(), value);
        }
    };
}
http_case!(
    register_request,
    RegisterRequest,
    RegisterRequest {
        name: "pc".into(),
        platform: "windows".into()
    },
    serde_json::json!({"name":"pc","platform":"windows"})
);
http_case!(
    register_response,
    RegisterResponse,
    RegisterResponse {
        device_id: UUID.into(),
        created: false
    },
    serde_json::json!({"device_id":UUID,"created":false})
);
http_case!(
    heartbeat_request,
    HeartbeatRequest,
    HeartbeatRequest {
        status: "idle".into()
    },
    serde_json::json!({"status":"idle"})
);
http_case!(
    heartbeat_response,
    HeartbeatResponse,
    HeartbeatResponse { server_time: 1000 },
    serde_json::json!({"server_time":1000})
);
http_case!(
    lease_request,
    LeaseRequest,
    LeaseRequest {},
    serde_json::json!({})
);
http_case!(
    lease_response,
    LeaseResponse,
    LeaseResponse {
        lease_id: UUID.into(),
        task_id: UUID.into(),
        attempt_id: UUID.into(),
        expires_at: 1600,
        input: "dummy".into()
    },
    serde_json::json!({"lease_id":UUID,"task_id":UUID,"attempt_id":UUID,"expires_at":1600,"input":"dummy"})
);
http_case!(
    complete_request,
    CompleteRequest,
    CompleteRequest {
        lease_id: UUID.into(),
        attempt_id: UUID.into(),
        result: "done".into()
    },
    serde_json::json!({"lease_id":UUID,"attempt_id":UUID,"result":"done"})
);
http_case!(
    complete_response,
    CompleteResponse,
    CompleteResponse {
        task_id: UUID.into(),
        state: "committed".into(),
        duplicate: false
    },
    serde_json::json!({"task_id":UUID,"state":"committed","duplicate":false})
);
http_case!(
    relay_token_request,
    RelayTokenRequest,
    RelayTokenRequest {
        peer_id: "peer".into()
    },
    serde_json::json!({"peer_id":"peer"})
);
http_case!(
    relay_token_response,
    RelayTokenResponse,
    RelayTokenResponse {
        token: "opaque-test-token".into(),
        expires_at: 1600
    },
    serde_json::json!({"token":"opaque-test-token","expires_at":1600})
);
http_case!(
    error_envelope,
    ErrorEnvelope,
    ErrorEnvelope {
        code: "bad_request".into(),
        message: "invalid body".into()
    },
    serde_json::json!({"code":"bad_request","message":"invalid body"})
);
http_case!(
    error_response,
    ErrorResponse,
    ErrorResponse {
        error: Some(ErrorEnvelope {
            code: "bad_request".into(),
            message: "invalid body".into()
        })
    },
    serde_json::json!({"error":{"code":"bad_request","message":"invalid body"}})
);
http_case!(
    health_response,
    HealthResponse,
    HealthResponse { ok: true },
    serde_json::json!({"ok":true})
);

#[test]
fn pairing_offer_golden_and_roundtrip() {
    let value = PairingOffer {
        peer_id: "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN".into(),
        addrs: vec!["/ip4/127.0.0.1/tcp/4001".into()],
        secret: vec![7; 32],
        expires_unix: 1700000000,
        version: 1,
    };
    let bytes = hex::decode(fixtures::OFFER_HEX).unwrap();
    assert_eq!(value.encode_to_vec(), bytes);
    assert_eq!(decode_strict::<PairingOffer>(&bytes).unwrap(), value);
    assert_eq!(format!("{value:?}"), "PairingOffer(<redacted>)");
}
#[test]
fn pairing_reject_roundtrip() {
    let value = PairingReject {};
    assert_eq!(
        decode_strict::<PairingReject>(&value.encode_to_vec()).unwrap(),
        value
    );
}
#[test]
fn pairing_frames_roundtrip_all_variants() {
    for payload in [
        pairing_frame::Payload::Nonce(vec![1; 32]),
        pairing_frame::Payload::Proof(vec![2; 32]),
        pairing_frame::Payload::Confirm(true),
        pairing_frame::Payload::Confirm(false),
        pairing_frame::Payload::Reject(PairingReject {}),
    ] {
        let value = PairingFrame {
            payload: Some(payload),
        };
        assert_eq!(
            decode_strict::<PairingFrame>(&value.encode_to_vec()).unwrap(),
            value
        );
    }
}
#[test]
fn relay_claims_roundtrip() {
    let value = RelayTokenClaims {
        peer: Some("peerA".into()),
        account: Some("acct".into()),
        device: Some("dev".into()),
        iat: Some(1000),
        exp: Some(1600),
    };
    assert_eq!(
        decode_strict::<RelayTokenClaims>(&value.encode_to_vec()).unwrap(),
        value
    );
}
#[test]
fn relay_token_golden_and_roundtrip() {
    let bytes = hex::decode(fixtures::TOKEN_HEX).unwrap();
    let token: RelayToken = decode_strict(&bytes).unwrap();
    assert_eq!(token.version, 1);
    assert_eq!(token.encode_to_vec(), bytes);
    assert_eq!(
        relay_token_from_text(&relay_token_to_text(&token)).unwrap(),
        token
    );
    let claims: RelayTokenClaims = decode_strict(&token.claims).unwrap();
    assert_eq!(
        claims,
        RelayTokenClaims {
            peer: Some("peerA".into()),
            account: Some("acct".into()),
            device: Some("dev".into()),
            iat: Some(1000),
            exp: Some(1600)
        }
    );
    assert_eq!(
        token.signature,
        hex::decode("ba71e475ee68cb7cf3142fd97246343ab922953dd592fa0e75ccc6dde8bb622e").unwrap()
    );
    assert_eq!(format!("{token:?}"), "RelayToken(<redacted>)");
}
#[test]
fn relay_auth_roundtrip_both_directions() {
    let token: RelayToken = decode_strict(&hex::decode(fixtures::TOKEN_HEX).unwrap()).unwrap();
    for payload in [
        relay_auth_frame::Payload::Token(token),
        relay_auth_frame::Payload::Accepted(true),
        relay_auth_frame::Payload::Accepted(false),
    ] {
        let value = RelayAuthFrame {
            payload: Some(payload),
        };
        assert_eq!(
            decode_strict::<RelayAuthFrame>(&value.encode_to_vec()).unwrap(),
            value
        );
        assert_eq!(format!("{value:?}"), "RelayAuthFrame(<redacted>)");
    }
}
#[test]
fn rejects_unknown_fields_duplicates_truncation_and_nested_unknowns() {
    let original = HeartbeatRequest {
        status: "idle".into(),
    }
    .encode_to_vec();
    let mut unknown = original.clone();
    unknown.extend_from_slice(&[0x78, 1]);
    assert!(decode_strict::<HeartbeatRequest>(&unknown).is_err());
    let mut duplicate = original.clone();
    duplicate.extend_from_slice(&original);
    assert!(decode_strict::<HeartbeatRequest>(&duplicate).is_err());
    assert!(decode_strict::<HeartbeatRequest>(&original[..original.len() - 1]).is_err());
    // PairingFrame.reject { unknown_tag_1: true } must not be silently emptied.
    assert!(decode_strict::<PairingFrame>(&[0x22, 2, 8, 1]).is_err());
    assert!(relay_token_from_text("v2.AA").is_err());
    assert!(relay_token_from_text(&format!("v1.{}", "A".repeat(2048))).is_err());
}
#[test]
fn all_http_requests_reject_unknown_and_missing_fields() {
    assert!(serde_json::from_str::<RegisterRequest>(
        r#"{"name":"pc","platform":"linux","extra":1}"#
    )
    .is_err());
    assert!(serde_json::from_str::<HeartbeatRequest>(r#"{"status":"idle","extra":1}"#).is_err());
    assert!(serde_json::from_str::<LeaseRequest>(r#"{"extra":1}"#).is_err());
    assert!(serde_json::from_value::<CompleteRequest>(
        serde_json::json!({"lease_id":UUID,"attempt_id":UUID,"result":"","extra":1})
    )
    .is_err());
    assert!(serde_json::from_str::<RelayTokenRequest>(r#"{"peer_id":"p","extra":1}"#).is_err());
    assert!(serde_json::from_str::<RegisterRequest>("{}").is_err());
    assert!(serde_json::from_str::<HeartbeatRequest>("{}").is_err());
    assert!(serde_json::from_str::<CompleteRequest>("{}").is_err());
    assert!(serde_json::from_str::<RelayTokenRequest>("{}").is_err());
}
#[test]
fn uuid_json_validation_and_normalization_is_preserved() {
    let compact = UUID.replace('-', "");
    let request: CompleteRequest = serde_json::from_value(
        serde_json::json!({"lease_id":compact,"attempt_id":UUID,"result":""}),
    )
    .unwrap();
    assert_eq!(request.lease_id, UUID);
    assert!(serde_json::from_value::<CompleteRequest>(
        serde_json::json!({"lease_id":"garbage","attempt_id":UUID,"result":""})
    )
    .is_err());
}
