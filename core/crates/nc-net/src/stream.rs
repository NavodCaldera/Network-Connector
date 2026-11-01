//! Authenticated, byte-counted stream handed to Core.

use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use futures::{AsyncRead, AsyncWrite};
use libp2p::{PeerId, Stream};

/// A stream to a **trusted** peer. `peer()` is the Noise/TLS-authenticated peer key, never an address.
///
/// Implements `futures::AsyncRead + AsyncWrite`. Bytes in both directions are added to the node's
/// direct or relayed counter.
pub struct PeerStream {
    peer: PeerId,
    inner: Stream,
    counter: Arc<AtomicU64>,
    relayed: bool,
}

impl std::fmt::Debug for PeerStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PeerStream")
            .field("peer", &self.peer)
            .field("relayed", &self.relayed)
            .finish()
    }
}

impl PeerStream {
    pub(crate) fn new(peer: PeerId, inner: Stream, counter: Arc<AtomicU64>, relayed: bool) -> Self {
        Self {
            peer,
            inner,
            counter,
            relayed,
        }
    }

    /// Authenticated key of the remote peer.
    pub fn peer(&self) -> PeerId {
        self.peer
    }

    /// True if the stream was opened while only a relay path existed.
    pub fn is_relayed(&self) -> bool {
        self.relayed
    }
}

impl AsyncRead for PeerStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        let r = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(n)) = &r {
            self.counter.fetch_add(*n as u64, Ordering::Relaxed);
        }
        r
    }
}

impl AsyncWrite for PeerStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let r = Pin::new(&mut self.inner).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = &r {
            self.counter.fetch_add(*n as u64, Ordering::Relaxed);
        }
        r
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_close(cx)
    }
}
