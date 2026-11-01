//! Platform protection for the software key file.
//!
//! Windows: DPAPI, current-user scope, with application entropy, UI forbidden. The file can only be decrypted by
//! the same Windows user (on this machine, or with a roaming profile). Elsewhere: raw seed in a 0600 file.
//! TODO(core, Phase 1): Linux kernel keyring / Secret Service, macOS Keychain.

#[cfg(windows)]
pub(crate) use self::dpapi::{protect, unprotect, SCHEME};
#[cfg(not(windows))]
pub(crate) use self::plain::{protect, unprotect, SCHEME};

#[cfg(windows)]
#[allow(unsafe_code)] // AGENT.md §1 rule 11: platform adapter; every block has a SAFETY comment.
mod dpapi {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };
    use zeroize::Zeroizing;

    use crate::IdentityError;

    /// File scheme byte for DPAPI-sealed seeds.
    pub(crate) const SCHEME: u8 = 1;
    const ENTROPY: &[u8] = b"nc-identity device root v1";

    fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        }
    }

    /// Copy a DPAPI output blob into owned, zeroising memory and free the original.
    fn take(out: CRYPT_INTEGER_BLOB) -> Zeroizing<Vec<u8>> {
        // SAFETY: on success DPAPI returns a LocalAlloc'd buffer of exactly cbData bytes.
        let v = Zeroizing::new(unsafe {
            std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec()
        });
        // SAFETY: the buffer was allocated by DPAPI with LocalAlloc; wipe then free it exactly once.
        unsafe {
            std::ptr::write_bytes(out.pbData, 0, out.cbData as usize);
            let _ = LocalFree(Some(HLOCAL(out.pbData.cast())));
        }
        v
    }

    pub(crate) fn protect(seed: &[u8]) -> Result<Vec<u8>, IdentityError> {
        let input = blob(seed);
        let entropy = blob(ENTROPY);
        let mut out = CRYPT_INTEGER_BLOB::default();
        // SAFETY: input/entropy point to live slices for the call; `out` is a valid out-pointer.
        unsafe {
            CryptProtectData(
                &input,
                PCWSTR::null(),
                Some(&entropy),
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        }
        .map_err(|e| IdentityError::Protect(format!("CryptProtectData: {}", e.code())))?;
        Ok(take(out).to_vec())
    }

    pub(crate) fn unprotect(sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>, IdentityError> {
        let input = blob(sealed);
        let entropy = blob(ENTROPY);
        let mut out = CRYPT_INTEGER_BLOB::default();
        // SAFETY: as in `protect`; no description out-pointer requested.
        unsafe {
            CryptUnprotectData(
                &input,
                None,
                Some(&entropy),
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        }
        .map_err(|_| IdentityError::Corrupt)?;
        Ok(take(out))
    }
}

#[cfg(not(windows))]
mod plain {
    use zeroize::Zeroizing;

    use crate::IdentityError;

    /// File scheme byte for an unsealed seed in a 0600 file.
    pub(crate) const SCHEME: u8 = 0;

    pub(crate) fn protect(seed: &[u8]) -> Result<Vec<u8>, IdentityError> {
        Ok(seed.to_vec())
    }

    pub(crate) fn unprotect(sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>, IdentityError> {
        Ok(Zeroizing::new(sealed.to_vec()))
    }
}
