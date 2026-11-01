# nc-blob

Synchronous local encrypted blob cache. Manifests are local metadata, not a second
network wire format. The caller authenticates/persists manifests, retains keys and
checks nc-policy/nc-tariff before authorizing transfers or work.

## Example

```rust
use nc_blob::{BlobKey, BlobManifest, BlobStore};
# fn example(root: &std::path::Path, secret: [u8; 32]) -> Result<(), nc_blob::Error> {
let key = BlobKey::new(secret); // Obtain from the caller's secure key manager.
let manifest = BlobManifest::with_default_chunks(b"example data")?;
let mut cache = BlobStore::open(root, 16 * 1024 * 1024)?;
cache.put_chunk(&manifest, 0, b"example data", &key)?;
assert!(cache.missing_chunks(&manifest, &key)?.is_empty());
let mut restored = Vec::new();
cache.assemble(&manifest, &key, &mut restored)?;
# Ok(())
# }
```

## Format and validation

- Format version 1; default chunk size 1 MiB, allowed range 64 KiB through 8 MiB.
  Total plaintext is capped at 1 TiB and the chunk list at 1,048,576 entries.
  Deserialization bounds list growth; validate rejects inconsistent length/count,
  invalid chunk size and unsupported version. Every store operation validates.
- Chunk ID: SHA-256(plaintext). Blob ID: SHA-256(concatenated ordered 32-byte chunk
  IDs followed by total_len encoded as an unsigned 64-bit little-endian integer).
  Empty blobs have zero chunks and hash the eight zero length bytes. Chunk size
  and version are validated metadata; they are not additional blob-ID inputs.
- Each file is exactly `nonce[24] || ciphertext[plaintext_len] || tag[16]`.
  XChaCha20-Poly1305 uses a fresh OS-random nonce for every write. Associated data
  is `blob_id[32] || chunk_index_u64_le[8]`. Plaintext length and SHA-256 are checked
  before writing and after authentication on reading.
- A blob directory is its lowercase hex blob ID. Chunk filenames are lowercase
  hex SHA-256 of associated data. This supports repeated identical chunks without
  reusing the index binding. Encryption is per blob/index: global plaintext-hash
  deduplication is intentionally not provided.
- Keys are caller-supplied on put, read, has, missing and assemble. The sketch's
  put/has/missing signatures need this additional argument to encrypt and validate
  authentication. A wrong key makes chunks unavailable; it never deletes them.
  Use one stable key per blob. Key rotation requires rewriting every chunk.
- BlobKey and cipher key storage zeroize on drop. Returned plaintext belongs to
  the caller, which must erase it if needed. No keys, payloads or paths are logged;
  errors retain I/O error kinds instead of path-bearing OS messages.

## Filesystem and operational boundaries

- Give each store a private directory with owner-controlled ancestors/permissions.
  OS file locking permits one open BlobStore per root and fails immediately for a
  second opener. All mutations require a mutable store reference. Locks are released
  on process exit/drop. Other programs must not bypass this ownership contract.
- Existing symbolic links, non-file chunk entries and unrecognized names are
  rejected. Portable std::fs cannot prevent hostile path replacement races;
  shared directories writable by untrusted users are unsupported. Local disk is
  required; network filesystem locking/rename semantics have not been verified.
- Quota is logical stored byte length, including nonce/tag overhead and staging
  files. Filesystem block allocation, directories and the empty lock file are
  excluded. Replacement needs room for the old file and the new staging file;
  a full cache can read/delete but must free space before repairing a chunk.
- Put uses exclusive creation of a random hash-named staging file in the blob
  directory, sync_all, close, then atomic rename. Failures clean up staging when
  possible. Existing published files survive pre-rename failure. This is atomic
  publication, not a guarantee against power-loss directory-entry loss.
- Interrupted writes leave absent final chunks or authenticated complete chunks.
  Crash-left staging is counted in usage; it never satisfies a chunk lookup.
  remove_blob and GC of an unkept blob reclaim it. Staging in kept blobs remains
  charged until removal. Persist manifests/keys outside this dedicated cache root.
- Presence/resume checks authenticate and hash every chunk, so they perform disk
  reads. Corruption is an error on read and absence on has/missing. Permission and
  other filesystem failures are surfaced, not silently treated as missing.
- Assemble buffers a chunk at a time. It can leave a verified prefix in its writer
  if a later chunk or write fails. For all-or-nothing application output, supply
  your own temporary writer and publish only after assemble succeeds.
- Only Windows/MSVC is exercised locally. Linux/macOS execution and physical
  crash/power-loss testing remain CI/integration responsibilities.
