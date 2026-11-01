//! `nc-relay`: libp2p circuit relay v2 that serves only peers holding a short-lived token from the
//! coordinator, with per-device (peer) and per-account quotas (AGENT.md §1 rule 6: no unauthenticated
//! public relay).
//!
//! Flow: a device gets a token from `POST /v1/relay/token` on the coordinator, connects to the relay
//! (Noise/TLS authenticates its peer id), sends the token on `/nc/relay-auth/1.0.0`, and only then may it
//! reserve a slot or open a circuit. Circuits carry the peers' own end-to-end encrypted connection; the
//! relay never sees plaintext.

pub mod quota;
pub mod server;
pub mod token;

pub use quota::{Limits, Quotas};
pub use server::{build_swarm, run, RelayConfig};
pub use token::{Claims, TokenError, TokenKey};

/// Current Unix time in seconds.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
