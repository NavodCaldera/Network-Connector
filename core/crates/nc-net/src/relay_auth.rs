//! Client side of relay authorisation: present a short-lived token to our relay (`nc-relay`) before
//! asking it for a reservation or circuit.
//!
//! Frame 1 (client -> relay): the opaque token bytes as issued by the coordinator (max 2 KiB).
//! Frame 2 (relay -> client): `b"ok"` or `b"denied"`.
//! The token is bound to our peer key by the issuer, so presenting someone else's token is useless.
//! The token is never logged.
//!
//! TODO(proto): move this frame format to `proto/` (Core owns it); see hand-back "Requests".

use futures::AsyncWriteExt;
use libp2p::{PeerId, StreamProtocol};

use crate::frame::{read_frame, write_frame};
use crate::NetError;

/// Protocol id spoken by `nc-relay` for token presentation.
pub const RELAY_AUTH_PROTOCOL: StreamProtocol = StreamProtocol::new("/nc/relay-auth/1.0.0");

pub(crate) async fn present_token(
    control: &mut libp2p_stream::Control,
    relay: PeerId,
    token: &[u8],
) -> Result<(), NetError> {
    let mut s = control
        .open_stream(relay, RELAY_AUTH_PROTOCOL)
        .await
        .map_err(|e| NetError::Stream(e.to_string()))?;
    write_frame(&mut s, token).await?;
    let reply = read_frame(&mut s, 64).await?;
    let _ = s.close().await;
    if reply == b"ok" {
        Ok(())
    } else {
        Err(NetError::Transport("relay denied the token".into()))
    }
}
