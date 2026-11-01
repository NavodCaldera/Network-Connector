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
Coordinator bodies never carried a version field and retain their current shape.

## Coordinator HTTP JSON

HTTP remains JSON, using serde derives attached to generated coordinator types by
prost-build. Field names, required request fields, numeric seconds, UUID strings,
empty lease requests and the nested error object remain unchanged. UUID field
serde helpers use the same UUID parser/normalization as the former UUID members.
Unknown request fields are rejected; endpoint validation retains the existing
name/platform/status/result/peer bounds. Database UUID conversions stay server-side.
HealthResponse covers `/healthz`. Server-specific validation is an extension trait,
not a second wire model. Requests still have a 16 KiB body limit before parsing.

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

The current `infra/Dockerfile` only copies `server/`, and its Docker-specific ignore
file excludes core/proto. The infrastructure owner must include `core/Cargo.toml`,
`core/crates/nc-protocol/` and `proto/` in both the build context and COPY steps
before building the updated server image. Task 1.P3 forbids infra edits. Docker's
engine was unavailable locally, so the ignored database lease-cycle test was not run.
