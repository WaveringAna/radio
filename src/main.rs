mod auth;
mod db;
mod loudness;
mod metadata;
mod radio;
mod routes;
mod subsonic;

use std::{net::SocketAddr, path::PathBuf};

use anyhow::Context;
use auth::{AuthConfig, AuthService, parse_admin_dids};
use db::Database;
use radio::RadioService;
use tower_http::trace::TraceLayer;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::{EnvFilter, fmt};

/// Runtime configuration for the radio backend.
#[derive(Clone, Debug)]
struct AppConfig {
    bind_addr: SocketAddr,
    database_url: String,
    app_url: String,
    cors_origin: String,
    session_cookie_name: String,
    session_ttl_days: i64,
    admin_dids: Vec<String>,
    audio_dir: PathBuf,
}

impl AppConfig {
    /// Loads runtime configuration from the environment.
    ///
    /// # Errors
    /// Returns an error when an env var cannot be parsed.
    fn from_env() -> anyhow::Result<Self> {
        let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:3000".into());
        let app_url = std::env::var("APP_URL").unwrap_or_else(|_| "http://127.0.0.1:3000".into());
        let cors_origin =
            std::env::var("CORS_ORIGIN").unwrap_or_else(|_| "http://127.0.0.1:5173".into());
        let database_url =
            std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite://radio.db".into());
        let session_cookie_name =
            std::env::var("SESSION_COOKIE_NAME").unwrap_or_else(|_| "radio_session".into());
        let session_ttl_days = std::env::var("SESSION_TTL_DAYS")
            .ok()
            .map(|value| value.parse::<i64>())
            .transpose()
            .context("parsing SESSION_TTL_DAYS")?
            .unwrap_or(30);
        let admin_dids = std::env::var("ADMIN_DIDS")
            .map(|value| parse_admin_dids(&value))
            .unwrap_or_default();
        let audio_dir = std::env::var("AUDIO_DIR").unwrap_or_else(|_| "data/audio".into());

        Ok(Self {
            bind_addr: bind_addr
                .parse()
                .with_context(|| format!("parsing BIND_ADDR {bind_addr}"))?,
            database_url,
            app_url: app_url.trim_end_matches('/').to_owned(),
            cors_origin: cors_origin.trim_end_matches('/').to_owned(),
            session_cookie_name,
            session_ttl_days,
            admin_dids,
            audio_dir: PathBuf::from(audio_dir),
        })
    }

    /// Builds the auth-specific config subset.
    fn auth_config(&self) -> AuthConfig {
        AuthConfig {
            app_url: self.app_url.clone(),
            frontend_url: self.cors_origin.clone(),
            session_cookie_name: self.session_cookie_name.clone(),
            session_ttl_days: self.session_ttl_days,
            admin_dids: self.admin_dids.clone(),
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    init_tracing();

    let config = AppConfig::from_env()?;
    let db = Database::connect(&config.database_url).await?;
    db.prepare().await?;
    let auth = AuthService::new(config.auth_config(), db.clone())?;
    let radio = RadioService::new(db, config.audio_dir.clone());
    match radio.backfill_missing_genres_on_boot().await {
        Ok(0) => {}
        Ok(updated) => tracing::info!(updated, "backfilled missing song genres on boot"),
        Err(error) => tracing::warn!(%error, "failed to backfill missing song genres on boot"),
    }

    // Loudness backfill can take seconds-per-track via ffmpeg; run it in the
    // background so the HTTP listener comes up immediately.
    let loudness_radio = radio.clone();
    tokio::spawn(async move {
        tracing::info!("starting loudness backfill in background");
        match loudness_radio.backfill_missing_loudness_on_boot().await {
            Ok(0) => tracing::info!("loudness backfill: nothing to do"),
            Ok(updated) => tracing::info!(updated, "loudness backfill complete"),
            Err(error) => {
                tracing::warn!(%error, "loudness backfill failed")
            }
        }
    });
    let app = routes::app(routes::AppState::new(auth, radio), &config.cors_origin)
        .layer(TraceLayer::new_for_http());

    let listener = tokio::net::TcpListener::bind(config.bind_addr)
        .await
        .with_context(|| format!("binding backend to {}", config.bind_addr))?;

    tracing::info!(
        bind_addr = %config.bind_addr,
        app_url = %config.app_url,
        cors_origin = %config.cors_origin,
        database_url = %config.database_url,
        "radio backend listening"
    );

    axum::serve(listener, app)
        .await
        .context("running axum server")?;

    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();

    let _ = fmt().with_env_filter(filter).try_init();
}
