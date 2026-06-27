use std::{collections::BTreeMap, str::FromStr, sync::Arc};

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

use super::{AppState, well_known};

pub(crate) const STATION_COLLECTION: &str = "pet.nkp.radio.station";
pub(crate) const STATION_RKEY: &str = "self";

const PDS_SIGNING_KEY_CONFIG: &str = "pds_signing_key_hex";
const STATION_RECORD_FINGERPRINT_CONFIG: &str = "station_record_fingerprint";
const STATION_RECORD_UPDATED_AT_CONFIG: &str = "station_record_updated_at";
const SUBSCRIBE_REPOS_SEQ: i64 = 1;

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
        let firehose_commit = commit_data
            .to_firehose_commit(
                service_did,
                SUBSCRIBE_REPOS_SEQ,
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
            seq: SUBSCRIBE_REPOS_SEQ,
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

    upsert_pds_config(pool, STATION_RECORD_FINGERPRINT_CONFIG, &fingerprint)
        .await
        .context("storing station record fingerprint")?;
    upsert_pds_config(pool, STATION_RECORD_UPDATED_AT_CONFIG, now)
        .await
        .context("storing station record updatedAt")?;

    Ok(now.to_owned())
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

    if params
        .cid
        .as_deref()
        .is_some_and(|expected| expected != state.pds.station_cid)
    {
        return Err(xrpc_error(
            StatusCode::NOT_FOUND,
            "RecordNotFound",
            "record cid not found",
        ));
    }

    Ok(Json(GetRecordOutput {
        uri: state.pds.station_uri.clone(),
        cid: state.pds.station_cid.clone(),
        value: state.pds.station_value.clone(),
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

    let records = if include_record {
        vec![RecordView {
            uri: state.pds.station_uri.clone(),
            cid: state.pds.station_cid.clone(),
            value: state.pds.station_value.clone(),
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

    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/vnd.ipld.car")],
        state.pds.repo_car.clone(),
    )
        .into_response())
}

pub(crate) async fn list_repos(
    State(state): State<AppState>,
    Query(params): Query<ListReposParams>,
) -> XrpcResult<ListReposOutput> {
    let include_repo = params.limit.unwrap_or(500) > 0 && params.cursor.is_none();
    let repos = if include_repo {
        vec![RepoView {
            did: state.service_did.as_str().to_owned(),
            head: state.pds.head.clone(),
            rev: state.pds.rev.clone(),
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

pub(crate) async fn subscribe_repos(
    State(state): State<AppState>,
    Query(params): Query<SubscribeReposParams>,
    ws: WebSocketUpgrade,
) -> Response {
    let cursor = params.cursor.unwrap_or(0);
    if cursor > state.pds.seq {
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
    if cursor < state.pds.seq
        && socket
            .send(Message::Binary(state.pds.commit_frame.clone()))
            .await
            .is_err()
    {
        return;
    }

    while let Some(message) = socket.recv().await {
        if !matches!(
            message,
            Ok(Message::Ping(_))
                | Ok(Message::Pong(_))
                | Ok(Message::Text(_))
                | Ok(Message::Binary(_))
        ) {
            break;
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

fn endpoint_host(url: &str) -> Option<String> {
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let host = without_scheme.split('/').next()?.split('@').next_back()?;
    let host = host.split(':').next()?.trim();
    (!host.is_empty()).then(|| host.to_owned())
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
                assert_eq!(commit.seq, SUBSCRIBE_REPOS_SEQ);
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
