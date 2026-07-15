use std::{collections::BTreeMap, env, net::SocketAddr, path::Path, sync::Arc, time::Duration};

use anyhow::{Context, anyhow};
use axum::{
    Json, Router,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    routing::{get, post},
};
use chrono::{SecondsFormat, Utc};
use hydrant::{
    FilterMode,
    config::Config,
    control::{EventStream, Hydrant},
    deps::{futures::StreamExt, jacquard::types::did::Did},
};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use tokio::{
    net::TcpListener,
    sync::{Mutex, RwLock},
    time::{MissedTickBehavior, interval},
};
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

const COLLECTION: &str = "pet.nkp.radio.station";

type StationIndex = Arc<RwLock<BTreeMap<String, StationEntry>>>;

#[derive(Clone)]
struct AppState {
    stations: StationIndex,
    hydrant: Hydrant,
    request_crawl_limiter: Arc<Mutex<RequestCrawlLimiter>>,
}

#[derive(Clone, Debug)]
struct HealthConfig {
    interval: Duration,
    timeout: Duration,
    failure_threshold: u32,
}

#[derive(Debug)]
struct RequestCrawlLimiter {
    day: String,
    used_new_hosts: u32,
    daily_limit: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StationRecord {
    #[serde(rename = "$type")]
    record_type: Option<String>,
    url: String,
    name: String,
    description: Option<String>,
    updated_at: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StationView {
    did: String,
    uri: String,
    cid: Option<String>,
    rev: String,
    url: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    updated_at: String,
    indexed_at: String,
}

#[derive(Clone, Debug)]
struct StationEntry {
    view: StationView,
    health: StationHealth,
}

#[derive(Clone, Debug)]
struct StationHealth {
    healthy: bool,
    consecutive_failures: u32,
    last_checked_at: Option<String>,
    last_healthy_at: Option<String>,
    last_error: Option<String>,
}

impl StationEntry {
    fn new(view: StationView) -> Self {
        Self {
            view,
            health: StationHealth::new(),
        }
    }

    fn is_visible(&self) -> bool {
        self.health.healthy
    }
}

impl StationHealth {
    fn new() -> Self {
        Self {
            healthy: false,
            consecutive_failures: 0,
            last_checked_at: None,
            last_healthy_at: None,
            last_error: None,
        }
    }
}

impl HealthConfig {
    fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            interval: Duration::from_secs(env_u64("SYNDICATION_HEALTH_INTERVAL_SECS", 30)?),
            timeout: Duration::from_secs(env_u64("SYNDICATION_HEALTH_TIMEOUT_SECS", 5)?),
            failure_threshold: env_u32("SYNDICATION_HEALTH_FAILURE_THRESHOLD", 2)?,
        })
    }
}

