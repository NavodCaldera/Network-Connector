//! Client side of relay authorisation: present a short-lived token to our relay (`nc-relay`) before
//! asking it for a reservation or circuit.
//!
//! Both frames are prost RelayAuthFrame messages with the existing u32 BE length prefix.
//! Token presentation carries a binary RelayToken; replies carry an explicit boolean.
//! The token is bound to the authenticated peer key and is never logged.

use futures::AsyncWriteExt;
use libp2p::{PeerId, StreamProtocol};
use nc_protocol::pb::v1::{relay_auth_frame::Payload, RelayAuthFrame};
use nc_protocol::wire::{decode_strict, relay_token_from_text};
use prost::Message;

use crate::frame::{read_frame, write_frame};
use crate::NetError;

/// Protocol id spoken by `nc-relay` for token presentation.
pub const RELAY_AUTH_PROTOCOL: StreamProtocol = StreamProtocol::new("/nc/relay-auth/1.0.0");

pub(crate) async fn present_token(
    control: &mut libp2p_stream::Control,
    relay: PeerId,
    token: &[u8],
) -> Result<(), NetError> {
    let token = std::str::from_utf8(token)
        .ok()
        .and_then(|text| relay_token_from_text(text).ok())
        .ok_or_else(|| NetError::Transport("relay denied the token".into()))?;
    let frame = RelayAuthFrame {
        payload: Some(Payload::Token(token)),
    }
    .encode_to_vec();
    let mut s = control
        .open_stream(relay, RELAY_AUTH_PROTOCOL)
        .await
        .map_err(|e| NetError::Stream(e.to_string()))?;
    write_frame(&mut s, &frame).await?;
    let reply = read_frame(&mut s, 64).await?;
    let _ = s.close().await;
    let reply: RelayAuthFrame =
        decode_strict(&reply).map_err(|_| NetError::Protocol("bad relay auth frame".into()))?;
    if matches!(reply.payload, Some(Payload::Accepted(true))) {
        Ok(())
    } else {
        Err(NetError::Transport("relay denied the token".into()))
    }
}
