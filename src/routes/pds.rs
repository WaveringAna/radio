use std::{collections::BTreeMap, str::FromStr, sync::Arc, time::Duration};

use anyhow::{Context, bail};
use axum::{
    Json,
    extract::{
        Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use jacquard_api::com_atproto::sync::subscribe_repos::RepoOpAction;
use jacquard_common::{
    CowStr,
    types::{
        cid::CidLink,
        crypto::{KeyCodec, PublicKey},
        string::{Datetime, Did, Nsid, RecordKey},
        value::RawData,
    },
};
use jacquard_repo::{
    MemoryBlockStore, Repository,
    car::write_car_bytes,
    commit::{SigningKey as RepoSigningKey, firehose::RepoOp},
    mst::RecordWriteOp,
};
use k256::ecdsa::SigningKey;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use sqlx::SqlitePool;
use tokio::sync::broadcast;

use super::{AppState, well_known};

pub(crate) const STATION_COLLECTION: &str = "pet.nkp.radio.station";
pub(crate) const STATION_RKEY: &str = "self";

const PDS_SIGNING_KEY_CONFIG: &str = "pds_signing_key_hex";
const STATION_RECORD_FINGERPRINT_CONFIG: &str = "station_record_fingerprint";
const STATION_RECORD_UPDATED_AT_CONFIG: &str = "station_record_updated_at";
const REQUEST_CRAWL_PATH: &str = "/xrpc/com.atproto.sync.requestCrawl";

#[derive(Clone)]
pub(crate) struct EmbeddedPds {
    station_uri: String,
    station_cid: String,
    station_value: serde_json::Value,
    repo_car: Bytes,
    commit_frame: Bytes,
    seq: i64,
    head: String,
    rev: String,
    public_key_multibase: String,
}

impl EmbeddedPds {
    pub(crate) async fn new(
        service_did: &Did<'static>,
        station_url: &str,
        station_name: &str,
        station_description: Option<&str>,
        updated_at: &str,
        signing_key: &SigningKey,
    ) -> anyhow::Result<Self> {
        let storage = Arc::new(MemoryBlockStore::new());
        let collection = Nsid::new_static(STATION_COLLECTION).context("validating station nsid")?;
        let rkey = RecordKey::any_static(STATION_RKEY).context("validating station record key")?;
        let station_record =
            station_record_map(station_url, station_name, station_description, updated_at);
        let station_value =
            serde_json::to_value(&station_record).context("serializing station record to json")?;

        let write = RecordWriteOp::Create {
            collection: collection.clone(),
            rkey: rkey.clone(),
            record: station_record,
        };
        let commit_data = Repository::format_init_commit(
            storage.clone(),
            service_did.clone(),
            signing_key,
            Some(&[write]),
        )
        .await?;
        let repo_car =
            Bytes::from(write_car_bytes(commit_data.cid, commit_data.blocks.clone()).await?);
        let repo = Repository::create_from_commit(storage, commit_data.clone()).await?;
        let record_cid = repo
            .get_record(&collection, &rkey)
            .await?
            .context("station record missing from embedded repo")?;
        let record_path = station_record_path();
        let seq = station_event_seq();
        let firehose_commit = commit_data
            .to_firehose_commit(
                service_did,
                seq,
                Datetime::from_str(updated_at).context("parsing station updatedAt")?,
                vec![RepoOp {
                    action: RepoOpAction::Create,
                    cid: Some(CidLink::from(record_cid)),
                    path: CowStr::copy_from_str(&record_path),
                    prev: None,
                    extra_data: None,
                }],
                Vec::new(),
            )
            .await?;

        Ok(Self {
            station_uri: station_record_uri(service_did),
            station_cid: record_cid.to_string(),
            station_value,
            repo_car,
            commit_frame: encode_commit_frame(&firehose_commit)?,
            seq,
            head: commit_data.cid.to_string(),
            rev: commit_data.rev.to_string(),
            public_key_multibase: public_key_multibase(signing_key),
        })
    }

    pub(crate) fn public_key_multibase(&self) -> &str {
        &self.public_key_multibase
    }
}

#[derive(Deserialize)]
pub(crate) struct DescribeRepoParams {
    repo: String,
}

#[derive(Deserialize)]
pub(crate) struct GetRecordParams {
    repo: String,
    collection: String,
    rkey: String,
    cid: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct ListRecordsParams {
    repo: String,
    collection: String,
    limit: Option<usize>,
    cursor: Option<String>,
    #[allow(dead_code)]
    reverse: Option<bool>,
}

#[derive(Deserialize)]
pub(crate) struct GetRepoParams {
    did: String,
    #[allow(dead_code)]
    since: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct SyncGetRecordParams {
    did: String,
    #[allow(dead_code)]
    collection: String,
    #[allow(dead_code)]
    rkey: String,
}

#[derive(Deserialize)]
pub(crate) struct ListReposParams {
    #[allow(dead_code)]
    cursor: Option<String>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
pub(crate) struct SubscribeReposParams {
    cursor: Option<i64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DescribeServerOutput {
    did: String,
    invite_code_required: bool,
    available_user_domains: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DescribeRepoOutput {
    handle: String,
    did: String,
    did_doc: serde_json::Value,
    collections: Vec<&'static str>,
    handle_is_correct: bool,
}

#[derive(Serialize)]
pub(crate) struct GetRecordOutput {
    uri: String,
    cid: String,
    value: serde_json::Value,
}

#[derive(Serialize)]
pub(crate) struct ListRecordsOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    cursor: Option<String>,
    records: Vec<RecordView>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ListReposOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    cursor: Option<String>,
    repos: Vec<RepoView>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RelayCrawlReport {
    pub(crate) relay: String,
    pub(crate) hostname: String,
    pub(crate) ok: bool,
    pub(crate) status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RelayCrawlResponse {
    hostname: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rev: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seq: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_at: Option<String>,
    relays: Vec<RelayCrawlReport>,
}

#[derive(Serialize)]
struct RecordView {
    uri: String,
    cid: String,
    value: serde_json::Value,
}

#[derive(Serialize)]
struct RepoView {
    did: String,
    head: String,
    rev: String,
    active: bool,
}

#[derive(Serialize)]
struct EventHeader {
    op: i64,
    t: &'static str,
}

#[derive(Debug, Serialize)]
pub(crate) struct XrpcError {
    error: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<&'static str>,
}

type XrpcResult<T> = Result<Json<T>, (StatusCode, Json<XrpcError>)>;
type XrpcResponseResult = Result<Response, (StatusCode, Json<XrpcError>)>;

pub(crate) async fn load_or_create_signing_key(pool: &SqlitePool) -> anyhow::Result<SigningKey> {
    if let Some(value) = std::env::var("PDS_SIGNING_KEY_HEX")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
    {
        return signing_key_from_hex(&value).context("parsing PDS_SIGNING_KEY_HEX");
    }

    if let Some(row) = sqlx::query_scalar::<_, String>("select value from pds_config where key = ?")
        .bind(PDS_SIGNING_KEY_CONFIG)
        .fetch_optional(pool)
        .await
        .context("loading pds signing key")?
    {
        return signing_key_from_hex(&row).context("parsing stored pds signing key");
    }

    let signing_key = SigningKey::random(&mut OsRng);
    let key_hex = hex::encode(signing_key.to_bytes());
    sqlx::query("insert into pds_config (key, value, updated_at) values (?, ?, unixepoch())")
        .bind(PDS_SIGNING_KEY_CONFIG)
        .bind(&key_hex)
        .execute(pool)
        .await
        .context("storing generated pds signing key")?;

    Ok(signing_key)
}

pub(crate) async fn load_or_update_station_updated_at(
    pool: &SqlitePool,
    station_url: &str,
    station_name: &str,
    station_description: Option<&str>,
    now: &str,
) -> anyhow::Result<String> {
    let fingerprint = serde_json::to_string(&serde_json::json!({
        "url": station_url,
        "name": station_name,
        "description": station_description,
    }))
    .context("serializing station record fingerprint")?;
    let stored_fingerprint = pds_config_value(pool, STATION_RECORD_FINGERPRINT_CONFIG)
        .await
        .context("loading station record fingerprint")?;
    let stored_updated_at = pds_config_value(pool, STATION_RECORD_UPDATED_AT_CONFIG)
        .await
        .context("loading station record updatedAt")?;

    if stored_fingerprint.as_deref() == Some(fingerprint.as_str()) {
        if let Some(updated_at) = stored_updated_at {
            return Ok(updated_at);
        }
    }

    store_station_updated_at(pool, station_url, station_name, station_description, now).await
}

pub(crate) async fn store_station_updated_at(
    pool: &SqlitePool,
    station_url: &str,
    station_name: &str,
    station_description: Option<&str>,
    updated_at: &str,
) -> anyhow::Result<String> {
    let fingerprint = serde_json::to_string(&serde_json::json!({
        "url": station_url,
        "name": station_name,
        "description": station_description,
    }))
    .context("serializing station record fingerprint")?;
    upsert_pds_config(pool, STATION_RECORD_FINGERPRINT_CONFIG, &fingerprint)
        .await
        .context("storing station record fingerprint")?;
    upsert_pds_config(pool, STATION_RECORD_UPDATED_AT_CONFIG, updated_at)
        .await
        .context("storing station record updatedAt")?;

    Ok(updated_at.to_owned())
}

async fn pds_config_value(pool: &SqlitePool, key: &str) -> anyhow::Result<Option<String>> {
    sqlx::query_scalar::<_, String>("select value from pds_config where key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .context("loading pds config value")
}

async fn upsert_pds_config(pool: &SqlitePool, key: &str, value: &str) -> anyhow::Result<()> {
    sqlx::query(
        "insert into pds_config (key, value, updated_at) values (?, ?, unixepoch())
         on conflict(key) do update set value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(key)
    .bind(value)
    .execute(pool)
    .await
    .context("upserting pds config value")?;

    Ok(())
}

pub(crate) async fn describe_server(State(state): State<AppState>) -> Json<DescribeServerOutput> {
    Json(DescribeServerOutput {
        did: state.service_did.as_str().to_owned(),
        invite_code_required: true,
        available_user_domains: station_user_domains(&state),
    })
}

pub(crate) async fn describe_repo(
    State(state): State<AppState>,
    Query(params): Query<DescribeRepoParams>,
) -> XrpcResult<DescribeRepoOutput> {
    ensure_repo(&state, &params.repo)?;

    Ok(Json(DescribeRepoOutput {
        handle: state.service_did.as_str().to_owned(),
        did: state.service_did.as_str().to_owned(),
        did_doc: well_known::did_document(&state),
        collections: vec![STATION_COLLECTION],
        handle_is_correct: true,
    }))
}

pub(crate) async fn get_record(
    State(state): State<AppState>,
    Query(params): Query<GetRecordParams>,
) -> XrpcResult<GetRecordOutput> {
    ensure_repo(&state, &params.repo)?;
    if params.collection != STATION_COLLECTION || params.rkey != STATION_RKEY {
        return Err(xrpc_error(
            StatusCode::NOT_FOUND,
            "RecordNotFound",
            "record not found",
        ));
    }

    let pds = state.pds.read().await;
    if params
        .cid
        .as_deref()
        .is_some_and(|expected| expected != pds.station_cid)
    {
        return Err(xrpc_error(
            StatusCode::NOT_FOUND,
            "RecordNotFound",
            "record cid not found",
        ));
    }

    Ok(Json(GetRecordOutput {
        uri: pds.station_uri.clone(),
        cid: pds.station_cid.clone(),
        value: pds.station_value.clone(),
    }))
}

pub(crate) async fn list_records(
    State(state): State<AppState>,
    Query(params): Query<ListRecordsParams>,
) -> XrpcResult<ListRecordsOutput> {
    ensure_repo(&state, &params.repo)?;

    let include_record = params.collection == STATION_COLLECTION
        && params.cursor.is_none()
        && params.limit.unwrap_or(50) > 0;

    let pds = state.pds.read().await;
    let records = if include_record {
        vec![RecordView {
            uri: pds.station_uri.clone(),
            cid: pds.station_cid.clone(),
            value: pds.station_value.clone(),
        }]
    } else {
        Vec::new()
    };

    Ok(Json(ListRecordsOutput {
        cursor: None,
        records,
    }))
}

pub(crate) async fn get_repo(
    State(state): State<AppState>,
    Query(params): Query<GetRepoParams>,
) -> XrpcResponseResult {
    ensure_repo(&state, &params.did)?;

    let pds = state.pds.read().await;
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/vnd.ipld.car")],
        pds.repo_car.clone(),
    )
        .into_response())
}

pub(crate) async fn sync_get_record(
    State(state): State<AppState>,
    Query(params): Query<SyncGetRecordParams>,
) -> XrpcResponseResult {
    ensure_repo(&state, &params.did)?;

    let pds = state.pds.read().await;
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/vnd.ipld.car")],
        pds.repo_car.clone(),
    )
        .into_response())
}

pub(crate) async fn list_repos(
    State(state): State<AppState>,
    Query(params): Query<ListReposParams>,
) -> XrpcResult<ListReposOutput> {
    let include_repo = params.limit.unwrap_or(500) > 0 && params.cursor.is_none();
    let pds = state.pds.read().await;
    let repos = if include_repo {
        vec![RepoView {
            did: state.service_did.as_str().to_owned(),
            head: pds.head.clone(),
            rev: pds.rev.clone(),
            active: true,
        }]
    } else {
        Vec::new()
    };

    Ok(Json(ListReposOutput {
        cursor: None,
        repos,
    }))
}

pub(crate) async fn announce_station_to_relays(State(state): State<AppState>) -> Response {
    let Some(hostname) = announce_hostname(&state.service_endpoint)
        .or_else(|| announce_hostname(&state.station_url))
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "public_hostname_required",
                "message": "station announce requires a public http(s) service endpoint"
            })),
        )
            .into_response();
    };

    let updated_at = rfc3339_now();
    let pds = match rebuild_station_pds(&state, &updated_at).await {
        Ok(pds) => pds,
        Err(error) => {
            tracing::error!(%error, "failed to reemit station record");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "station_reemit_failed",
                    "message": "failed to rebuild station record"
                })),
            )
                .into_response();
        }
    };
    let cid = pds.station_cid.clone();
    let rev = pds.rev.clone();
    let seq = pds.seq;
    let commit_frame = pds.commit_frame.clone();
    *state.pds.write().await = pds;
    let _ = state.pds_events.send(commit_frame);

    let relays = request_relay_crawls(&state.station_announce_relays, &hostname).await;
    Json(RelayCrawlResponse {
        hostname,
        cid: Some(cid),
        rev: Some(rev),
        seq: Some(seq),
        updated_at: Some(updated_at),
        relays,
    })
    .into_response()
}

