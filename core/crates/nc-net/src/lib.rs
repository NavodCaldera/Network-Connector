//! `nc-net`: libp2p transport, local discovery, pairing and relay client.
//!
//! What Core gets (AGENT.md §3): an authenticated stream API keyed by the peer's device key
//! ([`PeerId`]), never by name or address. See [`Node::open_stream`] and [`Node::accept`].
//!
//! Security rules this crate enforces (AGENT.md §1):
//! - **Discovery is not authorisation (rule 2).** mDNS only tells us an address. A connection from or to a
//!   peer that is not in the [`TrustStore`] is closed, unless it is a configured infrastructure peer (relay)
//!   or a pairing attempt that is in progress. Streams to/from untrusted peers are refused.
//! - **End-to-end encryption (rule 6).** Every connection is Noise (TCP) or TLS 1.3 (QUIC), including
//!   connections carried through a relay circuit; the relay only sees ciphertext.
//! - **No secrets in logs (rule 10).** Pairing secrets and relay tokens never reach `tracing`, and their
//!   `Debug` output is redacted.
//!
//! Connection ladder: same-LAN direct, internet direct, hole-punched (DCUtR), relayed. Each connection
//! records its [`Rung`]; stream bytes are counted as direct or relayed ([`TrafficStats`]). The TLS/443
//! fallback rung is not implemented in Phase 0.

mod frame;
mod ladder;
mod node;
pub mod pairing;
mod relay_auth;
mod stream;
mod trust;

pub use ladder::{classify_addr, ConnectionInfo, Rung, TrafficStats};
pub use libp2p::{identity::Keypair, multiaddr::Protocol, Multiaddr, PeerId, StreamProtocol};
pub use node::{IncomingPeerStreams, Node, NodeConfig, NodeEvent};
pub use pairing::{PairingOffer, PairingPrompt};
pub use relay_auth::RELAY_AUTH_PROTOCOL;
pub use stream::PeerStream;
pub use trust::TrustStore;

/// Name of this crate, used by diagnostics.
pub const CRATE_NAME: &str = "nc-net";

/// Errors returned by `nc-net`.
#[derive(Debug, thiserror::Error)]
pub enum NetError {
    /// The peer is not paired (not in the trust store). Discovery alone never makes a peer trusted.
    #[error("peer {0} is not trusted")]
    Untrusted(PeerId),
    /// The protocol name is reserved for `nc-net` internals.
    #[error("protocol is reserved by nc-net")]
    ReservedProtocol,
    /// Pairing failed. The message never contains the secret.
    #[error("pairing failed: {0}")]
    Pairing(String),
    /// The pairing offer could not be parsed.
    #[error("invalid pairing offer")]
    InvalidOffer,
    /// A frame was larger than allowed or malformed.
    #[error("protocol error: {0}")]
    Protocol(String),
    /// Opening a stream failed.
    #[error("stream: {0}")]
    Stream(String),
    /// libp2p transport or swarm error.
    #[error("transport: {0}")]
    Transport(String),
    /// The node task has stopped.
    #[error("node stopped")]
    Stopped,
    /// Timed out.
    #[error("timed out")]
    Timeout,
    /// I/O error.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}
