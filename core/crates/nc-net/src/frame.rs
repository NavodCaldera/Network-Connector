//! Length-prefixed frames (u32 big-endian length + body) with a hard size cap.

use futures::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::NetError;

/// Writes one frame.
pub(crate) async fn write_frame<S: AsyncWrite + Unpin>(
    s: &mut S,
    body: &[u8],
) -> Result<(), NetError> {
    let len =
        u32::try_from(body.len()).map_err(|_| NetError::Protocol("frame too large".into()))?;
    s.write_all(&len.to_be_bytes()).await?;
    s.write_all(body).await?;
    s.flush().await?;
    Ok(())
}

/// Reads one frame of at most `max` bytes. Larger frames are rejected before allocating.
pub(crate) async fn read_frame<S: AsyncRead + Unpin>(
    s: &mut S,
    max: usize,
) -> Result<Vec<u8>, NetError> {
    let mut len = [0u8; 4];
    s.read_exact(&mut len).await?;
    let len = u32::from_be_bytes(len) as usize;
    if len > max {
        return Err(NetError::Protocol(format!(
            "frame of {len} bytes exceeds cap {max}"
        )));
    }
    let mut body = vec![0u8; len];
    s.read_exact(&mut body).await?;
    Ok(body)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use futures::io::Cursor;

    #[tokio::test]
    async fn roundtrip_and_cap() {
        let mut buf = Cursor::new(Vec::new());
        write_frame(&mut buf, b"hello").await.unwrap();
        let mut r = Cursor::new(buf.into_inner());
        assert_eq!(read_frame(&mut r, 16).await.unwrap(), b"hello");

        let mut buf = Cursor::new(Vec::new());
        write_frame(&mut buf, &[0u8; 100]).await.unwrap();
        let mut r = Cursor::new(buf.into_inner());
        assert!(matches!(
            read_frame(&mut r, 16).await,
            Err(NetError::Protocol(_))
        ));
    }
}
