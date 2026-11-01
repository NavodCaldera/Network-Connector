use super::*;
use proptest::prelude::*;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn key() -> BlobKey {
    BlobKey::new([17; 32])
}
fn path(root: &Path, manifest: &BlobManifest, index: usize) -> PathBuf {
    let id = manifest.id().unwrap();
    let mut address = id.0.to_vec();
    address.extend_from_slice(&(index as u64).to_le_bytes());
    root.join(hex::encode(id.0))
        .join(hex::encode(Sha256::digest(address)))
}
fn put_all(store: &mut BlobStore, manifest: &BlobManifest, bytes: &[u8]) {
    for (i, chunk) in bytes.chunks(manifest.chunk_size as usize).enumerate() {
        store.put_chunk(manifest, i, chunk, &key()).unwrap();
    }
}

#[test]
fn small_empty_exact_and_odd_roundtrips() {
    for len in [
        0,
        1,
        333,
        MIN_CHUNK_SIZE as usize * 2,
        MIN_CHUNK_SIZE as usize * 2 + 19,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = BlobStore::open(dir.path(), u64::MAX).unwrap();
        let bytes: Vec<_> = (0..len).map(|i| i as u8).collect();
        let manifest = BlobManifest::from_bytes(&bytes, MIN_CHUNK_SIZE).unwrap();
        put_all(&mut store, &manifest, &bytes);
        let mut output = Vec::new();
        store.assemble(&manifest, &key(), &mut output).unwrap();
        assert_eq!(output, bytes);
        assert!(store.missing_chunks(&manifest, &key()).unwrap().is_empty());
        assert_eq!(
            store.usage().unwrap(),
            len as u64 + 40 * manifest.chunk_ids.len() as u64
        );
    }
}

#[test]
fn resume_after_reopening() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = vec![3; MIN_CHUNK_SIZE as usize * 3 + 1];
    let manifest = BlobManifest::from_bytes(&bytes, MIN_CHUNK_SIZE).unwrap();
    {
        let mut store = BlobStore::open(dir.path(), u64::MAX).unwrap();
        store
            .put_chunk(&manifest, 1, &bytes[..MIN_CHUNK_SIZE as usize], &key())
            .unwrap();
        assert_eq!(
            store.missing_chunks(&manifest, &key()).unwrap(),
            vec![0, 2, 3]
        );
    }
    let mut store = BlobStore::open(dir.path(), u64::MAX).unwrap();
    let missing = store.missing_chunks(&manifest, &key()).unwrap();
    assert_eq!(missing, vec![0, 2, 3]);
    for i in missing {
        let start = i * MIN_CHUNK_SIZE as usize;
        let end = (start + MIN_CHUNK_SIZE as usize).min(bytes.len());
        store
            .put_chunk(&manifest, i, &bytes[start..end], &key())
            .unwrap();
    }
    let mut output = Vec::new();
    store.assemble(&manifest, &key(), &mut output).unwrap();
    assert_eq!(output, bytes);
}

#[test]
fn wrong_key_corruption_and_truncation_are_missing() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = BlobStore::open(dir.path(), 10_000).unwrap();
    let manifest = BlobManifest::with_default_chunks(b"example payload").unwrap();
    put_all(&mut store, &manifest, b"example payload");
    assert!(matches!(
        store.read_chunk(&manifest, 0, &BlobKey::new([8; 32])),
        Err(Error::Corrupt)
    ));
    assert!(!store
        .has_chunk(&manifest, 0, &BlobKey::new([8; 32]))
        .unwrap());
    let file = path(dir.path(), &manifest, 0);
    let original = fs::read(&file).unwrap();
    // Exhaust every byte, including nonce and tag, for a small stored chunk.
    for position in 0..original.len() {
        let mut corrupt = original.clone();
        corrupt[position] ^= 1;
        fs::write(&file, corrupt).unwrap();
        assert!(matches!(
            store.read_chunk(&manifest, 0, &key()),
            Err(Error::Corrupt)
        ));
        assert_eq!(store.missing_chunks(&manifest, &key()).unwrap(), vec![0]);
        let mut output = Vec::new();
        assert!(store.assemble(&manifest, &key(), &mut output).is_err());
        assert!(output.is_empty());
    }
    for length in [0, 1, 23, 24, original.len() - 1] {
        fs::write(&file, &original[..length]).unwrap();
        assert!(matches!(
            store.read_chunk(&manifest, 0, &key()),
            Err(Error::Corrupt)
        ));
        assert!(!store.has_chunk(&manifest, 0, &key()).unwrap());
    }
    let mut oversized = original.clone();
    oversized.push(0);
    fs::write(&file, oversized).unwrap();
    assert!(!store.has_chunk(&manifest, 0, &key()).unwrap());
    put_all(&mut store, &manifest, b"example payload");
    assert!(store.has_chunk(&manifest, 0, &key()).unwrap());
}