impl RequestCrawlLimiter {
    fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            day: current_utc_day(),
            used_new_hosts: 0,
            daily_limit: env_u32("SYNDICATION_REQUEST_CRAWL_DAILY_LIMIT", 256)?,
        })
    }

    fn try_increment_new_host(&mut self) -> bool {
        let day = current_utc_day();
        if self.day != day {
            self.day = day;
            self.used_new_hosts = 0;
        }

        if self.used_new_hosts >= self.daily_limit {
            return false;
        }

        self.used_new_hosts += 1;
        true
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RootResponse {
    collection: &'static str,
    endpoints: [&'static str; 4],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HealthResponse {
    ok: bool,
    collection: &'static str,
    station_count: usize,
    healthy_station_count: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequestCrawlRequest {
    hostname: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct XrpcErrorResponse {
    error: &'static str,
    message: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    hydrant::deps::rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .ok();

    load_env_files();
    init_tracing();

    let bind_addr = env::var("SYNDICATION_BIND_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:3300".to_owned())
        .parse::<SocketAddr>()
        .context("parsing SYNDICATION_BIND_ADDR")?;
    let health_config = HealthConfig::from_env()?;
    let request_crawl_limiter = Arc::new(Mutex::new(RequestCrawlLimiter::from_env()?));

    let mut cfg = Config::from_env().map_err(report_to_anyhow)?;
    apply_live_hydrant_defaults(&mut cfg);

    let hydrant = Hydrant::new(cfg).await.map_err(report_to_anyhow)?;
    hydrant
        .filter
        .set_mode(FilterMode::Filter)
        .set_signals([COLLECTION])
        .set_collections([COLLECTION])
        .apply()
        .await
        .map_err(report_to_anyhow)?;

    let seed_dids = parse_seed_dids()?;
    if seed_dids.is_empty() {
        info!("SYNDICATION_SEED_DIDS is empty; relying on Hydrant crawler and firehose discovery");
    } else {
        let queued = hydrant
            .repos
            .track(seed_dids)
            .await
            .map_err(report_to_anyhow)?;
        info!(queued = queued.len(), "queued seed repos for backfill");
    }

    let stations = Arc::new(RwLock::new(BTreeMap::new()));
    let app_state = AppState {
        stations: stations.clone(),
        hydrant: hydrant.clone(),
        request_crawl_limiter,
    };
    let stream = hydrant.subscribe(Some(0));
    let hydrant_runner = hydrant.run().map_err(report_to_anyhow)?;

    tokio::select! {
        result = hydrant_runner => result.map_err(report_to_anyhow).context("hydrant stopped")?,
        result = serve_http(app_state, bind_addr) => result.context("http server stopped")?,
        result = health_check_loop(stations.clone(), health_config) => result.context("station health checker stopped")?,
        result = collect_station_records(stations, stream) => result.context("station stream stopped")?,
    }

    Ok(())
}

fn load_env_files() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    dotenvy::from_path(manifest_dir.join(".env.local")).ok();
    dotenvy::from_path(manifest_dir.join(".env")).ok();
    dotenvy::from_filename(".env.local").ok();
    dotenvy::dotenv().ok();
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("sister_radio_syndication_worker=info,hydrant=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

fn apply_live_hydrant_defaults(cfg: &mut Config) {
    if env::var_os("HYDRANT_FULL_NETWORK").is_none() {
        cfg.full_network = false;
    }
    if env::var_os("HYDRANT_ENABLE_FIREHOSE").is_none() {
        cfg.enable_firehose = true;
    }
    if env::var_os("HYDRANT_ENABLE_CRAWLER").is_none() {
        cfg.enable_crawler = Some(true);
    }
    if env::var_os("HYDRANT_FILTER_SIGNALS").is_none() {
        cfg.filter_signals = Some(vec![COLLECTION.to_owned()]);
    }
    if env::var_os("HYDRANT_FILTER_COLLECTIONS").is_none() {
        cfg.filter_collections = Some(vec![COLLECTION.to_owned()]);
    }
}

fn parse_seed_dids() -> anyhow::Result<Vec<Did<'static>>> {
    let raw = env::var("SYNDICATION_SEED_DIDS").unwrap_or_default();
    raw.split(',')
        .map(str::trim)
        .filter(|did| !did.is_empty())
        .map(|did| {
            Did::new_owned(did.to_owned())
                .map_err(|error| anyhow!("invalid DID in SYNDICATION_SEED_DIDS ({did}): {error}"))
        })
        .collect()
}

fn env_u64(name: &str, default: u64) -> anyhow::Result<u64> {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            value
                .trim()
                .parse::<u64>()
                .with_context(|| format!("parsing {name}"))
        })
        .transpose()
        .map(|value| value.unwrap_or(default))
}

fn env_u32(name: &str, default: u32) -> anyhow::Result<u32> {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            value
                .trim()
                .parse::<u32>()
                .with_context(|| format!("parsing {name}"))
        })
        .transpose()
        .map(|value| value.unwrap_or(default))
}

async fn serve_http(state: AppState, bind_addr: SocketAddr) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/", get(root))
        .route("/health", get(health))
        .route("/stations", get(list_stations))
        .route("/stations/{did}", get(get_station))
        .route(
            "/xrpc/com.atproto.sync.requestCrawl",
            post(request_crawl),
        )
        .with_state(state)
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive());

    let listener = TcpListener::bind(bind_addr).await?;
    info!(%bind_addr, "serving syndication worker api");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn root() -> Json<RootResponse> {
    Json(RootResponse {
        collection: COLLECTION,
        endpoints: [
            "/health",
            "/stations",
            "/stations/{did}",
            "/xrpc/com.atproto.sync.requestCrawl",
        ],
    })
}

