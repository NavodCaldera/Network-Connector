use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

/// Default plaintext chunk size: one MiB.
pub const DEFAULT_CHUNK_SIZE: u32 = 1024 * 1024;
/// Smallest supported plaintext chunk size.
pub const MIN_CHUNK_SIZE: u32 = 64 * 1024;
/// Largest supported plaintext chunk size.
pub const MAX_CHUNK_SIZE: u32 = 8 * 1024 * 1024;
/// Maximum blob size: one TiB.
pub const MAX_BLOB_LEN: u64 = 1024 * 1024 * 1024 * 1024;
/// Maximum number of chunk identifiers in a manifest.
pub const MAX_CHUNKS: usize = 1_048_576;

/// SHA-256 identifier of plaintext chunk bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChunkId(pub [u8; 32]);

impl ChunkId {
    /// Hash plaintext bytes.
    pub fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }
}

/// SHA-256 identifier of ordered chunk hashes followed by the LE u64 blob length.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlobId(pub [u8; 32]);

/// Caller-managed per-blob encryption key. Zeroized on drop; never formatted.
///
/// The caller must also protect and erase any original key copies it retains.
pub struct BlobKey(pub(crate) Zeroizing<[u8; 32]>);

impl BlobKey {
    /// Take ownership of a 32-byte encryption key.
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(Zeroizing::new(bytes))
    }
}

/// Versioned local cache metadata. Validate before using externally supplied data.
///
/// Deserialization bounds the chunk list; each store operation validates all fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobManifest {
    /// Format version; currently only 1 is accepted.
    pub version: u32,
    /// Total plaintext length in bytes.
    pub total_len: u64,
    /// Fixed plaintext chunk size, except the last chunk may be shorter.
    pub chunk_size: u32,
    /// Plaintext hashes in stream order, including repeated hashes.
    #[serde(deserialize_with = "bounded_chunks")]
    pub chunk_ids: Vec<ChunkId>,
}

fn bounded_chunks<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<ChunkId>, D::Error> {
    struct Visitor;
    impl<'de> serde::de::Visitor<'de> for Visitor {
        type Value = Vec<ChunkId>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a bounded sequence of chunk hashes")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            if seq.size_hint().is_some_and(|n| n > MAX_CHUNKS) {
                return Err(serde::de::Error::custom("too many chunk hashes"));
            }
            let mut chunks = Vec::new();
            while let Some(chunk) = seq.next_element()? {
                if chunks.len() == MAX_CHUNKS {
                    return Err(serde::de::Error::custom("too many chunk hashes"));
                }
                chunks.push(chunk);
            }
            Ok(chunks)
        }
    }
    d.deserialize_seq(Visitor)
}

impl BlobManifest {
    /// Construct a manifest from plaintext with an explicit chunk size.
    pub fn from_bytes(bytes: &[u8], chunk_size: u32) -> Result<Self, Error> {
        if !(MIN_CHUNK_SIZE..=MAX_CHUNK_SIZE).contains(&chunk_size)
            || bytes.len() as u64 > MAX_BLOB_LEN
            || bytes.len().div_ceil(chunk_size as usize) > MAX_CHUNKS
        {
            return Err(Error::InvalidManifest);
        }
        let manifest = Self {
            version: 1,
            total_len: bytes.len() as u64,
            chunk_size,
            chunk_ids: bytes.chunks(chunk_size as usize).map(ChunkId::of).collect(),
        };
        manifest.validate()?;
        Ok(manifest)
    }

    /// Construct a manifest using one MiB chunks.
    pub fn with_default_chunks(bytes: &[u8]) -> Result<Self, Error> {
        Self::from_bytes(bytes, DEFAULT_CHUNK_SIZE)
    }

    /// Reject unsupported versions, excessive sizes and inconsistent lengths/counts.
    pub fn validate(&self) -> Result<(), Error> {
        if self.version != 1
            || !(MIN_CHUNK_SIZE..=MAX_CHUNK_SIZE).contains(&self.chunk_size)
            || self.total_len > MAX_BLOB_LEN
            || self.chunk_ids.len() > MAX_CHUNKS
            || self.total_len.div_ceil(u64::from(self.chunk_size)) != self.chunk_ids.len() as u64
        {
            return Err(Error::InvalidManifest);
        }
        Ok(())
    }

    /// Compute the canonical identifier after validation.
    pub fn id(&self) -> Result<BlobId, Error> {
        self.validate()?;
        let mut hash = Sha256::new();
        for chunk in &self.chunk_ids {
            hash.update(chunk.0);
        }
        hash.update(self.total_len.to_le_bytes());
        Ok(BlobId(hash.finalize().into()))
    }

    pub(crate) fn chunk_len(&self, index: usize) -> Result<usize, Error> {
        self.validate()?;
        if index >= self.chunk_ids.len() {
            return Err(Error::InvalidIndex);
        }
        Ok((self.total_len - index as u64 * u64::from(self.chunk_size))
            .min(u64::from(self.chunk_size)) as usize)
    }
}

/// Cache failures; messages never contain keys, plaintext or filesystem paths.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Invalid or excessive manifest metadata.
    #[error("The blob manifest has an unsupported version, size or chunk count.")]
    InvalidManifest,
    /// Chunk index is outside the manifest.
    #[error("The chunk index is outside the manifest.")]
    InvalidIndex,
    /// Plaintext differs from the manifest.
    #[error("The chunk length or plaintext hash does not match the manifest.")]
    HashMismatch,
    /// Authentication, length or stored plaintext hash verification failed.
    #[error("The stored chunk is corrupt or the key is incorrect.")]
    Corrupt,
    /// Missing stored chunk.
    #[error("The chunk is not present in the cache.")]
    Missing,
    /// Quota includes ciphertext overhead and staging files.
    #[error("The cache quota cannot accommodate the staged chunk.")]
    Quota,
    /// Untrusted filesystem entry or symbolic link encountered.
    #[error("The cache contains an unsupported filesystem entry.")]
    UnsafeEntry,
    /// Another open store owns this root.
    #[error("The cache root is already locked or cannot be locked.")]
    Locked,
    /// OS entropy was unavailable.
    #[error("Secure random bytes are unavailable.")]
    Random,
    /// Filesystem or output-writer error, without sensitive path text.
    #[error("A cache or output I/O operation failed ({0:?}).")]
    Io(std::io::ErrorKind),
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.kind())
    }
}
