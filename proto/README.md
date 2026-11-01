# Protocol schemas

`proto/nc/v1/*.proto` is the single source for the versioned `nc.v1` wire types.
`nc-protocol/build.rs` invokes prost-build with protoc-bin-vendored once, from either
workspace. Consumers import `nc_protocol::pb::v1`; they never regenerate or copy types.
Existing manifest/identity signing formats are unchanged.

## Tags and evolution

Each field's permanent tag and meaning is documented in its schema. No fields have
been removed in this migration, so there are no removed numbers to reserve yet.
When removing a field, add `reserved <number>;` and `reserved "<name>";` in that
message. Never renumber or reuse a tag. Existing textual version prefixes are also
represented by `uint32 version = 1` in RelayToken and tag 5 in PairingOffer, both 1.
Coordinator v2 lease/complete messages add an explicit version=2 while retaining
all historical tags.

## Coordinator HTTP JSON

HTTP remains JSON, using serde derives attached to generated coordinator types by
prost-build. Field names, required request fields, numeric seconds, UUID strings,
empty lease requests and the nested error object remain unchanged. UUID field
serde helpers use the same UUID parser/normalization as the former UUID members.
Unknown request fields are rejected; endpoint validation retains the existing
name/platform/status/result/peer bounds. Database UUID conversions stay server-side.
HealthResponse covers `/healthz`. Server-specific validation is an extension trait,
not a second wire model. Control requests retain their 16 KiB body limit; job submission allows 384 KiB
for JSON byte-array expansion. Blob uploads have a separate streaming cap.

### Exact HTTP request-signing bytes

Ed25519 signs the UTF-8 bytes of this concatenation, with **five LF bytes** (`0a`),
no CR bytes, no final LF, no BOM, and no terminating NUL:

```text
NC-REQ-V1\n{method}\n{path}\n{timestamp}\n{nonce}\n{sha256_hex_of_body}
```

- `method`: the HTTP method string as supplied (normally uppercase `POST`).
- `path`: `Uri::path()`, exactly as passed to auth; the query string is excluded.
- `timestamp`: signed Unix seconds parsed from `x-nc-ts`, formatted as a decimal i64
  (the parsed number is formatted, not the original header spelling).
- `nonce`: exact `x-nc-nonce` text, 16..64 ASCII characters `[A-Za-z0-9_-]`.
- `sha256_hex_of_body`: 64 lowercase hexadecimal characters, SHA-256 over the exact
  received body bytes. JSON whitespace/order is never normalized before signing.
- `x-nc-key` and `x-nc-sig`: unpadded base64url of the 32-byte public key and 64-byte
  signature. They are headers, not part of the concatenation.

The fixed-vector test in `nc-coordinator/src/auth.rs` pins the complete bytes for
`POST`, `/v1/leases/request`, timestamp 1700000000, nonce `abcdefghijklmnop`, body
`{}`. The body digest is
`44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a`.
The existing 300-second clock window, nonce replay storage, signature verification,
lease expiry/ownership and attempt-checked idempotent commit remain enforced.

## P2P framing and tokens

Pairing and relay-auth bodies are canonical prost binary, preceded by their
existing unsigned 32-bit big-endian byte length. Receivers cap lengths before
allocation. `decode_strict` requires bytes equal to prost's re-encoding, rejecting
unknown fields (also nested), duplicates, reordered fields and noncanonical varints.
This preserves fail-closed unknown-field handling instead of prost silently
ignoring extensions. Schema changes need coordinated reader/writer upgrades.

Pairing QR text is `nc-pair:v1:` plus unpadded base64url of encoded PairingOffer.
The encoded text body is capped at 4096 characters; offers retain the 16-address,
32-byte secret, valid peer/address and exclusive expiry checks. The nc-net runtime
PairingOffer holds parsed PeerId/Multiaddr values and redacts its secret; it has no
serde representation. PairingFrame selects exactly one nonce/proof/confirm/reject;
nonces and proofs are exactly 32 bytes, frames are capped at 1024 bytes.