async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    let stations = state.stations.read().await;
    Json(HealthResponse {
        ok: true,
        collection: COLLECTION,
        station_count: stations.len(),
        healthy_station_count: stations.values().filter(|entry| entry.is_visible()).count(),
    })
}

async fn list_stations(State(state): State<AppState>) -> Json<Vec<StationView>> {
    let stations = state.stations.read().await;
    Json(
        stations
            .values()
            .filter(|entry| entry.is_visible())
            .map(|entry| entry.view.clone())
            .collect(),
    )
}

async fn get_station(
    State(state): State<AppState>,
    AxumPath(did): AxumPath<String>,
) -> Result<Json<StationView>, StatusCode> {
    let stations = state.stations.read().await;
    stations
        .get(&did)
        .filter(|entry| entry.is_visible())
        .map(|entry| Json(entry.view.clone()))
        .ok_or(StatusCode::NOT_FOUND)
}

fn request_crawl_source(raw_hostname: &str) -> Result<(String, Url), String> {
    let hostname = raw_hostname.trim().trim_end_matches('/');
    if hostname.is_empty() {
        return Err("hostname is required".to_owned());
    }
    if hostname.contains("://") || hostname.contains('/') || hostname.contains('\\') {
        return Err("hostname must not include a scheme or path".to_owned());
    }

    let url =
        Url::parse(&format!("wss://{hostname}/")).map_err(|error| error.to_string())?;
    let host = url
        .host_str()
        .ok_or_else(|| "hostname must include a valid host".to_owned())?
        .to_owned();
    if !is_public_hostname(&host) {
        return Err("hostname must be publicly reachable".to_owned());
    }

    Ok((host, url))
}

fn is_public_hostname(hostname: &str) -> bool {
    let host = hostname.trim_matches(['[', ']']).to_lowercase();
    if host == "localhost" || host.ends_with(".localhost") || host == "::1" || host.starts_with("127.") {
        return true;
    }
    if host == "0.0.0.0" {
        return false;
    }
    if host.starts_with("10.") || host.starts_with("192.168.") {
        return false;
    }

    let mut parts = host.split('.');
    let first = parts.next().and_then(|part| part.parse::<u8>().ok());
    let second = parts.next().and_then(|part| part.parse::<u8>().ok());
    !matches!((first, second), (Some(172), Some(16..=31)))
}

fn xrpc_error(
    status: StatusCode,
    error: &'static str,
    message: String,
) -> (StatusCode, Json<XrpcErrorResponse>) {
    (status, Json(XrpcErrorResponse { error, message }))
}

fn xrpc_bad_request(
    error: &'static str,
    message: String,
) -> (StatusCode, Json<XrpcErrorResponse>) {
    xrpc_error(StatusCode::BAD_REQUEST, error, message)
}

async fn request_crawl(
    State(state): State<AppState>,
    Json(request): Json<RequestCrawlRequest>,
) -> Result<StatusCode, (StatusCode, Json<XrpcErrorResponse>)> {
    let (hostname, source_url) = request_crawl_source(&request.hostname)
        .map_err(|message| xrpc_bad_request("InvalidHostname", message))?;
    if state.hydrant.pds.is_banned(&hostname) {
        return Err(xrpc_bad_request(
            "HostBanned",
            "host is banned".to_owned(),
        ));
    }

    if state.hydrant.firehose.is_source_running(&source_url) {
        return Ok(StatusCode::OK);
    }

    if !state.hydrant.firehose.is_source_known(&source_url) {
        let mut limiter = state.request_crawl_limiter.lock().await;
        if !limiter.try_increment_new_host() {
            return Err(xrpc_error(
                StatusCode::TOO_MANY_REQUESTS,
                "RateLimitExceeded",
                "daily limit for new PDS sources reached".to_owned(),
            ));
        }
    }

    state
        .hydrant
        .firehose
        .add_source(source_url.clone(), true)
        .await
        .map_err(|error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(XrpcErrorResponse {
                    error: "InternalServerError",
                    message: format!("failed to add pds source: {error}"),
                }),
            )
        })?;

    info!(hostname, source = %source_url, "accepted requestCrawl");
    Ok(StatusCode::OK)
}

