//! Opaque content-addressed uploads; authentication precedes disk writes.
use super::*;
use axum::{
    extract::Path,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use http_body_util::BodyExt;
use nc_protocol::jobs::MAX_TRANSFER;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio_util::io::ReaderStream;

pub fn valid_hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
struct Staging(std::path::PathBuf);
impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub async fn upload(
    State(s): State<AppState>,
    Path(hash): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    mut body: Body,
) -> Result<Response, ApiError> {
    if !valid_hash(&hash) {
        return Err(ApiError::bad_request("invalid sha256"));
    }
    let c = auth::verify_digest(&method, uri.path(), &headers, &hash, now())?;
    record_nonce(&s, &c, now()).await?;
    let d = device(&s, &c).await?;
    if headers.get(header::CONTENT_LENGTH).is_some_and(|v| {
        v.to_str()
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .is_none_or(|n| n > MAX_TRANSFER)
    }) {
        return Err(ApiError::too_large());
    }
    let staging = Staging(s.blob_dir.join(format!(".upload-{}", Uuid::new_v4())));
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staging.0)
        .await?;
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| ApiError::bad_request("invalid upload stream"))?;
        if let Ok(data) = frame.into_data() {
            size = size
                .checked_add(data.len() as u64)
                .ok_or_else(ApiError::too_large)?;
            if size > MAX_TRANSFER {
                return Err(ApiError::too_large());
            }
            hasher.update(&data);
            file.write_all(&data).await?;
        }
    }
    if hex::encode(hasher.finalize()) != hash {
        return Err(ApiError::bad_request("blob hash mismatch"));
    }
    file.sync_all().await?;
    drop(file);
    // A transaction-scoped advisory lock serializes publication of this content hash.
    let mut tx = s.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(&hash)
        .execute(&mut *tx)
        .await?;
    let old: Option<i64> = sqlx::query_scalar("SELECT size FROM blobs WHERE sha256=$1")
        .bind(&hash)
        .fetch_optional(&mut *tx)
        .await?;
    if old.is_some_and(|n| n != size as i64) {
        return Err(ApiError::internal());
    }
    if old.is_none() {
        tokio::fs::rename(&staging.0, s.blob_dir.join(&hash)).await?;
        sqlx::query("INSERT INTO blobs(sha256,size,uploader,created_at) VALUES($1,$2,$3,$4)")
            .bind(&hash)
            .bind(size as i64)
            .bind(d.id)
            .bind(now())
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query(
        "INSERT INTO blob_uploaders(sha256,device_id) VALUES($1,$2) ON CONFLICT DO NOTHING",
    )
    .bind(&hash)
    .bind(d.id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(StatusCode::OK.into_response())
}

fn range(text: Option<&str>, size: u64) -> Result<Option<(u64, u64)>, ApiError> {
    let Some(text) = text else { return Ok(None) };
    let text = text.strip_prefix("bytes=").ok_or_else(ApiError::range)?;
    let (start, end) = text.split_once('-').ok_or_else(ApiError::range)?;
    if size == 0 || text.contains(',') {
        return Err(ApiError::range());
    }
    let pair = if start.is_empty() {
        let n = end.parse::<u64>().map_err(|_| ApiError::range())?;
        if n == 0 {
            return Err(ApiError::range());
        }
        (size.saturating_sub(n), size - 1)
    } else {
        let start = start.parse::<u64>().map_err(|_| ApiError::range())?;
        let end = if end.is_empty() {
            size - 1
        } else {
            end.parse::<u64>()
                .map_err(|_| ApiError::range())?
                .min(size - 1)
        };
        (start, end)
    };
    if pair.0 >= size || pair.1 < pair.0 {
        return Err(ApiError::range());
    }
    Ok(Some(pair))
}

pub async fn download(
    State(s): State<AppState>,
    Path(hash): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    if !valid_hash(&hash) {
        return Err(ApiError::bad_request("invalid sha256"));
    }
    let (c, b) = read_signed(&s, &method, &uri, &headers, body).await?;
    if !b.is_empty() {
        return Err(ApiError::bad_request("GET body must be empty"));
    }
    let d = device(&s, &c).await?;
    let allowed:bool=sqlx::query_scalar("SELECT EXISTS(
        SELECT 1 FROM job_blobs jb JOIN jobs j ON j.id=jb.job_id WHERE jb.sha256=$1 AND EXISTS(SELECT 1 FROM blob_uploaders u WHERE u.sha256=jb.sha256 AND u.device_id=j.requester) AND (j.requester=$2 OR (j.deadline>$3*1000 AND EXISTS(
        SELECT 1 FROM tasks t JOIN leases l ON l.task_id=t.id WHERE t.job_id=j.id AND l.device_id=$2 AND l.state='active' AND l.expires_at>$3)))
        UNION ALL SELECT 1 FROM task_results r JOIN tasks t ON t.id=r.task_id JOIN jobs j ON j.id=t.job_id WHERE r.sha256=$1 AND j.requester=$2 AND t.state='committed')")
        .bind(&hash).bind(d.id).bind(now()).fetch_one(&s.pool).await?;
    if !allowed {
        return Err(ApiError::forbidden());
    }
    let size: Option<i64> = sqlx::query_scalar("SELECT size FROM blobs WHERE sha256=$1")
        .bind(&hash)
        .fetch_optional(&s.pool)
        .await?;
    let size = size.ok_or_else(|| ApiError::not_found("no such blob"))? as u64;
    let range_text = headers
        .get(header::RANGE)
        .map(|h| h.to_str().map_err(|_| ApiError::range()))
        .transpose()?;
    let requested = range(range_text, size)?;
    let (start, len) = requested.map(|(a, b)| (a, b - a + 1)).unwrap_or((0, size));
    let mut file = tokio::fs::File::open(s.blob_dir.join(&hash)).await?;
    file.seek(std::io::SeekFrom::Start(start)).await?;
    let mut response = Response::new(Body::from_stream(ReaderStream::new(file.take(len))));
    *response.status_mut() = if requested.is_some() {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    let h = response.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/octet-stream"),
    );
    h.insert(
        header::ACCEPT_RANGES,
        header::HeaderValue::from_static("bytes"),
    );
    h.insert(
        header::CONTENT_LENGTH,
        len.to_string().parse().map_err(|_| ApiError::internal())?,
    );
    h.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("private, no-store"),
    );
    if let Some((a, b)) = requested {
        h.insert(
            header::CONTENT_RANGE,
            format!("bytes {a}-{b}/{size}")
                .parse()
                .map_err(|_| ApiError::internal())?,
        );
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn range_edges() {
        assert_eq!(range(Some("bytes=3-"), 10).ok(), Some(Some((3, 9))));
        assert_eq!(range(Some("bytes=-3"), 10).ok(), Some(Some((7, 9))));
        assert_eq!(range(Some("bytes=0-999"), 10).ok(), Some(Some((0, 9))));
        for bad in [
            "bytes=10-",
            "bytes=4-3",
            "bytes=-0",
            "bytes=1-2,4-5",
            "bytes=x-",
            "things=0-1",
        ] {
            assert!(range(Some(bad), 10).is_err())
        }
        assert!(range(Some("bytes=0-"), 0).is_err());
    }
}
