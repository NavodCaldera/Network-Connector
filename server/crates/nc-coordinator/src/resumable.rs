//! 1.L5: resumable chunked uploads into the blob store (exact wire contract in `proto/README.md`).
//!
//! A session is keyed by (calling device, final sha256, final size) and lives as one append-only
//! partial file in the blob directory. Each chunk is signed (NC-REQ-V1 over the chunk bytes, digest
//! sent in `x-nc-chunk-sha256` so the signature is checked before any body byte is read), capped,
//! hash-checked in memory and only then appended at the exact offset. The last chunk triggers a
//! full-blob hash check and the same publication as the single PUT. Abandoned partials are
//! garbage-collected by age ([`gc`]).
use super::*;
use axum::{
    extract::Path,
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use nc_protocol::jobs::MAX_TRANSFER;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Largest chunk body accepted by one PUT.
pub const MAX_CHUNK: u64 = 4 * 1024 * 1024;
/// Open partial uploads one device may hold at once.
pub const MAX_SESSIONS_PER_DEVICE: usize = 8;
/// A partial not appended to for this long is deleted by [`gc`].
pub const PARTIAL_TTL: Duration = Duration::from_secs(24 * 3600);
/// Response header carrying the number of bytes the server holds for the session.
pub const OFFSET_HEADER: &str = "x-nc-upload-offset";
/// Response header set to `true` once the blob is published.
pub const COMPLETE_HEADER: &str = "x-nc-upload-complete";
/// Request header: lowercase hex sha256 of this chunk's body (it is the signed body digest).
pub const CHUNK_HASH_HEADER: &str = "x-nc-chunk-sha256";

const PARTIAL_PREFIX: &str = ".partial-";
const STAGING_PREFIX: &str = ".upload-";

/// Sessions with a request in flight (single coordinator process).
#[derive(Clone, Default)]
pub struct UploadLocks(Arc<Mutex<HashSet<String>>>);

struct Held(UploadLocks, String);
impl Drop for Held {
    fn drop(&mut self) {
        if let Ok(mut g) = (self.0).0.lock() {
            g.remove(&self.1);
        }
    }
}
impl UploadLocks {
    fn try_hold(&self, key: &str) -> Result<Held, ApiError> {
        let mut g = self.0.lock().map_err(|_| ApiError::internal())?;
        if !g.insert(key.to_owned()) {
            return Err(ApiError::conflict(
                "upload_busy",
                "another request for this upload is in progress",
            ));
        }
        Ok(Held(self.clone(), key.to_owned()))
    }
    fn is_held(&self, key: &str) -> bool {
        self.0.lock().map(|g| g.contains(key)).unwrap_or(true)
    }
}

fn partial_name(device: Uuid, hash: &str, size: u64) -> String {
    format!("{PARTIAL_PREFIX}{}-{hash}-{size}", device.simple())
}

fn parse_size(text: &str) -> Result<u64, ApiError> {
    if text.is_empty() || text.len() > 20 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ApiError::bad_request("invalid size or offset"));
    }
    if text.len() > 1 && text.starts_with('0') {
        return Err(ApiError::bad_request("invalid size or offset"));
    }
    text.parse()
        .map_err(|_| ApiError::bad_request("invalid size or offset"))
}

fn session_args(hash: &str, size: &str) -> Result<u64, ApiError> {
    if !blobs::valid_hash(hash) {
        return Err(ApiError::bad_request("invalid sha256"));
    }
    let size = parse_size(size)?;
    if size == 0 {
        return Err(ApiError::bad_request(
            "resumable uploads need size >= 1; use the single PUT for an empty blob",
        ));
    }
    if size > MAX_TRANSFER {
        return Err(ApiError::too_large());
    }
    Ok(size)
}

fn with_offset(status: StatusCode, offset: u64, complete: bool) -> Result<Response, ApiError> {
    let mut r = status.into_response();
    let h = r.headers_mut();
    h.insert(
        OFFSET_HEADER,
        offset
            .to_string()
            .parse()
            .map_err(|_| ApiError::internal())?,
    );
    if complete {
        h.insert(COMPLETE_HEADER, HeaderValue::from_static("true"));
    }
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    Ok(r)
}

