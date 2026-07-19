mod auth;
mod chat;
mod db;
mod import;
mod loudness;
mod metadata;
mod radio;
mod routes;
mod subsonic;

use std::{net::SocketAddr, path::PathBuf, time::Duration};

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
    frontend_url: String,
    admin_dids: Vec<String>,
    service_did: Did<'static>,
    service_endpoint: String,
    service_ids: Vec<String>,
    station_announce_relays: Vec<String>,
    station_announce_on_startup: bool,
    station_url: String,
    station_name: String,
    station_description: Option<String>,
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
        let frontend_url = std::env::var("FRONTEND_URL").unwrap_or_else(|_| app_url.clone());
        let database_url =
            std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite://radio.db".into());
        let admin_dids = std::env::var("ADMIN_DIDS")
            .map(|value| parse_admin_dids(&value))
            .unwrap_or_default();
        let service_did = service_did_from_env(&app_url)?;
        let service_endpoint = std::env::var("SERVICE_ENDPOINT")
            .unwrap_or_else(|_| default_service_endpoint(&app_url));
        let service_ids = service_ids_from_env();
        let station_announce_relays = station_announce_relays_from_env();
        let station_announce_on_startup = station_announce_on_startup_from_env();
        let station_url = std::env::var("STATION_URL")
            .unwrap_or_else(|_| service_endpoint.trim_end_matches('/').to_owned());
        let station_name = std::env::var("STATION_NAME").unwrap_or_else(|_| "radio".into());
        let station_description = std::env::var("STATION_DESCRIPTION")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        let audio_dir = std::env::var("AUDIO_DIR").unwrap_or_else(|_| "data/audio".into());

        Ok(Self {
            bind_addr: bind_addr
                .parse()
                .with_context(|| format!("parsing BIND_ADDR {bind_addr}"))?,
            database_url,
            app_url: app_url.trim_end_matches('/').to_owned(),
            frontend_url: frontend_url.trim_end_matches('/').to_owned(),
            admin_dids,
            service_did,
            service_endpoint: service_endpoint.trim_end_matches('/').to_owned(),
            service_ids,
            station_announce_relays,
            station_announce_on_startup,
            station_url: station_url.trim_end_matches('/').to_owned(),
            station_name,
            station_description,
            audio_dir: PathBuf::from(audio_dir),
        })
    }

    /// Builds the auth-specific config subset.
    fn auth_config(&self) -> AuthConfig {
        AuthConfig {
            app_url: self.app_url.clone(),
            frontend_url: self.frontend_url.clone(),
            admin_dids: self.admin_dids.clone(),
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // .env.local takes precedence over .env, so a developer can drop
    // dev-only overrides (APP_URL/FRONTEND_URL pointing at localhost) into
    // .env.local without disturbing the prod values in .env. dotenvy only
    // sets vars that aren't already populated, so loading local first works.
    dotenvy::from_filename(".env.local").ok();
    dotenvy::dotenv().ok();
    init_tracing();

    let config = AppConfig::from_env()?;

    // `import <dir> [--did <did>]` runs the one-shot local library importer and
    // exits instead of starting the HTTP server.
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("import") {
        return run_import_command(&config, args.collect()).await;
    }

    let db = Database::connect(&config.database_url).await?;
    db.prepare().await?;
    let pds_signing_key = routes::pds::load_or_create_signing_key(db.pool()).await?;
    let auth = AuthService::new(config.auth_config(), db.clone())?;
    let chat = ChatService::new(db.clone());
    let pds_pool = db.pool().clone();
    let station_now = rfc3339_now();
    let station_has_public_announce_host = routes::pds::announce_hostname(&config.service_endpoint)
        .or_else(|| routes::pds::announce_hostname(&config.station_url))
        .is_some();
    let station_updated_at =
        if config.station_announce_on_startup && station_has_public_announce_host {
            routes::pds::store_station_updated_at(
                db.pool(),
                &config.station_url,
                &config.station_name,
                config.station_description.as_deref(),
                &station_now,
            )
            .await?
        } else {
            routes::pds::load_or_update_station_updated_at(
                db.pool(),
                &config.station_url,
                &config.station_name,
                config.station_description.as_deref(),
                &station_now,
            )
            .await?
        };
    let radio = RadioService::new(db, config.audio_dir.clone(), chat.clone());
    let pds = routes::pds::EmbeddedPds::new(
        &config.service_did,
        &config.station_url,
        &config.station_name,
        config.station_description.as_deref(),
        &station_updated_at,
        &pds_signing_key,
    )
    .await
    .context("initializing embedded pds repository")?;

    match radio.cleanup_unsupported_audio_on_boot().await {
        Ok(0) => {}
        Ok(removed) => tracing::warn!(removed, "removed unsupported legacy audio rows on boot"),
        Err(error) => {
            tracing::warn!(%error, "failed to clean unsupported legacy audio rows on boot")
        }
    }

    match radio.auto_sync_albums().await {
        Ok(()) => tracing::info!("auto-synced album loops on boot"),
        Err(error) => tracing::warn!(%error, "failed to auto-sync album loops on boot"),
    }

    // Cover and genre backfills hit online metadata services per missing song,
    // so run them in the background to keep the HTTP listener responsive at
    // boot. Covers run first because yt-dlp imports can leave a large artwork
    // backlog while the station remains otherwise usable.
    let metadata_radio = radio.clone();
    tokio::spawn(async move {
        tracing::info!("starting cover backfill in background");
        match metadata_radio.backfill_missing_covers_on_boot().await {
            Ok(0) => tracing::info!("cover backfill: nothing to do"),
            Ok(updated) => tracing::info!(updated, "cover backfill complete"),
            Err(error) => tracing::warn!(%error, "cover backfill failed"),
        }

        match metadata_radio.backfill_missing_genres_on_boot().await {
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
            config.station_announce_relays.clone(),
            pds,
            pds_pool,
            pds_signing_key.clone(),
            config.station_url.clone(),
            config.station_name.clone(),
            config.station_description.clone(),
        ),
        &config.frontend_url,
    )
    .layer(TraceLayer::new_for_http());

    let listener = tokio::net::TcpListener::bind(config.bind_addr)
        .await
        .with_context(|| format!("binding backend to {}", config.bind_addr))?;

    if config.station_announce_on_startup {
        if let Some(hostname) = routes::pds::announce_hostname(&config.service_endpoint)
            .or_else(|| routes::pds::announce_hostname(&config.station_url))
        {
            let relays = config.station_announce_relays.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(750)).await;
                for report in routes::pds::request_relay_crawls(&relays, &hostname).await {
                    if report.ok {
                        tracing::info!(
                            relay = %report.relay,
                            hostname = %report.hostname,
                            status = ?report.status,
                            "announced station repo to relay"
                        );
                    } else {
                        tracing::warn!(
                            relay = %report.relay,
                            hostname = %report.hostname,
                            status = ?report.status,
                            error = ?report.error,
                            "failed to announce station repo to relay"
                        );
                    }
                }
            });
        } else {
            tracing::info!(
                service_endpoint = %config.service_endpoint,
                station_url = %config.station_url,
                "skipping station relay announce without a public endpoint"
            );
        }
    }

    tracing::info!(
        bind_addr = %config.bind_addr,
        app_url = %config.app_url,
        frontend_url = %config.frontend_url,
        database_url = %config.database_url,
        service_did = %config.service_did.as_str(),
        service_endpoint = %config.service_endpoint,
        service_ids = ?config.service_ids,
        station_announce_relays = ?config.station_announce_relays,
        station_url = %config.station_url,
        station_name = %config.station_name,
        "radio backend listening"
    );

    axum::serve(listener, app)
        .await
        .context("running axum server")?;

    Ok(())
}

