use crate::{BlobId, BlobKey, BlobManifest, ChunkId, Error};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305,
};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
use zeroize::{Zeroize, Zeroizing};

const OVERHEAD: usize = 24 + 16;

/// Exclusively owned cache root. Closing/dropping releases its OS file lock.
///
/// Use a private, trusted directory: hostile concurrent filesystem mutation is
/// outside this std::fs implementation's threat model. Mutation requires `&mut self`.
/// Quota measures file lengths (including staging), not filesystem allocation units.
/// Keys are never persisted. The caller persists authenticated manifests separately.
pub struct BlobStore {
    root: PathBuf,
    quota_bytes: u64,
    _lock: File,
}

struct Staged(Option<PathBuf>);
impl Drop for Staged {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}

fn regular(path: &Path, directory: bool) -> Result<(), Error> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink()
        || (directory && !meta.is_dir())
        || (!directory && !meta.is_file())
    {
        return Err(Error::UnsafeEntry);
    }
    Ok(())
}

fn hash_name(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn valid_name(path: &Path) -> bool {
    path.file_name().and_then(|s| s.to_str()).is_some_and(|s| {
        s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn aad(id: BlobId, index: usize) -> [u8; 40] {
    let mut bytes = [0; 40];
    bytes[..32].copy_from_slice(&id.0);
    bytes[32..].copy_from_slice(&(index as u64).to_le_bytes());
    bytes
}

impl BlobStore {
    /// Open/create a private cache root and acquire an exclusive, nonblocking lock.
    /// Existing contents may exceed the quota; reads/removal remain available.
    pub fn open(root_dir: impl AsRef<Path>, quota_bytes: u64) -> Result<Self, Error> {
        let root = root_dir.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;
        regular(&root, true)?;
        let lock_path = root.join(hash_name(b"nc-blob root lock v1"));
        match fs::symlink_metadata(&lock_path) {
            Ok(_) => regular(&lock_path, false)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)?;
        lock.try_lock().map_err(|_| Error::Locked)?;
        let store = Self {
            root,
            quota_bytes,
            _lock: lock,
        };
        store.usage()?;
        Ok(store)
    }

    fn directory(&self, id: BlobId) -> PathBuf {
        self.root.join(hex::encode(id.0))
    }

    fn chunk_path(&self, id: BlobId, index: usize) -> PathBuf {
        self.directory(id).join(hash_name(&aad(id, index)))
    }
    /// Hash-check and encrypt a chunk, then publish it via a same-directory atomic rename.
    /// The key is explicit because encryption cannot be performed without it.
    /// Staging must fit the quota even when replacing an existing chunk.
    pub fn put_chunk(
        &mut self,
        manifest: &BlobManifest,
        index: usize,
        plaintext: &[u8],
        key: &BlobKey,
    ) -> Result<(), Error> {
        let len = manifest.chunk_len(index)?;
        if plaintext.len() != len || ChunkId::of(plaintext) != manifest.chunk_ids[index] {
            return Err(Error::HashMismatch);
        }
        let sealed_len = (len + OVERHEAD) as u64;
        if self
            .usage()?
            .checked_add(sealed_len)
            .is_none_or(|n| n > self.quota_bytes)
        {
            return Err(Error::Quota);
        }
        let id = manifest.id()?;
        let directory = self.directory(id);
        fs::create_dir_all(&directory)?;
        regular(&directory, true)?;
        let destination = self.chunk_path(id, index);
        match fs::symlink_metadata(&destination) {
            Ok(_) => regular(&destination, false)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let mut nonce = [0; 24];
        getrandom::fill(&mut nonce).map_err(|_| Error::Random)?;
        let cipher = XChaCha20Poly1305::new((&*key.0).into());
        let associated = aad(id, index);
        let ciphertext = cipher
            .encrypt(
                (&nonce).into(),
                Payload {
                    msg: plaintext,
                    aad: &associated,
                },
            )
            .map_err(|_| Error::Corrupt)?;
        let mut random_name = [0; 32];
        getrandom::fill(&mut random_name).map_err(|_| Error::Random)?;
        let temporary = directory.join(hash_name(&random_name));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        let mut staged = Staged(Some(temporary.clone()));
        file.write_all(&nonce)?;
        file.write_all(&ciphertext)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, destination)?;
        staged.0 = None;
        Ok(())
    }

    /// Return a verified plaintext chunk. Wrong keys and corrupt bytes return errors.
    /// Allocation is bounded by the validated chunk length, even for hostile disk files.
    pub fn read_chunk(
        &self,
        manifest: &BlobManifest,
        index: usize,
        key: &BlobKey,
    ) -> Result<Vec<u8>, Error> {
        manifest.chunk_len(index)?;
        self.read_verified(manifest, index, key, manifest.id()?)
    }

    fn read_verified(
        &self,
        manifest: &BlobManifest,
        index: usize,
        key: &BlobKey,
        id: BlobId,
    ) -> Result<Vec<u8>, Error> {
        let len = manifest.chunk_len(index)?;
        let directory = self.directory(id);
        let path = self.chunk_path(id, index);
        for (entry, is_dir) in [(&directory, true), (&path, false)] {
            match regular(entry, is_dir) {
                Err(Error::Io(std::io::ErrorKind::NotFound)) => return Err(Error::Missing),
                result => result?,
            }
        }
        let mut file = File::open(path)?;
        if file.metadata()?.len() != (len + OVERHEAD) as u64 {
            return Err(Error::Corrupt);
        }
        let mut sealed = vec![0; len + OVERHEAD];
        match file.read_exact(&mut sealed) {
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Err(Error::Corrupt),
            result => result?,
        }
        let mut nonce = [0; 24];
        nonce.copy_from_slice(&sealed[..24]);
        let cipher = XChaCha20Poly1305::new((&*key.0).into());
        let associated = aad(id, index);
        let mut plaintext = cipher
            .decrypt(
                (&nonce).into(),
                Payload {
                    msg: &sealed[24..],
                    aad: &associated,
                },
            )
            .map_err(|_| Error::Corrupt)?;
        if plaintext.len() != len || ChunkId::of(&plaintext) != manifest.chunk_ids[index] {
            plaintext.zeroize();
            return Err(Error::Corrupt);
        }
        Ok(plaintext)
    }

    /// Authenticate presence; corrupt/truncated chunks (or a wrong key) return false.
    /// Other I/O errors and unsafe filesystem entries remain errors.
    pub fn has_chunk(
        &self,
        manifest: &BlobManifest,
        index: usize,
        key: &BlobKey,
    ) -> Result<bool, Error> {
        match self.read_chunk(manifest, index, key) {
            Ok(bytes) => {
                let _verified = Zeroizing::new(bytes);
                Ok(true)
            }
            Err(Error::Missing | Error::Corrupt) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// List absent or unauthentic chunks in manifest order for resumable transfer.
    pub fn missing_chunks(
        &self,
        manifest: &BlobManifest,
        key: &BlobKey,
    ) -> Result<Vec<usize>, Error> {
        let id = manifest.id()?;
        let mut missing = Vec::new();
        for index in 0..manifest.chunk_ids.len() {
            match self.read_verified(manifest, index, key, id) {
                Ok(bytes) => {
                    let _verified = Zeroizing::new(bytes);
                }
                Err(Error::Missing | Error::Corrupt) => missing.push(index),
                Err(e) => return Err(e),
            }
        }
        Ok(missing)
    }

    /// Stream verified chunks to the writer, using at most one plaintext chunk at once.
    /// A later failure can leave an already verified prefix in the caller's writer.
    pub fn assemble(
        &self,
        manifest: &BlobManifest,
        key: &BlobKey,
        mut writer: impl Write,
    ) -> Result<(), Error> {
        let id = manifest.id()?;
        for index in 0..manifest.chunk_ids.len() {
            let bytes = Zeroizing::new(self.read_verified(manifest, index, key, id)?);
            writer.write_all(&bytes)?;
        }
        Ok(())
    }

    fn directories(&self) -> Result<Vec<PathBuf>, Error> {
        regular(&self.root, true)?;
        let lock_name = hash_name(b"nc-blob root lock v1");
        let mut directories = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let path = entry?.path();
            if path.file_name().is_some_and(|s| s == lock_name.as_str()) {
                regular(&path, false)?;
                continue;
            }
            if !valid_name(&path) {
                return Err(Error::UnsafeEntry);
            }
            regular(&path, true)?;
            for child in fs::read_dir(&path)? {
                let child = child?.path();
                if !valid_name(&child) {
                    return Err(Error::UnsafeEntry);
                }
                regular(&child, false)?;
            }
            directories.push(path);
        }
        Ok(directories)
    }

    /// Sum stored file lengths, including ciphertext overhead and abandoned staging.
    /// Directory metadata, allocation slack and the zero-length lock file are excluded.
    pub fn usage(&self) -> Result<u64, Error> {
        let mut total = 0_u64;
        for directory in self.directories()? {
            for entry in fs::read_dir(directory)? {
                total = total
                    .checked_add(entry?.metadata()?.len())
                    .ok_or(Error::Quota)?;
            }
        }
        Ok(total)
    }

    /// Remove all cached chunks/staging for a blob. Missing blobs are a no-op.
    pub fn remove_blob(&mut self, id: BlobId) -> Result<(), Error> {
        // Validate the complete tree before recursive deletion: no symlink traversal.
        let directories = self.directories()?;
        let path = self.directory(id);
        if directories.contains(&path) {
            fs::remove_dir_all(path)?;
        }
        Ok(())
    }

    /// Remove all blob directories except the supplied identifiers.
    /// Kept blobs retain abandoned staging files until their removal; these count in quota.
    pub fn gc(&mut self, keep: &[BlobId]) -> Result<(), Error> {
        let keep: std::collections::HashSet<_> =
            keep.iter().map(|id| self.directory(*id)).collect();
        for directory in self.directories()? {
            if !keep.contains(&directory) {
                fs::remove_dir_all(directory)?;
            }
        }
        Ok(())
    }
}
