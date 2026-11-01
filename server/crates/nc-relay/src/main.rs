//! `nc-relay` binary. Configuration by environment:
//!
//! - `NC_RELAY_TOKEN_KEY` (required): 64 hex chars, shared with the coordinator. The relay refuses to
//!   start without it, so it can never run as an open relay.
//! - `NC_RELAY_LISTEN`: comma-separated multiaddrs. Default loopback only
//!   (`/ip4/127.0.0.1/udp/4001/quic-v1,/ip4/127.0.0.1/tcp/4001`).
//! - `NC_RELAY_EXTERNAL`: comma-separated public multiaddrs clients dial. Required in practice: libp2p
//!   relay v2 refuses reservations until the server has an external address.
//! - `NC_RELAY_KEY_FILE`: identity key file (created if missing). Default `relay.key`.
//! - `NC_RELAY_CIRCUIT_BYTES`, `NC_RELAY_BYTES_PER_PEER`, `NC_RELAY_BYTES_PER_ACCOUNT`,
//!   `NC_RELAY_RESERVATIONS_PER_PEER`, `NC_RELAY_RESERVATIONS_PER_ACCOUNT`: quota overrides.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use libp2p::{identity, Multiaddr};
use nc_relay::{Limits, Quotas, RelayConfig, TokenKey};

fn env_num(name: &str, default: u64) -> Result<u64> {
    match std::env::var(name) {
        Ok(v) => v
            .parse()
            .with_context(|| format!("{name} must be a number")),
        Err(_) => Ok(default),
    }
}

fn load_key(path: &str) -> Result<identity::Keypair> {
    if let Ok(b) = std::fs::read(path) {
        return identity::Keypair::from_protobuf_encoding(&b).context("bad relay key file");
    }
    let k = identity::Keypair::generate_ed25519();
    std::fs::write(path, k.to_protobuf_encoding()?)?;
    Ok(k)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,libp2p=warn".into()),
        )
        .init();
    let Some(token_key) = std::env::var("NC_RELAY_TOKEN_KEY")
        .ok()
        .and_then(|s| TokenKey::from_hex(&s))
    else {
        bail!("NC_RELAY_TOKEN_KEY (64 hex chars) is required; refusing to run an unauthenticated relay");
    };
    let listen = std::env::var("NC_RELAY_LISTEN")
        .unwrap_or_else(|_| "/ip4/127.0.0.1/udp/4001/quic-v1,/ip4/127.0.0.1/tcp/4001".into());
    let listen = listen
        .split(',')
        .map(|s| s.trim().parse::<Multiaddr>())
        .collect::<Result<Vec<_>, _>>()
        .context("NC_RELAY_LISTEN")?;
    let d = Limits::default();
    let limits = Limits {
        reservations_per_peer: env_num(
            "NC_RELAY_RESERVATIONS_PER_PEER",
            d.reservations_per_peer.into(),
        )? as u32,
        reservations_per_account: env_num(
            "NC_RELAY_RESERVATIONS_PER_ACCOUNT",
            d.reservations_per_account.into(),
        )? as u32,
        bytes_per_peer: env_num("NC_RELAY_BYTES_PER_PEER", d.bytes_per_peer)?,
        bytes_per_account: env_num("NC_RELAY_BYTES_PER_ACCOUNT", d.bytes_per_account)?,
        circuit_bytes: env_num("NC_RELAY_CIRCUIT_BYTES", d.circuit_bytes)?,
        window: d.window,
    };
    let key = load_key(&std::env::var("NC_RELAY_KEY_FILE").unwrap_or_else(|_| "relay.key".into()))?;
    tracing::info!(peer = %key.public().to_peer_id(), "nc-relay starting");
    let external = match std::env::var("NC_RELAY_EXTERNAL") {
        Ok(s) if !s.trim().is_empty() => s
            .split(',')
            .map(|s| s.trim().parse::<Multiaddr>())
            .collect::<Result<Vec<_>, _>>()
            .context("NC_RELAY_EXTERNAL")?,
        _ => Vec::new(),
    };
    let cfg = RelayConfig {
        listen,
        external,
        token_key,
        max_reservations: 256,
        max_circuits: 256,
        max_circuits_per_peer: 8,
        max_circuit_duration: Duration::from_secs(30 * 60),
    };
    let quotas = Quotas::new(limits);
    let swarm = nc_relay::build_swarm(key, &cfg, &quotas).map_err(|e| anyhow::anyhow!(e))?;
    nc_relay::run(swarm, cfg, quotas)
        .await
        .map_err(|e| anyhow::anyhow!(e))
}