async fn rebuild_station_pds(state: &AppState, updated_at: &str) -> anyhow::Result<EmbeddedPds> {
    store_station_updated_at(
        &state.pds_pool,
        &state.station_url,
        &state.station_name,
        state.station_description.as_deref(),
        updated_at,
    )
    .await?;

    EmbeddedPds::new(
        &state.service_did,
        &state.station_url,
        &state.station_name,
        state.station_description.as_deref(),
        updated_at,
        state.pds_signing_key.as_ref(),
    )
    .await
}

pub(crate) async fn request_relay_crawls(
    relays: &[String],
    hostname: &str,
) -> Vec<RelayCrawlReport> {
    let Ok(client) = reqwest::Client::builder()
        .user_agent("sister-radio/0.1")
        .timeout(Duration::from_secs(10))
        .build()
    else {
        return relays
            .iter()
            .map(|relay| RelayCrawlReport {
                relay: relay.clone(),
                hostname: hostname.to_owned(),
                ok: false,
                status: None,
                error: Some("failed to construct http client".to_owned()),
            })
            .collect();
    };

    let mut reports = Vec::with_capacity(relays.len());
    for relay in relays {
        reports.push(request_relay_crawl(&client, relay, hostname).await);
    }
    reports
}

pub(crate) async fn subscribe_repos(
    State(state): State<AppState>,
    Query(params): Query<SubscribeReposParams>,
    ws: WebSocketUpgrade,
) -> Response {
    let cursor = params.cursor.unwrap_or(0);
    let seq = state.pds.read().await.seq;
    if cursor > seq {
        return xrpc_error(
            StatusCode::BAD_REQUEST,
            "FutureCursor",
            "cursor is ahead of this embedded pds",
        )
        .into_response();
    }

    ws.on_upgrade(move |socket| subscribe_repos_socket(state, cursor, socket))
        .into_response()
}