Pairing HMAC/SAS transcript bytes are unchanged:
`"nc-pair-v1" || len(A)_u32_be || A || len(B)_u32_be || B || 32_u32_be || nB ||
32_u32_be || nA`, where A/B are responder/initiator PeerId binary bytes and nB/nA
are initiator/responder nonces. Proof HMAC-SHA256 input is role (`init` or `resp`)
followed by that transcript. SAS uses role `sas`, interprets the first four digest
bytes as u32 BE, reduces modulo 1,000,000 and prints six decimal digits. Five failed
proofs lock the single-use offer; both users must still confirm matching SAS.

RelayToken is binary: version 1, exact encoded RelayTokenClaims bytes, and a 32-byte
HMAC-SHA256 signature over `b"nc-relay-token-v1." || claims_bytes`. HMAC verification
precedes claims decoding. Claims fields have explicit protobuf presence so omitted
peer/account/device/iat/exp fields remain invalid, including when a present iat is zero. HTTP represents that binary token as the opaque string
`v1.` plus unpadded base64url; nc-net unwraps it before sending a RelayAuthFrame.token.
The relay's response selects accepted=true/false, replacing the former raw text.
The existing 2048-character token cap, exclusive expiry, at-most-900-second TTL,
60-second future-issuance allowance, authenticated peer binding and quotas remain.
Relay-auth frame cap is 2056 bytes (the old token cap plus envelope headroom);
response frames stay capped at 64 bytes by the client. Secret-bearing generated
messages have redacted Debug implementations.

The old JSON pairing frames/QR payload and JSON/text relay tokens are intentionally
not accepted: this requested encoding migration requires all communicating builds
to update together. Existing coordinator HTTP JSON clients retain their shapes;
relay tokens remain opaque strings. Golden vectors in nc-protocol tests pin the
pairing offer and complete signed binary relay token; nc-relay also pins issued
HTTP token text. Test keys and secrets in these vectors are synthetic fixtures.

## Remaining process-boundary format outside task 1.P3 ownership

`core/crates/nc-exec/src/ipc.rs` still defines its private NCX1/NCR1 worker IPC.
The task explicitly forbids editing nc-exec, so migrating that process boundary
requires its owner and is not claimed complete here. Application payload streams
remain caller-defined opaque bytes, not new handwritten control messages.

The server Docker image builds with `core/crates/nc-protocol/` and `proto/` (task 1.P4); see `infra/README.md`.

## Real jobs (HTTP v2)

`JobManifest` embeds the existing `TaskManifest`; it does not replace the executor
contract. One submission creates one task: job UUID is the 16 task_id bytes in UUID
network order. `publisher_key_id` is lowercase hex of the embedded publisher key.
The publisher signs the entire job; requester identity comes from request auth.
Publisher validity is cryptographic, not a server endorsement; helpers still apply
their own trusted-publisher policy.

Canonical encoding is prost/protoc proto3 binary: fields in ascending tag order,
minimal unsigned varints, default scalar values omitted, length-delimited nested
messages recursively canonical, repeated fields in declared order. No unknown,
duplicate, reordered, packed substitute or nonminimal fields are accepted.
`SignedJobManifest.manifest` is the exact encoding of `JobManifest`; signature
bytes are Ed25519 over ASCII `nc.v1.JobManifest` followed by one NUL byte and those
manifest bytes, with no length prefix or final newline. The module signature is
unchanged: ASCII `nc.v1.module`, NUL, then the 32 plaintext module SHA-256 bytes.
Both signatures use strict Ed25519 verification.

Synthetic fixed vector, computed independently with Python cryptography Ed25519:
seed = 32 bytes `2a`; verification time = 1799999999000 ms.