#[test]
fn swapping_identical_plaintext_chunks_fails_index_authentication() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = BlobStore::open(dir.path(), u64::MAX).unwrap();
    let bytes = vec![7; MIN_CHUNK_SIZE as usize * 2];
    let manifest = BlobManifest::from_bytes(&bytes, MIN_CHUNK_SIZE).unwrap();
    put_all(&mut store, &manifest, &bytes);
    let first = path(dir.path(), &manifest, 0);
    let second = path(dir.path(), &manifest, 1);
    let a = fs::read(&first).unwrap();
    let b = fs::read(&second).unwrap();
    assert_ne!(a, b);
    fs::write(first, b).unwrap();
    fs::write(second, a).unwrap();
    assert_eq!(store.missing_chunks(&manifest, &key()).unwrap(), vec![0, 1]);
}

#[test]
fn cross_blob_swapping_fails_blob_authentication() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = BlobStore::open(dir.path(), u64::MAX).unwrap();
    let a = vec![7; MIN_CHUNK_SIZE as usize];
    let mut b = a.clone();
    b.push(9);
    let ma = BlobManifest::from_bytes(&a, MIN_CHUNK_SIZE).unwrap();
    let mb = BlobManifest::from_bytes(&b, MIN_CHUNK_SIZE).unwrap();
    put_all(&mut store, &ma, &a);
    put_all(&mut store, &mb, &b);
    fs::copy(path(dir.path(), &ma, 0), path(dir.path(), &mb, 0)).unwrap();
    assert!(!store.has_chunk(&mb, 0, &key()).unwrap());
}

#[test]
fn hash_length_index_and_manifest_rejection_do_not_write() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = BlobStore::open(dir.path(), 1_000).unwrap();
    let manifest = BlobManifest::with_default_chunks(b"abc").unwrap();
    for bytes in [&b"xyz"[..], &b"a"[..]] {
        assert!(matches!(
            store.put_chunk(&manifest, 0, bytes, &key()),
            Err(Error::HashMismatch)
        ));
    }
    assert!(matches!(
        store.put_chunk(&manifest, 1, b"abc", &key()),
        Err(Error::InvalidIndex)
    ));
    assert_eq!(store.usage().unwrap(), 0);
    assert!(matches!(
        store.read_chunk(&manifest, 0, &key()),
        Err(Error::Missing)
    ));
    let mut invalid = manifest;
    invalid.version = 2;
    assert!(matches!(
        store.missing_chunks(&invalid, &key()),
        Err(Error::InvalidManifest)
    ));
    assert!(matches!(
        store.assemble(&invalid, &key(), Vec::new()),
        Err(Error::InvalidManifest)
    ));
}