async fn subscribe_repos_socket(state: AppState, cursor: i64, mut socket: WebSocket) {
    let mut pds_events = state.pds_events.subscribe();
    let commit_frame = {
        let pds = state.pds.read().await;
        (cursor < pds.seq).then(|| pds.commit_frame.clone())
    };
    if let Some(commit_frame) = commit_frame {
        if socket.send(Message::Binary(commit_frame)).await.is_err() {
            return;
        }
    }

    loop {
        tokio::select! {
            message = socket.recv() => {
                match message {
                    Some(Ok(Message::Ping(_)))
                    | Some(Ok(Message::Pong(_)))
                    | Some(Ok(Message::Text(_)))
                    | Some(Ok(Message::Binary(_))) => {}
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                }
            }
            event = pds_events.recv() => {
                match event {
                    Ok(commit_frame) => {
                        if socket.send(Message::Binary(commit_frame)).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

fn ensure_repo(state: &AppState, repo: &str) -> Result<(), (StatusCode, Json<XrpcError>)> {
    if repo == state.service_did.as_str() {
        Ok(())
    } else {
        Err(xrpc_error(
            StatusCode::NOT_FOUND,
            "RepoNotFound",
            "repo not found",
        ))
    }
}

pub(crate) async fn xrpc_not_found() -> (StatusCode, Json<XrpcError>) {
    xrpc_error(
        StatusCode::NOT_FOUND,
        "MethodNotFound",
        "xrpc method not found",
    )
}

fn station_record_map(
    station_url: &str,
    station_name: &str,
    station_description: Option<&str>,
    updated_at: &str,
) -> BTreeMap<SmolStr, RawData<'static>> {
    let mut record = BTreeMap::new();
    record.insert("$type".into(), raw_string(STATION_COLLECTION));
    record.insert("url".into(), raw_string(station_url));
    record.insert("name".into(), raw_string(station_name));
    if let Some(description) = station_description.filter(|value| !value.is_empty()) {
        record.insert("description".into(), raw_string(description));
    }
    record.insert("updatedAt".into(), raw_string(updated_at));
    record
}

fn raw_string(value: &str) -> RawData<'static> {
    RawData::String(CowStr::copy_from_str(value))
}

fn station_record_uri(did: &Did<'_>) -> String {
    format!("at://{did}/{}/{}", STATION_COLLECTION, STATION_RKEY)
}

fn station_record_path() -> String {
    format!("{STATION_COLLECTION}/{STATION_RKEY}")
}

fn encode_commit_frame(
    commit: &jacquard_repo::commit::firehose::FirehoseCommit<'static>,
) -> anyhow::Result<Bytes> {
    let mut frame = serde_ipld_dagcbor::to_vec(&EventHeader {
        op: 1,
        t: "#commit",
    })
    .context("encoding subscribeRepos frame header")?;
    frame
        .extend(serde_ipld_dagcbor::to_vec(commit).context("encoding subscribeRepos commit body")?);
    Ok(Bytes::from(frame))
}

fn public_key_multibase(signing_key: &SigningKey) -> String {
    let public_key = PublicKey {
        codec: KeyCodec::Secp256k1,
        bytes: RepoSigningKey::public_key(signing_key).into(),
    };
    let mut bytes = encode_uvarint(public_key_codec(&public_key));
    bytes.extend_from_slice(public_key.bytes.as_ref());
    multibase::encode(multibase::Base::Base58Btc, bytes)
}

fn public_key_codec(public_key: &PublicKey<'_>) -> u64 {
    match public_key.codec {
        KeyCodec::Ed25519 => 0xed,
        KeyCodec::Secp256k1 => 0xe7,
        KeyCodec::P256 => 0x1200,
        KeyCodec::Unknown(code) => code,
    }
}

fn encode_uvarint(mut value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    while value >= 0x80 {
        out.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
    out
}

fn signing_key_from_hex(value: &str) -> anyhow::Result<SigningKey> {
    let value = value.trim().trim_start_matches("0x");
    let bytes = hex::decode(value).context("decoding key hex")?;
    if bytes.len() != 32 {
        bail!(
            "expected 32-byte secp256k1 signing key, got {} bytes",
            bytes.len()
        );
    }
    SigningKey::from_slice(&bytes).context("constructing secp256k1 signing key")
}

fn station_user_domains(state: &AppState) -> Vec<String> {
    endpoint_host(&state.station_url)
        .or_else(|| endpoint_host(&state.service_endpoint))
        .map(|host| vec![format!(".{host}")])
        .unwrap_or_default()
}

fn station_event_seq() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp().max(1)
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

pub(crate) fn announce_hostname(url: &str) -> Option<String> {
    let host = endpoint_host(url)?;
    is_public_hostname(&host).then_some(host)
}

fn endpoint_host(url: &str) -> Option<String> {
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let host = without_scheme.split('/').next()?.split('@').next_back()?;
    let host = host.split(':').next()?.trim();
    (!host.is_empty()).then(|| host.to_owned())
}

fn is_public_hostname(hostname: &str) -> bool {
    let host = hostname.to_lowercase();
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

async fn request_relay_crawl(
    client: &reqwest::Client,
    relay: &str,
    hostname: &str,
) -> RelayCrawlReport {
    let relay = relay.trim().trim_end_matches('/').to_owned();
    let url = match request_crawl_url(&relay) {
        Ok(url) => url,
        Err(error) => {
            return RelayCrawlReport {
                relay,
                hostname: hostname.to_owned(),
                ok: false,
                status: None,
                error: Some(error),
            };
        }
    };

    let body = serde_json::json!({ "hostname": hostname }).to_string();
    match client
        .post(url)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await
    {
        Ok(response) => {
            let status = response.status();
            let ok = status.is_success();
            let error = if ok {
                None
            } else {
                Some(
                    response
                        .text()
                        .await
                        .unwrap_or_else(|_| "relay request failed".to_owned()),
                )
            };
            RelayCrawlReport {
                relay,
                hostname: hostname.to_owned(),
                ok,
                status: Some(status.as_u16()),
                error,
            }
        }
        Err(error) => RelayCrawlReport {
            relay,
            hostname: hostname.to_owned(),
            ok: false,
            status: None,
            error: Some(error.to_string()),
        },
    }
}

fn request_crawl_url(relay: &str) -> Result<reqwest::Url, String> {
    let normalized = relay
        .strip_prefix("wss://")
        .map(|host| format!("https://{host}"))
        .or_else(|| {
            relay
                .strip_prefix("ws://")
                .map(|host| format!("http://{host}"))
        })
        .unwrap_or_else(|| relay.to_owned());
    let base = reqwest::Url::parse(&normalized).map_err(|error| error.to_string())?;
    base.join(REQUEST_CRAWL_PATH)
        .map_err(|error| error.to_string())
}

fn xrpc_error(
    status: StatusCode,
    error: &'static str,
    message: &'static str,
) -> (StatusCode, Json<XrpcError>) {
    (
        status,
        Json(XrpcError {
            error,
            message: Some(message),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use jacquard_api::com_atproto::sync::subscribe_repos::SubscribeReposMessage;

    #[test]
    fn endpoint_host_extracts_http_host_without_port() {
        assert_eq!(
            endpoint_host("https://radio.example.com:3000/path").as_deref(),
            Some("radio.example.com")
        );
    }

    #[test]
    fn announce_hostname_requires_public_http_host() {
        assert_eq!(
            announce_hostname("https://radio.example.com:3000/path").as_deref(),
            Some("radio.example.com")
        );
        assert_eq!(announce_hostname("http://127.0.0.1:3000").as_deref(), Some("127.0.0.1"));
        assert_eq!(announce_hostname("https://localhost:3000").as_deref(), Some("localhost"));
        assert_eq!(announce_hostname("https://10.0.0.5"), None);
        assert_eq!(announce_hostname("https://172.16.0.1"), None);
        assert_eq!(
            announce_hostname("https://stream-roof-records-auto.trycloudflare.com").as_deref(),
            Some("stream-roof-records-auto.trycloudflare.com")
        );
    }

    #[test]
    fn request_crawl_url_accepts_http_or_websocket_relay_base() {
        assert_eq!(
            request_crawl_url("wss://relay.fire.hose.cam/")
                .unwrap()
                .as_str(),
            "https://relay.fire.hose.cam/xrpc/com.atproto.sync.requestCrawl"
        );
        assert_eq!(
            request_crawl_url("https://relay.fire.hose.cam")
                .unwrap()
                .as_str(),
            "https://relay.fire.hose.cam/xrpc/com.atproto.sync.requestCrawl"
        );
    }

    #[test]
    fn generated_public_key_is_valid_multikey() {
        let signing_key = SigningKey::random(&mut OsRng);
        let decoded = PublicKey::decode(&public_key_multibase(&signing_key)).unwrap();
        assert_eq!(decoded.codec, KeyCodec::Secp256k1);
        assert_eq!(decoded.bytes.len(), 33);
    }

    #[tokio::test]
    async fn embedded_pds_builds_decodable_commit_frame() {
        let did = Did::new_static("did:plc:test").unwrap();
        let signing_key = SigningKey::random(&mut OsRng);
        let pds = EmbeddedPds::new(
            &did,
            "https://radio.example.com",
            "radio",
            Some("test station"),
            "2026-06-27T00:00:00Z",
            &signing_key,
        )
        .await
        .unwrap();

        let message = SubscribeReposMessage::decode_framed(&pds.commit_frame).unwrap();
        match message {
            SubscribeReposMessage::Commit(commit) => {
                assert_eq!(commit.seq, pds.seq);
                assert!(commit.seq > 0);
                assert_eq!(commit.repo.as_str(), did.as_str());
                assert_eq!(commit.ops.len(), 1);
                assert_eq!(commit.ops[0].path.as_ref(), station_record_path());
            }
            _ => panic!("expected commit frame"),
        }
    }

    #[tokio::test]
    async fn station_record_cid_changes_with_record_value() {
        let did = Did::new_static("did:plc:test").unwrap();
        let signing_key = SigningKey::random(&mut OsRng);
        let first = EmbeddedPds::new(
            &did,
            "https://one.example",
            "radio",
            None,
            "2026-06-27T00:00:00Z",
            &signing_key,
        )
        .await
        .unwrap();
        let second = EmbeddedPds::new(
            &did,
            "https://two.example",
            "radio",
            None,
            "2026-06-27T00:00:00Z",
            &signing_key,
        )
        .await
        .unwrap();

        assert_ne!(first.station_cid, second.station_cid);
    }
}