async fn health_check_loop(stations: StationIndex, config: HealthConfig) -> anyhow::Result<()> {
    let client = Client::builder()
        .user_agent("sister-radio-syndication-worker/0.1")
        .timeout(config.timeout)
        .build()
        .context("building station health client")?;
    let mut tick = interval(config.interval);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tick.tick().await;
        health_check_once(stations.clone(), &client, &config).await;
    }
}

async fn health_check_once(stations: StationIndex, client: &Client, config: &HealthConfig) {
    let snapshot: Vec<(String, StationView)> = {
        let stations = stations.read().await;
        stations
            .iter()
            .map(|(did, entry)| (did.clone(), entry.view.clone()))
            .collect()
    };

    for (did, station) in snapshot {
        let result = probe_station_health(client, &station).await;
        let checked_at = now_rfc3339();
        let mut stations = stations.write().await;
        let Some(entry) = stations.get_mut(&did) else {
            continue;
        };
        if entry.view.url != station.url || entry.view.rev != station.rev {
            continue;
        }

        entry.health.last_checked_at = Some(checked_at.clone());
        match result {
            Ok(()) => {
                let was_healthy = entry.health.healthy;
                entry.health.healthy = true;
                entry.health.consecutive_failures = 0;
                entry.health.last_healthy_at = Some(checked_at);
                entry.health.last_error = None;
                if !was_healthy {
                    info!(did, url = %entry.view.url, "station became healthy");
                }
            }
            Err(error) => {
                entry.health.consecutive_failures =
                    entry.health.consecutive_failures.saturating_add(1);
                entry.health.last_error = Some(error.clone());
                if entry.health.consecutive_failures >= config.failure_threshold {
                    if entry.health.healthy {
                        warn!(
                            did,
                            url = %entry.view.url,
                            failures = entry.health.consecutive_failures,
                            error,
                            "station became unhealthy"
                        );
                    }
                    entry.health.healthy = false;
                }
            }
        }
    }
}

async fn probe_station_health(client: &Client, station: &StationView) -> Result<(), String> {
    let urls = station_health_urls(&station.url).map_err(|error| error.to_string())?;
    let mut last_error = None;

    for url in urls {
        match client.get(url.clone()).send().await {
            Ok(response) if response.status().is_success() => {
                let status = response.status();
                match response.json::<serde_json::Value>().await {
                    Ok(value)
                        if value.get("ok").and_then(serde_json::Value::as_bool) == Some(true) =>
                    {
                        info!(did = %station.did, url = %station.url, status = status.as_u16(), "station health check passed");
                        return Ok(());
                    }
                    Ok(value) => {
                        last_error = Some(format!("{url} returned non-healthy JSON: {value}"));
                    }
                    Err(error) => {
                        last_error =
                            Some(format!("{url} returned non-JSON health response: {error}"));
                    }
                }
            }
            Ok(response) => {
                last_error = Some(format!("{url} returned {}", response.status()));
            }
            Err(error) => {
                last_error = Some(format!("{url} failed: {error}"));
            }
        }
    }

    Err(last_error.unwrap_or_else(|| "no station health URLs were probeable".to_owned()))
}

fn validate_station_identity(did: &str, station_url: &str) -> Result<(), String> {
    let Some(did_host) = did_web_host(did) else {
        return Err("station DID must be did:web".to_owned());
    };
    let url = Url::parse(station_url).map_err(|error| format!("invalid station url: {error}"))?;
    let url_host = url
        .host_str()
        .ok_or_else(|| "station url must have a hostname".to_owned())?;

    if did_host.eq_ignore_ascii_case(url_host) {
        Ok(())
    } else {
        Err(format!(
            "station url host {url_host} does not match DID host {did_host}"
        ))
    }
}

fn did_web_host(did: &str) -> Option<String> {
    let suffix = did.strip_prefix("did:web:")?;
    let host = suffix.split(':').next()?.trim();
    (!host.is_empty()).then(|| host.to_owned())
}

fn station_health_urls(station_url: &str) -> anyhow::Result<Vec<Url>> {
    let base =
        Url::parse(station_url).with_context(|| format!("parsing station url {station_url}"))?;
    Ok(vec![
        base.join("/api/health")
            .context("building /api/health URL")?,
        base.join("/health").context("building /health URL")?,
    ])
}

