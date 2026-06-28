use std::{collections::BTreeMap, env, net::SocketAddr, path::Path, sync::Arc};

use anyhow::{Context, anyhow};
use axum::{
    Json, Router,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    routing::get,
};
use chrono::{SecondsFormat, Utc};
use hydrant::{
    FilterMode,
    config::Config,
    control::{EventStream, Hydrant},
    deps::{futures::StreamExt, jacquard::types::did::Did},
};
use serde::{Deserialize, Serialize};
use tokio::{net::TcpListener, sync::RwLock};
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

const COLLECTION: &str = "pet.nkp.radio.station";

type StationIndex = Arc<RwLock<BTreeMap<String, StationView>>>;

#[derive(Clone)]
struct AppState {
    stations: StationIndex,
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RootResponse {
    collection: &'static str,
    endpoints: [&'static str; 3],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HealthResponse {
    ok: bool,
    collection: &'static str,
    station_count: usize,
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
    };
    let stream = hydrant.subscribe(Some(0));
    let hydrant_runner = hydrant.run().map_err(report_to_anyhow)?;

    tokio::select! {
        result = hydrant_runner => result.map_err(report_to_anyhow).context("hydrant stopped")?,
        result = serve_http(app_state, bind_addr) => result.context("http server stopped")?,
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

async fn serve_http(state: AppState, bind_addr: SocketAddr) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/", get(root))
        .route("/health", get(health))
        .route("/stations", get(list_stations))
        .route("/stations/{did}", get(get_station))
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
        endpoints: ["/health", "/stations", "/stations/{did}"],
    })
}

async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        ok: true,
        collection: COLLECTION,
        station_count: state.stations.read().await.len(),
    })
}

async fn list_stations(State(state): State<AppState>) -> Json<Vec<StationView>> {
    let stations = state.stations.read().await;
    Json(stations.values().cloned().collect())
}

async fn get_station(
    State(state): State<AppState>,
    AxumPath(did): AxumPath<String>,
) -> Result<Json<StationView>, StatusCode> {
    let stations = state.stations.read().await;
    stations
        .get(&did)
        .cloned()
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
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

                    let view = StationView {
                        uri: format!("at://{did}/{COLLECTION}/{}", record.rkey),
                        cid: record.cid.map(|cid| cid.to_string()),
                        rev: record.rev.to_string(),
                        did: did.clone(),
                        url: station.url,
                        name: station.name,
                        description: station.description,
                        updated_at: station.updated_at,
                        indexed_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                    };
                    stations.write().await.insert(did.clone(), view);
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