async fn held_len(path: &std::path::Path) -> Result<u64, ApiError> {
    match tokio::fs::metadata(path).await {
        Ok(m) => Ok(m.len()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e.into()),
    }
}

async fn device_sessions(dir: &std::path::Path, device: Uuid) -> Result<usize, ApiError> {
    let prefix = format!("{PARTIAL_PREFIX}{}-", device.simple());
    let mut n = 0;
    let mut rd = tokio::fs::read_dir(dir).await?;
    while let Some(e) = rd.next_entry().await? {
        if e.file_name().to_string_lossy().starts_with(&prefix) {
            n += 1;
        }
    }
    Ok(n)
}

/// `GET /v1/blobs/{hash}/upload/{size}`: how many bytes the server holds for the caller's session.
pub async fn status(
    State(s): State<AppState>,
    path: Result<Path<(String, String)>, axum::extract::rejection::PathRejection>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    let Path((hash, size)) = path.map_err(|_| ApiError::bad_request("invalid upload path"))?;
    let size = session_args(&hash, &size)?;
    let (c, b) = read_signed(&s, &method, &uri, &headers, body).await?;
    if !b.is_empty() {
        return Err(ApiError::bad_request("GET body must be empty"));
    }
    let d = device(&s, &c).await?;
    let held = held_len(&s.blob_dir.join(partial_name(d.id, &hash, size))).await?;
    with_offset(StatusCode::OK, held.min(size), false)
}

/// `PUT /v1/blobs/{hash}/upload/{size}/{offset}`: append one chunk at exactly `offset`.
pub async fn put_chunk(
    State(s): State<AppState>,
    path: Result<Path<(String, String, String)>, axum::extract::rejection::PathRejection>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    let Path((hash, size, offset)) =
        path.map_err(|_| ApiError::bad_request("invalid upload path"))?;
    let size = session_args(&hash, &size)?;
    let offset = parse_size(&offset)?;
    let chunk_hash = headers
        .get(CHUNK_HASH_HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|h| blobs::valid_hash(h))
        .ok_or_else(|| ApiError::bad_request("missing or invalid x-nc-chunk-sha256"))?
        .to_owned();
    // Authenticate before reading any body byte; the digest is re-checked against the body below.
    let c = auth::verify_digest(&method, uri.path(), &headers, &chunk_hash, now())?;
    record_nonce(&s, &c, now()).await?;
    let d = device(&s, &c).await?;
    if offset >= size {
        return Err(ApiError::bad_request("offset must be below size"));
    }
    let room = (size - offset).min(MAX_CHUNK);
    if headers.get(header::CONTENT_LENGTH).is_some_and(|v| {
        v.to_str()
            .ok()
            .and_then(|t| t.parse::<u64>().ok())
            .is_none_or(|n| n > room)
    }) {
        return Err(ApiError::too_large());
    }
    let name = partial_name(d.id, &hash, size);
    let _held = s.uploads.try_hold(&name)?;
    let file_path = s.blob_dir.join(&name);
    let held = held_len(&file_path).await?;
    if held != offset {
        let mut r = with_offset(StatusCode::CONFLICT, held.min(size), false)?;
        let e = ApiError::conflict(
            "upload_offset",
            "offset does not match the bytes held; resume from x-nc-upload-offset",
        )
        .into_response();
        *r.body_mut() = e.into_body();
        r.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        return Ok(r);
    }
    if held == 0 && device_sessions(&s.blob_dir, d.id).await? >= MAX_SESSIONS_PER_DEVICE {
        return Err(ApiError::conflict(
            "upload_limit",
            "too many unfinished uploads for this device",
        ));
    }
    // Room is at most MAX_CHUNK (4 MiB), so buffering the chunk is bounded.
    let data = axum::body::to_bytes(body, room as usize)
        .await
        .map_err(|_| ApiError::too_large())?;
    if data.is_empty() {
        return Err(ApiError::bad_request("empty chunk"));
    }
    if hex::encode(Sha256::digest(&data)) != chunk_hash {
        return Err(ApiError::bad_request("chunk hash mismatch"));
    }
    append(&file_path, offset, &data).await?;
    let end = offset + data.len() as u64;
    if end < size {
        return with_offset(StatusCode::OK, end, false);
    }
    // Last chunk: check the whole blob, then publish. Any failure discards the session.
    let result = async {
        if file_sha256(&file_path).await? != hash {
            return Err(ApiError::bad_request(
                "blob hash mismatch; upload discarded",
            ));
        }
        blobs::publish(&s, &hash, size, d.id, &file_path).await
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&file_path).await;
    }
    result?;
    let _ = tokio::fs::remove_file(&file_path).await;
    with_offset(StatusCode::OK, size, true)
}

