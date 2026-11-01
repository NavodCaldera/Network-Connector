//! `nc-coordinator` binary. Configuration by environment:
//!
//! - `NC_DATABASE_URL` (required): PostgreSQL URL. Never logged.
//! - `NC_ENROLL_SECRET` (required, >= 16 chars): owner enrolment secret for device registration.
//! - `NC_BIND`: listen address, default `127.0.0.1:8080` (loopback only).
//! - `NC_TLS_CERT` + `NC_TLS_KEY`: PEM files; when both are set the server speaks HTTPS only.
//! - `NC_RELAY_TOKEN_KEY`: 64 hex chars shared with `nc-relay`; enables `/v1/relay/token`.

use std::net::SocketAddr;

use anyhow::{bail, Context, Result};
use nc_coordinator::{app, AppState};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,sqlx=warn".into()),
        )
        .init();
    let db = std::env::var("NC_DATABASE_URL").context("NC_DATABASE_URL is required")?;
    let enroll_secret = std::env::var("NC_ENROLL_SECRET").unwrap_or_default();
    if enroll_secret.len() < 16 {
        bail!("NC_ENROLL_SECRET (at least 16 characters) is required");
    }
    let relay_key = match std::env::var("NC_RELAY_TOKEN_KEY") {
        Ok(s) => Some(
            nc_relay::TokenKey::from_hex(&s).context("NC_RELAY_TOKEN_KEY must be 64 hex chars")?,
        ),
        Err(_) => None,
    };
    let bind: SocketAddr = std::env::var("NC_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8080".into())
        .parse()
        .context("NC_BIND")?;

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect(&db)
        .await
        .context("connecting to PostgreSQL")?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("running migrations")?;

    let blob_dir = std::env::var_os("NC_BLOB_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "blobs".into());
    tokio::fs::create_dir_all(&blob_dir)
        .await
        .context("creating blob directory")?;
    let router = app(AppState {
        blob_dir,
        pool,
        enroll_secret,
        relay_key,
    });
    match (std::env::var("NC_TLS_CERT"), std::env::var("NC_TLS_KEY")) {
        (Ok(cert), Ok(key)) => {
            let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key)
                .await
                .context("loading TLS certificate")?;
            tracing::info!(%bind, "coordinator listening (HTTPS)");
            axum_server::bind_rustls(bind, tls)
                .serve(router.into_make_service())
                .await?;
        }
        _ => {
            tracing::warn!(%bind, "coordinator listening on plain HTTP (no NC_TLS_CERT/NC_TLS_KEY); local use only");
            let listener = tokio::net::TcpListener::bind(bind).await?;
            axum::serve(listener, router).await?;
        }
    }
    Ok(())
}