#[test]
fn quota_includes_overhead_staging_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = BlobManifest::with_default_chunks(b"abc").unwrap();
    {
        let mut store = BlobStore::open(dir.path(), 42).unwrap();
        assert!(matches!(
            store.put_chunk(&manifest, 0, b"abc", &key()),
            Err(Error::Quota)
        ));
        assert_eq!(store.usage().unwrap(), 0);
    }
    {
        let mut store = BlobStore::open(dir.path(), 43).unwrap();
        put_all(&mut store, &manifest, b"abc");
        assert_eq!(store.usage().unwrap(), 43);
        assert!(matches!(
            store.put_chunk(&manifest, 0, b"abc", &key()),
            Err(Error::Quota)
        ));
        assert!(store.has_chunk(&manifest, 0, &key()).unwrap());
    }
    let mut store = BlobStore::open(dir.path(), 0).unwrap();
    assert_eq!(store.usage().unwrap(), 43);
    store.remove_blob(manifest.id().unwrap()).unwrap();
    store.remove_blob(manifest.id().unwrap()).unwrap();
    assert_eq!(store.usage().unwrap(), 0);
}

#[test]
fn gc_keeps_only_listed_blobs_and_counts_orphan_staging() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = BlobStore::open(dir.path(), 1_000).unwrap();
    let a = BlobManifest::with_default_chunks(b"abc").unwrap();
    let b = BlobManifest::with_default_chunks(b"def").unwrap();
    put_all(&mut store, &a, b"abc");
    put_all(&mut store, &b, b"def");
    fs::write(
        path(dir.path(), &b, 0)
            .parent()
            .unwrap()
            .join("00".repeat(32)),
        b"orphan",
    )
    .unwrap();
    assert_eq!(store.usage().unwrap(), 92);
    store.gc(&[a.id().unwrap()]).unwrap();
    assert!(store.has_chunk(&a, 0, &key()).unwrap());
    assert!(!store.has_chunk(&b, 0, &key()).unwrap());
    assert_eq!(store.usage().unwrap(), 43);
    store.gc(&[]).unwrap();
    assert_eq!(store.usage().unwrap(), 0);
}

#[test]
fn manifest_bounds_and_canonical_id_and_serde() {
    let manifest =
        BlobManifest::from_bytes(&vec![0; MIN_CHUNK_SIZE as usize + 1], MIN_CHUNK_SIZE).unwrap();
    let json = serde_json::to_string(&manifest).unwrap();
    assert_eq!(
        serde_json::from_str::<BlobManifest>(&json).unwrap(),
        manifest
    );
    let mut canonical = Vec::new();
    for chunk in &manifest.chunk_ids {
        canonical.extend_from_slice(&chunk.0);
    }
    canonical.extend_from_slice(&manifest.total_len.to_le_bytes());
    assert_eq!(
        manifest.id().unwrap(),
        BlobId(Sha256::digest(canonical).into())
    );
    for size in [0, MIN_CHUNK_SIZE - 1, MAX_CHUNK_SIZE + 1, u32::MAX] {
        assert!(BlobManifest::from_bytes(b"", size).is_err());
        let mut bad = manifest.clone();
        bad.chunk_size = size;
        assert!(bad.validate().is_err());
    }
    for len in [0, MAX_BLOB_LEN + 1, u64::MAX] {
        let mut bad = manifest.clone();
        bad.total_len = len;
        assert!(bad.validate().is_err());
    }
    let mut bad = manifest.clone();
    bad.chunk_ids.clear();
    assert!(bad.validate().is_err());
    let bad = BlobManifest {
        version: 1,
        total_len: (MAX_CHUNKS as u64 + 1) * u64::from(MIN_CHUNK_SIZE),
        chunk_size: MIN_CHUNK_SIZE,
        chunk_ids: vec![ChunkId([0; 32]); MAX_CHUNKS + 1],
    };
    assert!(bad.validate().is_err());
    let mut reversed = manifest.clone();
    reversed.chunk_ids.reverse();
    assert_ne!(manifest.id().unwrap(), reversed.id().unwrap());
    assert_eq!(
        BlobManifest::with_default_chunks(b"").unwrap().chunk_size,
        DEFAULT_CHUNK_SIZE
    );
}

#[test]
fn exclusive_root_lock_and_unmanaged_entries() {
    let dir = tempfile::tempdir().unwrap();
    let store = BlobStore::open(dir.path(), 100).unwrap();
    assert!(matches!(
        BlobStore::open(dir.path(), 100),
        Err(Error::Locked)
    ));
    drop(store);
    let store = BlobStore::open(dir.path(), 100).unwrap();
    fs::write(dir.path().join("unmanaged"), b"x").unwrap();
    assert!(matches!(store.usage(), Err(Error::UnsafeEntry)));
}