async fn collect_station_records(
    stations: StationIndex,
    mut stream: EventStream,
) -> anyhow::Result<()> {
    while let Some(item) = stream.next().await {
        let event = item.map_err(|error| anyhow!("hydrant stream error: {error}"))?;

        if let Some(record) = event.record {
            if record.collection.as_str() != COLLECTION {
                continue;
            }

            let did = record.did.as_str().to_owned();
            match record.action.as_str() {
                "create" | "update" => {
                    let Some(raw) = record.record else {
                        warn!(did, rkey = %record.rkey, "station record event had no record body");
                        continue;
                    };
                    let station = serde_json::from_str::<StationRecord>(raw.get())
                        .with_context(|| format!("parsing station record for {did}"))?;
                    if station
                        .record_type
                        .as_deref()
                        .is_some_and(|record_type| record_type != COLLECTION)
                    {
                        warn!(did, record_type = ?station.record_type, "skipping mismatched station record type");
                        continue;
                    }
                    if let Err(error) = validate_station_identity(&did, &station.url) {
                        warn!(
                            did,
                            url = %station.url,
                            error,
                            "skipping station record with mismatched DID/url binding"
                        );
                        continue;
                    }

                    let view = StationView {
                        uri: format!("at://{did}/{COLLECTION}/{}", record.rkey),
                        cid: record.cid.map(|cid| cid.to_string()),
                        rev: record.rev.to_string(),
                        did: did.clone(),
                        url: station.url,
                        name: station.name,
                        description: station.description,
                        updated_at: station.updated_at,
                        indexed_at: now_rfc3339(),
                    };
                    stations
                        .write()
                        .await
                        .insert(did.clone(), StationEntry::new(view));
                    info!(did, action = %record.action, "indexed station record");
                }
                "delete" => {
                    stations.write().await.remove(&did);
                    info!(did, "removed station record");
                }
                action => {
                    warn!(did, action, "ignoring unknown record action");
                }
            }
        } else if let Some(account) = event.account
            && !account.active
        {
            stations.write().await.remove(account.did.as_str());
        }
    }

    Ok(())
}

fn report_to_anyhow(error: miette::Report) -> anyhow::Error {
    anyhow!("{error:?}")
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn current_utc_day() -> String {
    Utc::now().date_naive().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn station_health_urls_probe_api_health_then_health() {
        let urls = station_health_urls("https://radio.example.com/some/path").unwrap();
        let rendered: Vec<_> = urls.into_iter().map(|url| url.to_string()).collect();
        assert_eq!(
            rendered,
            vec![
                "https://radio.example.com/api/health",
                "https://radio.example.com/health"
            ]
        );
    }

    #[test]
    fn station_identity_requires_did_web_host_to_match_url_host() {
        assert!(
            validate_station_identity("did:web:radio.example.com", "https://radio.example.com")
                .is_ok()
        );
        assert!(
            validate_station_identity("did:web:radio.example.com", "https://other.example.com")
                .is_err()
        );
        assert!(validate_station_identity("did:plc:abc123", "https://radio.example.com").is_err());
    }

    #[test]
    fn request_crawl_source_accepts_public_hostname_only() {
        let (hostname, source) =
            request_crawl_source("radio.example.com").expect("public host accepted");
        assert_eq!(hostname, "radio.example.com");
        assert_eq!(source.as_str(), "wss://radio.example.com/");

        let (_, source_with_port) =
            request_crawl_source("radio.example.com:8443").expect("public host with port accepted");
        assert_eq!(source_with_port.as_str(), "wss://radio.example.com:8443/");

        assert!(request_crawl_source("https://radio.example.com").is_err());
        assert!(request_crawl_source("radio.example.com/path").is_err());
        assert!(request_crawl_source("localhost").is_ok());
        assert!(request_crawl_source("127.0.0.1").is_ok());
        assert!(request_crawl_source("10.0.0.3").is_err());
    }

    #[test]
    fn request_crawl_limiter_caps_new_hosts_per_day() {
        let mut limiter = RequestCrawlLimiter {
            day: current_utc_day(),
            used_new_hosts: 0,
            daily_limit: 1,
        };

        assert!(limiter.try_increment_new_host());
        assert!(!limiter.try_increment_new_host());

        limiter.day = "1999-01-01".to_owned();
        assert!(limiter.try_increment_new_host());
        assert_eq!(limiter.used_new_hosts, 1);
    }
}