async fn append(path: &std::path::Path, offset: u64, data: &[u8]) -> Result<(), ApiError> {
    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    let r = async {
        f.write_all(data).await?;
        f.sync_all().await
    }
    .await;
    if let Err(e) = r {
        // Roll back a torn append so the held length stays a verified prefix.
        let _ = f.set_len(offset).await;
        return Err(e.into());
    }
    Ok(())
}

async fn file_sha256(path: &std::path::Path) -> Result<String, ApiError> {
    let mut f = tokio::fs::File::open(path).await?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

/// Deletes partial uploads (and single-PUT staging files left by a killed process) whose last
/// write is older than `max_age`. Sessions with a request in flight are skipped. Returns the
/// number of files removed. File names are never logged.
pub async fn gc(dir: &std::path::Path, locks: &UploadLocks, max_age: Duration) -> usize {
    let Ok(mut rd) = tokio::fs::read_dir(dir).await else {
        return 0;
    };
    let mut removed = 0;
    while let Ok(Some(e)) = rd.next_entry().await {
        let name = e.file_name().to_string_lossy().into_owned();
        if !(name.starts_with(PARTIAL_PREFIX) || name.starts_with(STAGING_PREFIX))
            || locks.is_held(&name)
        {
            continue;
        }
        let old = e
            .metadata()
            .await
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age >= max_age);
        if old && tokio::fs::remove_file(e.path()).await.is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn numbers_are_strict() {
        assert_eq!(parse_size("0").ok(), Some(0));
        assert_eq!(parse_size("16777216").ok(), Some(16 * 1024 * 1024));
        for bad in ["", "01", "-1", "+1", "1e3", "99999999999999999999999", " 1"] {
            assert!(parse_size(bad).is_err(), "{bad}");
        }
        let h = "a".repeat(64);
        assert!(session_args(&h, "0").is_err());
        assert!(session_args(&h, &(MAX_TRANSFER + 1).to_string()).is_err());
        assert!(session_args("ABC", "1").is_err());
        assert_eq!(session_args(&h, "5").ok(), Some(5));
    }

    #[test]
    fn locks_are_exclusive_and_released() {
        let l = UploadLocks::default();
        let a = l.try_hold("k").unwrap();
        assert!(l.try_hold("k").is_err());
        assert!(l.is_held("k"));
        drop(a);
        assert!(!l.is_held("k"));
        assert!(l.try_hold("k").is_ok());
    }

    #[tokio::test]
    async fn append_hash_and_gc() {
        let root = std::env::temp_dir().join(format!("nc-resume-test-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let p = root.join(partial_name(Uuid::new_v4(), &"b".repeat(64), 6));
        append(&p, 0, b"mod").await.unwrap();
        append(&p, 3, b"ule").await.unwrap();
        assert_eq!(held_len(&p).await.unwrap(), 6);
        assert_eq!(
            file_sha256(&p).await.unwrap(),
            hex::encode(Sha256::digest(b"module"))
        );
        std::fs::write(root.join("keepme"), b"x").unwrap();
        std::fs::write(root.join(".upload-x"), b"x").unwrap();
        let locks = UploadLocks::default();
        // Fresh files survive a normal TTL.
        assert_eq!(gc(&root, &locks, PARTIAL_TTL).await, 0);
        // Held sessions are skipped even when expired.
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        let held = locks.try_hold(&name).unwrap();
        assert_eq!(gc(&root, &locks, Duration::ZERO).await, 1);
        assert!(p.exists());
        drop(held);
        assert_eq!(gc(&root, &locks, Duration::ZERO).await, 1);
        assert!(!p.exists());
        assert!(root.join("keepme").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