```json
{
  "manifest_hex": "0af0010a0208011210010101010101010101010101010101011a0b6370752d636f72652d76312220120970d812836f19888625587a4606a5ad23cef31c8684e601771552548fc6b92a20197f6b23e16c8532c6abc838facd5ea789be0c76b2920334039bfa8b3d368d613240b4a3c4c6ef5275ec21f6bfd273643dad906f465786986dcaef36cc7e75c5f8524319d2250b5efb1f340291ad1b8553c64e4892dc14abcaa24770392f76a7700d3a240a20c96c6d5be8d08a12e7b5cdc1b207fa6b2430974c86803d8891675e76fd992c201005421208c0843d10e80718808040208080042880084a04080110015080a0f1c2b13410051a403139376636623233653136633835333263366162633833386661636435656137383962653063373662323932303333343033396266613862336433363864363120012a240a20c96c6d5be8d08a12e7b5cdc1b207fa6b2430974c86803d8891675e76fd992c20100532240a20120970d812836f19888625587a4606a5ad23cef31c8684e601771552548fc6b91006",
  "signature_hex": "67024365a3d138e81ebaffbdcb7cf33c064fb31197791dde23250e01d62ca1d2aca129a6d9bb2a14572fb6c89ce6964202c6c3d38fbf8e6ab2957b5ab7a27c08",
  "publisher_key_hex": "197f6b23e16c8532c6abc838facd5ea789be0c76b2920334039bfa8b3d368d61",
  "module_signature_hex": "b4a3c4c6ef5275ec21f6bfd273643dad906f465786986dcaef36cc7e75c5f8524319d2250b5efb1f340291ad1b8553c64e4892dc14abcaa24770392f76a7700d"
}
```

The same fixture is checked with Rust ed25519-dalek and the nc-exec-compatible
module verifier. Deadline is exclusive, Unix milliseconds; lease expiry is seconds.
`min_helpers` and verifier.replicas default from zero to 1, may be 1 or 2, and
min_helpers cannot exceed replicas. Replicas are sequential distinct devices.
After six attempts a task fails. Deadline expiry is reported as failed.

`task.inputs` describes plaintext chunks (up to 8 MiB, matching nc-blob);
`total_input_size` is their checked sum (at most 1 GiB, at most 1024 chunks).
`input_transfers` has one exact uploaded-byte hash/size per plaintext chunk, in the
same order. `module_transfer` likewise describes uploaded module bytes. Transport
objects may be encrypted: clients own encryption, key distribution, decryption,
and the mapping to nc-blob plaintext identifiers. Server hashes ONLY the uploaded
opaque bytes, never receives encryption keys, and cannot validate plaintext.
Devices decrypt and verify plaintext hashes and the module publisher signature.

Maxima: 64 KiB encoded job, 16 MiB each transfer/output/checkpoint, fuel 10^10,
memory 512 MiB, wall time 300000 ms, stack 8 MiB, deadline within seven days.
Fuel, memory, wall time, stack and output caps must be nonzero. JSON request cap
is 384 KiB for submission (byte arrays expand in JSON), 16 KiB for other control
requests; unknown fields at every nested level fail.

POST /v1/jobs accepts SubmitJobRequest, idempotent only for identical bytes and
requester. Submission may precede upload; scheduling waits for every referenced
blob with the signed size and proof that the requester uploaded it. A requester
cannot gain access to somebody else's object by submitting its hash. PUT /v1/blobs/{lowercase_sha256} signs the actual body
using NC-REQ-V1; server authenticates against the path hash before streaming and
then checks it against streamed bytes. Uploads cap at 16 MiB including chunked
requests. Re-PUT authenticates and hashes the body again and returns 200.
GET blob and GET job status sign an empty body using the existing auth format.
Single byte ranges (inclusive, open-ended, suffix) are supported; malformed or
multiple ranges return the common JSON error with HTTP 416. Range is not in the
auth transcript; it selects bytes of the same authorized immutable object.
Only the requester or a device holding a live lease for a referencing job can GET
inputs/modules. Committed outputs are available to the requester. Upload alone
does not authorize GET. Blob refs are relative HTTPS paths, not bearer URLs;
every request needs a fresh signed nonce. TLS terminates at the coordinator or
trusted deployment proxy; the loopback Docker test stack uses HTTP.

LeaseResponse.version=2 includes the full signed manifest and module-first transfer
refs. Demand-created dummy jobs are retired; input tag 5 remains deprecated for
wire history. CompleteRequest.version=2 requires a preuploaded output owned by the
committing device, exact size and hash, and empty legacy result. Two differing
results dispute the task; two identical results from distinct devices commit it.
Duplicate complete/fail must have identical payload; stale attempts fail.
LeaseFailRequest uses numeric reason enum 1..9 and optional [A-Z0-9_]{1,32} code.
GET /v1/jobs/{uuid} returns per-task state and only committed output refs, no data.
NC_BLOB_DIR names a private server-owned directory on a persistent volume. Never
share it with untrusted filesystem writers. Temporary uploads are removed on all
normal errors; process-kill leftovers need operator cleanup while stopped.
