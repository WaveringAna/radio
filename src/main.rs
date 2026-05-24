mod auth;
mod chat;
mod db;
mod loudness;
mod metadata;
mod radio;
mod routes;
mod subsonic;

use std::{net::SocketAddr, path::PathBuf};

use anyhow::Context;
use auth::{AuthConfig, AuthService, parse_admin_dids};
use chat::ChatService;
use db::Database;
use jacquard::types::did::Did;
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
    service_did: Did<'static>,
    service_endpoint: String,
    service_ids: Vec<String>,
    audio_dir: PathBuf,
}

impl AppConfig {
    /// Loads runtime configuration from the environment.
    ///
    /// # Errors
    /// Returns an error when an env var cannot be parsed.
    fn from_env() -> anyhow::Result<Self> {
        let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".into());
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
        let service_did = service_did_from_env(&app_url)?;
        let service_endpoint = std::env::var("SERVICE_ENDPOINT")
            .unwrap_or_else(|_| default_service_endpoint(&app_url));
        let service_ids = service_ids_from_env();
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
            service_did,
            service_endpoint: service_endpoint.trim_end_matches('/').to_owned(),
            service_ids,
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
    // .env.local takes precedence over .env, so a developer can drop
    // dev-only overrides (APP_URL/CORS_ORIGIN pointing at localhost) into
    // .env.local without disturbing the prod values in .env. dotenvy only
    // sets vars that aren't already populated, so loading local first works.
    dotenvy::from_filename(".env.local").ok();
    dotenvy::dotenv().ok();
    init_tracing();

    let config = AppConfig::from_env()?;
    let db = Database::connect(&config.database_url).await?;
    db.prepare().await?;
    let auth = AuthService::new(config.auth_config(), db.clone())?;
    let chat = ChatService::new(db.clone());
    let radio = RadioService::new(db, config.audio_dir.clone(), chat.clone());

    match radio.cleanup_unsupported_audio_on_boot().await {
        Ok(0) => {}
        Ok(removed) => tracing::warn!(removed, "removed unsupported legacy audio rows on boot"),
        Err(error) => {
            tracing::warn!(%error, "failed to clean unsupported legacy audio rows on boot")
        }
    }

    // Genre backfill hits an online metadata service per missing song, so run
    // it in the background to keep the HTTP listener responsive at boot.
    let genre_radio = radio.clone();
    tokio::spawn(async move {
        match genre_radio.backfill_missing_genres_on_boot().await {
            Ok(0) => {}
            Ok(updated) => tracing::info!(updated, "backfilled missing song genres on boot"),
            Err(error) => tracing::warn!(%error, "failed to backfill missing song genres on boot"),
        }
    });

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
    let app = routes::app(
        routes::AppState::new(
            auth,
            radio,
            chat,
            config.service_did.clone(),
            config.service_endpoint.clone(),
            config.service_ids.clone(),
        ),
        &config.cors_origin,
    )
    .layer(TraceLayer::new_for_http());

    let listener = tokio::net::TcpListener::bind(config.bind_addr)
        .await
        .with_context(|| format!("binding backend to {}", config.bind_addr))?;

    tracing::info!(
        bind_addr = %config.bind_addr,
        app_url = %config.app_url,
        cors_origin = %config.cors_origin,
        database_url = %config.database_url,
        service_did = %config.service_did.as_str(),
        service_endpoint = %config.service_endpoint,
        service_ids = ?config.service_ids,
        "radio backend listening"
    );

    axum::serve(listener, app)
        .await
        .context("running axum server")?;

    Ok(())
}

fn service_did_from_env(app_url: &str) -> anyhow::Result<Did<'static>> {
    let value = std::env::var("SERVICE_DID").unwrap_or_else(|_| default_service_did(app_url));
    Did::new_owned(&value).map_err(|error| anyhow::anyhow!("invalid SERVICE_DID {value}: {error}"))
}

fn default_service_did(app_url: &str) -> String {
    app_url
        .strip_prefix("https://")
        .and_then(|rest| rest.split('/').next())
        .filter(|host| !host.is_empty())
        .map(|host| format!("did:web:{host}"))
        .unwrap_or_else(|| "did:web:localhost".into())
}

fn default_service_endpoint(app_url: &str) -> String {
    app_url.replacen("http://", "https://", 1)
}

fn service_ids_from_env() -> Vec<String> {
    let ids: Vec<String> = std::env::var("SERVICE_IDS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|id| id.starts_with('#'))
        .map(ToOwned::to_owned)
        .collect();

    if ids.is_empty() {
        vec!["#radio_xrpc".into()]
    } else {
        ids
    }
}

fn init_tracing() {
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();

    let _ = fmt().with_env_filter(filter).try_init();
}
