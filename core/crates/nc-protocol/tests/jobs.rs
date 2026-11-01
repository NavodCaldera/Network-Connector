#![allow(clippy::unwrap_used, clippy::expect_used, deprecated)]
use ed25519_dalek::{Signer as _, SigningKey};
use nc_protocol::{jobs::*, pb::v1::*, sign::*, wire::decode_strict};
use prost::Message;
fn vector() -> SignedJobManifest {
    let v: serde_json::Value = serde_json::from_str(include_str!("fixtures/job.json")).unwrap();
    SignedJobManifest {
        manifest: hex::decode(v["manifest_hex"].as_str().unwrap()).unwrap(),
        signature: hex::decode(v["signature_hex"].as_str().unwrap()).unwrap(),
    }
}
fn resign(j: &JobManifest) -> SignedJobManifest {
    let bytes = j.encode_to_vec();
    let mut msg = b"nc.v1.JobManifest\0".to_vec();
    msg.extend(&bytes);
    SignedJobManifest {
        manifest: bytes,
        signature: SigningKey::from_bytes(&[42; 32])
            .sign(&msg)
            .to_bytes()
            .to_vec(),
    }
}
#[test]
fn independent_fixed_vector_and_executor_module_signature() {
    let s = vector();
    let j = verify_job(&s, 1799999999000).unwrap();
    assert_eq!(j.encode_to_vec(), s.manifest);
    let t = j.task.unwrap();
    verify_module_signature(&t.publisher_key, &t.module_sha256, &t.module_signature).unwrap();
    let mut bad = s.clone();
    bad.signature[0] ^= 1;
    assert!(verify_job(&bad, 1799999999000).is_err());
    assert_eq!(verify_job(&s, 1800000000000), Err(ProtocolError::Expired));
}
#[test]
fn signed_but_noncanonical_or_excessive_jobs_fail() {
    let j = verify_job(&vector(), 1799999999000).unwrap();
    let mut s = vector();
    s.manifest.extend([0xf8, 0x07, 0x01]);
    assert!(verify_job(&s, 1799999999000).is_err());
    let mut s = vector();
    s.manifest = vec![0; 65537];
    assert_eq!(verify_job(&s, 0), Err(ProtocolError::TooLarge));
    for change in 0..9 {
        let mut j = j.clone();
        match change {
            0 => j.total_input_size += 1,
            1 => {
                j.task.as_mut().unwrap().inputs = vec![
                    InputRef {
                        sha256: vec![1; 32],
                        size: 1
                    };
                    1025
                ]
            }
            2 => j.task.as_mut().unwrap().limits.as_mut().unwrap().fuel = u64::MAX,
            3 => {
                j.task
                    .as_mut()
                    .unwrap()
                    .limits
                    .as_mut()
                    .unwrap()
                    .memory_bytes = u64::MAX
            }
            4 => j.task.as_mut().unwrap().limits.as_mut().unwrap().wall_ms = u64::MAX,
            5 => {
                j.task
                    .as_mut()
                    .unwrap()
                    .limits
                    .as_mut()
                    .unwrap()
                    .output_bytes = u64::MAX
            }
            6 => j.task.as_mut().unwrap().verifier.as_mut().unwrap().replicas = 3,
            7 => j.publisher_key_id = "wrong".into(),
            _ => j.input_transfers[0].size = MAX_TRANSFER + 1,
        }
        assert!(
            verify_job(&resign(&j), 1799999999000).is_err(),
            "case {change}"
        );
    }
}
macro_rules! golden {
    ($name:ident,$ty:ty,$value:expr,$hex:expr) => {
        #[test]
        fn $name() {
            let v: $ty = $value;
            let bytes = hex::decode($hex).unwrap();
            assert_eq!(v.encode_to_vec(), bytes);
            assert_eq!(decode_strict::<$ty>(&bytes).unwrap(), v);
            let json = serde_json::to_value(&v).unwrap();
            assert_eq!(serde_json::from_value::<$ty>(json).unwrap(), v);
            let mut bad = bytes;
            bad.extend([0xf8, 0x07, 1]);
            assert!(decode_strict::<$ty>(&bad).is_err());
        }
    };
}
golden!(
    transfer_ref,
    TransferRef,
    TransferRef {
        sha256: "h".into(),
        size: 2,
        path: "/x".into()
    },
    "0a016810021a022f78"
);
golden!(
    submit_request,
    SubmitJobRequest,
    SubmitJobRequest {
        manifest: Some(SignedJobManifest {
            manifest: vec![1],
            signature: vec![2]
        })
    },
    "0a060a0101120102"
);
golden!(
    signed_job,
    SignedJobManifest,
    SignedJobManifest {
        manifest: vec![1],
        signature: vec![2]
    },
    "0a0101120102"
);
golden!(
    submit_response,
    SubmitJobResponse,
    SubmitJobResponse {
        job_id: "j".into(),
        task_ids: vec!["t".into()]
    },
    "0a016a120174"
);
golden!(
    fail_request,
    LeaseFailRequest,
    LeaseFailRequest {
        lease_id: "l".into(),
        attempt_id: "a".into(),
        reason: 1,
        code: Some("POLICY".into())
    },
    "0a016c12016118012206504f4c494359"
);
golden!(
    fail_response,
    LeaseFailResponse,
    LeaseFailResponse {
        task_id: "t".into(),
        state: "queued".into(),
        duplicate: true
    },
    "0a017412067175657565641801"
);
golden!(
    task_status,
    TaskStatus,
    TaskStatus {
        task_id: "t".into(),
        state: "queued".into(),
        output: None
    },
    "0a01741206717565756564"
);
golden!(
    job_status,
    JobStatusResponse,
    JobStatusResponse {
        job_id: "j".into(),
        tasks: vec![TaskStatus {
            task_id: "t".into(),
            state: "queued".into(),
            output: None
        }]
    },
    "0a016a120b0a01741206717565756564"
);
#[test]
fn nested_json_unknown_fields_fail() {
    assert!(serde_json::from_str::<SubmitJobRequest>(
        r#"{"manifest":{"manifest":[],"signature":[],"secret":1}}"#
    )
    .is_err());
    assert!(serde_json::from_str::<LeaseFailRequest>(
        r#"{"lease_id":"l","attempt_id":"a","reason":1,"code":null,"text":"data"}"#
    )
    .is_err());
    let mut v = serde_json::to_value(verify_job(&vector(), 1799999999000).unwrap()).unwrap();
    v["task"]["limits"]["extra"] = serde_json::json!(1);
    assert!(serde_json::from_value::<JobManifest>(v).is_err());
}
#[test]
fn v2_complete_and_lease_roundtrip() {
    let id = "00000000-0000-4000-8000-000000000001".to_string();
    let c = CompleteRequest {
        lease_id: id.clone(),
        attempt_id: id.clone(),
        result: String::new(),
        output_sha256: "ab".into(),
        output_size: 3,
        version: 2,
    };
    let expected = format!(
        "0a24{}1224{}2202616228033002",
        hex::encode(id.as_bytes()),
        hex::encode(id.as_bytes())
    );
    assert_eq!(hex::encode(c.encode_to_vec()), expected);
    assert_eq!(
        decode_strict::<CompleteRequest>(&c.encode_to_vec()).unwrap(),
        c
    );
    assert_eq!(
        serde_json::from_value::<CompleteRequest>(serde_json::to_value(&c).unwrap()).unwrap(),
        c
    );
    let l = LeaseResponse {
        lease_id: id.clone(),
        task_id: id.clone(),
        attempt_id: id.clone(),
        expires_at: 7,
        input: String::new(),
        manifest: Some(SignedJobManifest {
            manifest: vec![1],
            signature: vec![2],
        }),
        transfers: vec![TransferRef {
            sha256: "h".into(),
            size: 2,
            path: "/x".into(),
        }],
        version: 2,
    };
    let expected = format!(
        "0a24{}1224{}1a24{}200732060a01011201023a090a016810021a022f784002",
        hex::encode(id.as_bytes()),
        hex::encode(id.as_bytes()),
        hex::encode(id.as_bytes())
    );
    assert_eq!(hex::encode(l.encode_to_vec()), expected);
    assert_eq!(
        decode_strict::<LeaseResponse>(&l.encode_to_vec()).unwrap(),
        l
    );
}