#[test]
fn output_failure_propagates() {
    struct Fails;
    impl std::io::Write for Fails {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let mut store = BlobStore::open(dir.path(), 100).unwrap();
    let manifest = BlobManifest::with_default_chunks(b"abc").unwrap();
    put_all(&mut store, &manifest, b"abc");
    assert!(matches!(
        store.assemble(&manifest, &key(), Fails),
        Err(Error::Io(std::io::ErrorKind::BrokenPipe))
    ));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]
    #[test]
    fn randomized_roundtrip(seed in prop::collection::vec(any::<u8>(), 1..1024), len in 0usize..200_000, size in prop_oneof![MIN_CHUNK_SIZE..=2 * MIN_CHUNK_SIZE, MIN_CHUNK_SIZE..=MAX_CHUNK_SIZE]) {
        let bytes: Vec<_> = (0..len).map(|i| seed[i % seed.len()]).collect();
        let manifest = BlobManifest::from_bytes(&bytes, size).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut store = BlobStore::open(dir.path(), u64::MAX).unwrap();
        put_all(&mut store, &manifest, &bytes);
        let mut output = Vec::new();
        store.assemble(&manifest, &key(), &mut output).unwrap();
        prop_assert_eq!(output, bytes);
    }
    #[test]
    fn any_single_byte_corruption_is_detected(bytes in prop::collection::vec(any::<u8>(), 1..8192), position in any::<usize>(), delta in 1u8..=255) {
        let manifest = BlobManifest::with_default_chunks(&bytes).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut store = BlobStore::open(dir.path(), u64::MAX).unwrap();
        put_all(&mut store, &manifest, &bytes);
        let file = path(dir.path(), &manifest, 0);
        let mut sealed = fs::read(&file).unwrap();
        let index = position % sealed.len(); sealed[index] ^= delta;
        fs::write(file, sealed).unwrap();
        prop_assert!(matches!(store.read_chunk(&manifest, 0, &key()), Err(Error::Corrupt)));
        prop_assert!(!store.has_chunk(&manifest, 0, &key()).unwrap());
    }
}

#[test]
fn authenticated_wrong_plaintext_hash_is_rejected() {
    use chacha20poly1305::{
        aead::{Aead, KeyInit, Payload},
        XChaCha20Poly1305,
    };
    let dir = tempfile::tempdir().unwrap();
    let mut store = BlobStore::open(dir.path(), 1_000).unwrap();
    let manifest = BlobManifest::with_default_chunks(b"abc").unwrap();
    put_all(&mut store, &manifest, b"abc");
    let mut aad = manifest.id().unwrap().0.to_vec();
    aad.extend_from_slice(&0u64.to_le_bytes());
    let nonce = [0; 24];
    let cipher = XChaCha20Poly1305::new((&[17; 32]).into());
    let mut sealed = nonce.to_vec();
    sealed.extend(
        cipher
            .encrypt(
                (&nonce).into(),
                Payload {
                    msg: b"xyz",
                    aad: &aad,
                },
            )
            .unwrap(),
    );
    fs::write(path(dir.path(), &manifest, 0), sealed).unwrap();
    assert!(matches!(
        store.read_chunk(&manifest, 0, &key()),
        Err(Error::Corrupt)
    ));
}

#[test]
fn nested_directories_are_rejected_before_gc() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = BlobStore::open(dir.path(), 1_000).unwrap();
    let manifest = BlobManifest::with_default_chunks(b"abc").unwrap();
    put_all(&mut store, &manifest, b"abc");
    let unexpected = path(dir.path(), &manifest, 0)
        .parent()
        .unwrap()
        .join("11".repeat(32));
    fs::create_dir(&unexpected).unwrap();
    assert!(matches!(store.gc(&[]), Err(Error::UnsafeEntry)));
    assert!(unexpected.exists());
    assert!(store.has_chunk(&manifest, 0, &key()).unwrap());
}