/// Parses `import <dir> [--did <did>]` args and runs the local library importer.
async fn run_import_command(config: &AppConfig, args: Vec<String>) -> anyhow::Result<()> {
    let mut dir: Option<String> = None;
    let mut did_override: Option<String> = None;
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--did" => {
                did_override = Some(
                    iter.next()
                        .context("--did requires a value (the admin DID to attribute songs to)")?,
                );
            }
            other if dir.is_none() => dir = Some(other.to_owned()),
            other => return Err(anyhow::anyhow!("unexpected argument: {other}")),
        }
    }

    let dir = dir.context(
        "usage: radio import <dir> [--did <did>]  (path to the music library to import)",
    )?;

    // Attribute imported songs to an admin so they behave like a normal upload.
    let admin_did = did_override
        .or_else(|| config.admin_dids.first().cloned())
        .context(
            "no admin DID available: set ADMIN_DIDS in .env or pass --did <did> to the importer",
        )?;

    import::run(
        &config.database_url,
        config.audio_dir.clone(),
        &admin_did,
        std::path::Path::new(&dir),
    )
    .await
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

fn station_announce_relays_from_env() -> Vec<String> {
    let mut targets = Vec::new();
    extend_announce_targets(
        &mut targets,
        &std::env::var("STATION_ANNOUNCE_RELAYS")
            .unwrap_or_else(|_| "https://relay.fire.hose.cam".into()),
    );
    if let Ok(workers) = std::env::var("STATION_ANNOUNCE_WORKERS") {
        extend_announce_targets(&mut targets, &workers);
    }
    targets
}

fn extend_announce_targets(targets: &mut Vec<String>, raw: &str) {
    for target in raw
        .split(',')
        .map(str::trim)
        .map(|target| target.trim_end_matches('/'))
        .filter(|target| !target.is_empty())
    {
        if !targets.iter().any(|existing| existing == target) {
            targets.push(target.to_owned());
        }
    }
}

fn station_announce_on_startup_from_env() -> bool {
    !matches!(
        std::env::var("STATION_ANNOUNCE_ON_STARTUP")
            .unwrap_or_else(|_| "true".into())
            .trim()
            .to_lowercase()
            .as_str(),
        "0" | "false" | "off" | "no"
    )
}

fn rfc3339_now() -> String {
    let now = time::OffsetDateTime::now_utc();
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    )
}

fn init_tracing() {
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();

    let _ = fmt().with_env_filter(filter).try_init();
}
