//! Keystore adapter trait and the software keystore.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signer as _, SigningKey};
use zeroize::Zeroizing;

use crate::platform;

/// How well the root key is protected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Assurance {
    /// Software key in a protected file. Lower assurance.
    Software,
    /// Non-exportable hardware key.
    Hardware,
}

/// Errors from identity operations. Never contain key bytes or file contents.
#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    /// File I/O failed.
    #[error("key file I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// The key file is not in a format we wrote.
    #[error("key file is corrupt or from another format")]
    Corrupt,
    /// OS protection (DPAPI) failed.
    #[error("key protection failed: {0}")]
    Protect(String),
    /// No randomness available.
    #[error("no system randomness")]
    Random,
    /// Certificate checks failed.
    #[error("certificate refused: {0}")]
    Cert(&'static str),
    /// Signature or encoding error.
    #[error(transparent)]
    Protocol(#[from] nc_protocol::ProtocolError),
}

/// A device root key held somewhere the caller cannot read it directly.
pub trait DeviceKeyStore: Send + Sync {
    /// Protection level.
    fn assurance(&self) -> Assurance;
    /// Ed25519 public key.
    fn public_key(&self) -> [u8; 32];
    /// Sign `msg` with the root key.
    fn sign(&self, msg: &[u8]) -> Result<[u8; 64], IdentityError>;
}

/// Adapter: lets any [`DeviceKeyStore`] sign protocol objects (`nc_protocol::Signer`).
pub struct RootSigner<'a>(pub &'a dyn DeviceKeyStore);

impl nc_protocol::Signer for RootSigner<'_> {
    fn public_key(&self) -> [u8; 32] {
        self.0.public_key()
    }
    fn sign(&self, msg: &[u8]) -> Result<[u8; 64], nc_protocol::ProtocolError> {
        self.0
            .sign(msg)
            .map_err(|e| nc_protocol::ProtocolError::Sign(e.to_string()))
    }
}

impl nc_protocol::Signer for SoftwareKeyStore {
    fn public_key(&self) -> [u8; 32] {
        DeviceKeyStore::public_key(self)
    }
    fn sign(&self, msg: &[u8]) -> Result<[u8; 64], nc_protocol::ProtocolError> {
        Ok(self.key.sign(msg).to_bytes())
    }
}

const MAGIC: &[u8; 4] = b"NCK1";

/// Software root key in a protected file.
pub struct SoftwareKeyStore {
    key: SigningKey,
    path: PathBuf,
}

impl std::fmt::Debug for SoftwareKeyStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the key; the path is also kept out of Debug (AGENT.md §1 rule 10).
        f.debug_struct("SoftwareKeyStore").finish_non_exhaustive()
    }
}

pub(crate) fn random_seed() -> Result<Zeroizing<[u8; 32]>, IdentityError> {
    let mut seed = Zeroizing::new([0u8; 32]);
    getrandom::fill(seed.as_mut()).map_err(|_| IdentityError::Random)?;
    Ok(seed)
}

impl SoftwareKeyStore {
    /// Load the key at `path`, or create a new one there if the file does not exist.
    pub fn open_or_create(path: impl Into<PathBuf>) -> Result<Self, IdentityError> {
        let path = path.into();
        match fs::read(&path) {
            Ok(bytes) => Self::load(path, &Zeroizing::new(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::create(path),
            Err(e) => Err(e.into()),
        }
    }

    fn load(path: PathBuf, bytes: &[u8]) -> Result<Self, IdentityError> {
        if bytes.len() < 5 || &bytes[..4] != MAGIC || bytes[4] != platform::SCHEME {
            return Err(IdentityError::Corrupt);
        }
        let seed = platform::unprotect(&bytes[5..])?;
        let seed: &[u8; 32] = seed
            .as_slice()
            .try_into()
            .map_err(|_| IdentityError::Corrupt)?;
        Ok(Self {
            key: SigningKey::from_bytes(seed),
            path,
        })
    }

    fn create(path: PathBuf) -> Result<Self, IdentityError> {
        let seed = random_seed()?;
        let sealed = platform::protect(seed.as_ref())?;
        let mut file = Vec::with_capacity(5 + sealed.len());
        file.extend_from_slice(MAGIC);
        file.push(platform::SCHEME);
        file.extend_from_slice(&sealed);
        write_new_file(&path, &file)?;
        Ok(Self {
            key: SigningKey::from_bytes(&seed),
            path,
        })
    }

    /// Where the key file lives.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Write `data` to a new file next to `path`, then rename into place (no partially written key files).
fn write_new_file(path: &Path, data: &[u8]) -> Result<(), IdentityError> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

impl DeviceKeyStore for SoftwareKeyStore {
    fn assurance(&self) -> Assurance {
        Assurance::Software
    }

    fn public_key(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }

    fn sign(&self, msg: &[u8]) -> Result<[u8; 64], IdentityError> {
        Ok(self.key.sign(msg).to_bytes())
    }
}
