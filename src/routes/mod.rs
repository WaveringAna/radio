mod admin;
mod chat;
pub(crate) mod helpers;
pub(crate) mod pds;
mod radio;
mod songs;
mod subsonic_import;
mod upload;
mod well_known;
mod xrpc;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::Error;
use axum::{
    Json, Router,
    extract::DefaultBodyLimit,
    http::StatusCode,
    routing::{any, get, post},
};
use bytes::Bytes;
use jacquard::{
    identity::{JacquardResolver, resolver::ResolverOptions},
    types::did::Did,
};
use jacquard_axum::{IntoRouter, service_auth::ServiceAuth};
use k256::ecdsa::SigningKey;
use radio_lexicons::pet_nkp::radio::{
    admin::modify::ModifyRequest as AdminModifyRequest,
    admin::permissions::PermissionsRequest as AdminPermissionsRequest,
    albums::list::ListRequest as AlbumsListRequest,
    albums::modify::ModifyRequest as AlbumsModifyRequest,
    chat::bans::list::ListRequest as ChatBansListRequest,
    chat::bans::modify::ModifyRequest as ChatBansModifyRequest,
    chat::messages::modify::ModifyRequest as ChatMessagesModifyRequest,
    chat::send::SendRequest as ChatSendRequest, control::ControlRequest,
    playlists::list::ListRequest as PlaylistsListRequest,
    playlists::modify::ModifyRequest as PlaylistsModifyRequest,
    queue::list::ListRequest as QueueListRequest,
    queue::modify::ModifyRequest as QueueModifyRequest, songs::add::AddRequest as SongsAddRequest,
    songs::cover::CoverRequest as SongsCoverRequest, songs::list::ListRequest as SongsListRequest,
    songs::modify::ModifyRequest as SongsModifyRequest,
    songs::upload::UploadRequest as SongsUploadRequest,
    subsonic::import::ImportRequest as SubsonicImportRequest,
    subsonic::search::SearchRequest as SubsonicSearchRequest,
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tokio::sync::{RwLock, broadcast};
use tower_http::{
    cors::CorsLayer,
    services::{ServeDir, ServeFile},
};

use crate::{auth::AuthService, chat::ChatService, radio::RadioService};

pub(crate) const VIEWER_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(60);
pub(crate) const VIEWER_KEEPALIVE_GRACE: Duration = Duration::from_secs(10);
pub(crate) const MAX_VIEWER_ID_LEN: usize = 128;

// ── AppState ──

/// Shared application state for HTTP routes.
#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) auth: Arc<AuthService>,
    pub(crate) radio: Arc<RadioService>,
    pub(crate) chat: Arc<ChatService>,
    pub(crate) service_did: Did<'static>,
    pub(crate) service_endpoint: String,
    pub(crate) service_ids: Vec<String>,
    pub(crate) station_announce_relays: Vec<String>,
    pub(crate) pds: Arc<RwLock<pds::EmbeddedPds>>,
    pub(crate) pds_events: broadcast::Sender<Bytes>,
    pub(crate) pds_pool: SqlitePool,
    pub(crate) pds_public_key_multibase: String,
    pub(crate) pds_signing_key: Arc<SigningKey>,
    pub(crate) station_url: String,
    pub(crate) station_name: String,
    pub(crate) station_description: Option<String>,
    service_auth_resolver: JacquardResolver,
    viewers: ViewerTracker,
}

impl AppState {
    /// Creates route state from the auth, radio, chat, and service-auth DID.
    pub(crate) fn new(
        auth: AuthService,
        radio: RadioService,
        chat: ChatService,
        service_did: Did<'static>,
        service_endpoint: String,
        service_ids: Vec<String>,
        station_announce_relays: Vec<String>,
        pds: pds::EmbeddedPds,
        pds_pool: SqlitePool,
        pds_signing_key: SigningKey,
        station_url: String,
        station_name: String,
        station_description: Option<String>,
    ) -> Self {
        let pds_public_key_multibase = pds.public_key_multibase().to_owned();
        let (pds_events, _) = broadcast::channel(32);
        Self {
            auth: Arc::new(auth),
            radio: Arc::new(radio),
            chat: Arc::new(chat),
            service_did,
            service_endpoint,
            service_ids,
            station_announce_relays,
            pds: Arc::new(RwLock::new(pds)),
            pds_events,
            pds_pool,
            pds_public_key_multibase,
            pds_signing_key: Arc::new(pds_signing_key),
            station_url,
            station_name,
            station_description,
            service_auth_resolver: JacquardResolver::new(
                reqwest::Client::new(),
                ResolverOptions::default(),
            ),
            viewers: ViewerTracker::new(),
        }
    }
}

impl ServiceAuth for AppState {
    type Resolver = JacquardResolver;

    fn service_did(&self) -> &Did<'_> {
        &self.service_did
    }

    fn resolver(&self) -> &Self::Resolver {
        &self.service_auth_resolver
    }

    fn require_lxm(&self) -> bool {
        true
    }
}

// ── Viewer tracking ──

#[derive(Clone, Debug, Default)]
struct ViewerEntry {
    connections: usize,
    did: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ViewerStats {
    pub(crate) count: usize,
    pub(crate) listener_dids: Vec<String>,
}

#[derive(Clone)]
struct ViewerTracker {
    inner: Arc<Mutex<HashMap<String, ViewerEntry>>>,
    events: broadcast::Sender<ViewerStats>,
}

impl ViewerTracker {
    fn new() -> Self {
        let (events, _) = broadcast::channel(32);
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            events,
        }
    }

    fn stats(&self) -> ViewerStats {
        let viewers = self.inner.lock().expect("viewer tracker mutex poisoned");
        Self::collect_stats(&viewers)
    }

    fn collect_stats(viewers: &HashMap<String, ViewerEntry>) -> ViewerStats {
        let mut listener_dids: Vec<String> = viewers
            .values()
            .filter_map(|entry| entry.did.clone())
            .collect();
        listener_dids.sort();
        listener_dids.dedup();
        ViewerStats {
            count: viewers.len(),
            listener_dids,
        }
    }

    fn subscribe(&self) -> broadcast::Receiver<ViewerStats> {
        self.events.subscribe()
    }

    fn register(&self, viewer_id: &str, did: Option<String>) -> ViewerStats {
        let mut viewers = self.inner.lock().expect("viewer tracker mutex poisoned");
        let previous_stats = Self::collect_stats(&viewers);
        let entry = viewers.entry(viewer_id.to_owned()).or_default();
        entry.connections += 1;
        entry.did = did;
        let stats = Self::collect_stats(&viewers);
        drop(viewers);

        if stats.count != previous_stats.count
            || stats.listener_dids != previous_stats.listener_dids
        {
            let _ = self.events.send(stats.clone());
        }

        stats
    }

    fn update_did(&self, viewer_id: &str, did: Option<String>) -> ViewerStats {
        let mut viewers = self.inner.lock().expect("viewer tracker mutex poisoned");
        let previous_stats = Self::collect_stats(&viewers);
        if let Some(entry) = viewers.get_mut(viewer_id) {
            entry.did = did;
        }
        let stats = Self::collect_stats(&viewers);
        drop(viewers);

        if stats.listener_dids != previous_stats.listener_dids {
            let _ = self.events.send(stats.clone());
        }

        stats
    }

    fn unregister(&self, viewer_id: &str) {
        let mut viewers = self.inner.lock().expect("viewer tracker mutex poisoned");
        let previous_stats = Self::collect_stats(&viewers);
        if let Some(entry) = viewers.get_mut(viewer_id) {
            entry.connections = entry.connections.saturating_sub(1);
            if entry.connections == 0 {
                viewers.remove(viewer_id);
            }
        }
        let stats = Self::collect_stats(&viewers);
        drop(viewers);

        if stats.count != previous_stats.count
            || stats.listener_dids != previous_stats.listener_dids
        {
            let _ = self.events.send(stats);
        }
    }
}

// ── Client message types (used by WebSocket handlers) ──

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub(crate) enum RadioClientMessage {
    ViewerHello {
        #[serde(alias = "viewerId")]
        viewer_id: String,
        #[serde(default, alias = "did")]
        did: Option<String>,
    },
    ViewerKeepalive {
        #[serde(alias = "viewerId")]
        viewer_id: String,
        #[serde(default, alias = "did")]
        did: Option<String>,
    },
}

// ── Common error types ──

#[derive(Serialize)]
pub(crate) struct ErrorResponse {
    pub(crate) error: String,
}

// ── Router ──

pub(crate) fn app(state: AppState, _app_url: &str) -> Router {
    let public_cors = CorsLayer::permissive();

    let xrpc_routes = Router::new()
        .route(
            "/xrpc/com.atproto.server.describeServer",
            get(pds::describe_server),
        )
        .route(
            "/xrpc/com.atproto.repo.describeRepo",
            get(pds::describe_repo),
        )
        .route("/xrpc/com.atproto.repo.getRecord", get(pds::get_record))
        .route("/xrpc/com.atproto.repo.listRecords", get(pds::list_records))
        .route(
            "/xrpc/com.atproto.sync.getRecord",
            get(pds::sync_get_record),
        )
        .route("/xrpc/com.atproto.sync.getRepo", get(pds::get_repo))
        .route("/xrpc/com.atproto.sync.listRepos", get(pds::list_repos))
        .route(
            "/xrpc/com.atproto.sync.subscribeRepos",
            get(pds::subscribe_repos),
        )
        .route("/xrpc/{*path}", any(pds::xrpc_not_found))
        .merge(QueueListRequest::into_router(xrpc::xrpc_queue_list))
        .merge(QueueModifyRequest::into_router(xrpc::xrpc_queue_modify))
        .merge(SongsListRequest::into_router(xrpc::xrpc_songs_list))
        .merge(SongsAddRequest::into_router(xrpc::xrpc_songs_add))
        .merge(SongsUploadRequest::into_router(xrpc::xrpc_songs_upload))
        .merge(SongsCoverRequest::into_router(xrpc::xrpc_songs_cover))
        .merge(SongsModifyRequest::into_router(xrpc::xrpc_songs_modify))
        .merge(AdminPermissionsRequest::into_router(
            xrpc::xrpc_admin_permissions_query,
        ))
        .merge(AdminModifyRequest::into_router(xrpc::xrpc_admin_modify))
        .merge(ControlRequest::into_router(xrpc::xrpc_control))
        .merge(AlbumsListRequest::into_router(xrpc::xrpc_albums_list))
        .merge(AlbumsModifyRequest::into_router(xrpc::xrpc_albums_modify))
        .merge(PlaylistsListRequest::into_router(xrpc::xrpc_playlists_list))
        .merge(PlaylistsModifyRequest::into_router(
            xrpc::xrpc_playlists_modify,
        ))
        .merge(ChatSendRequest::into_router(xrpc::xrpc_chat_send))
        .merge(ChatBansListRequest::into_router(xrpc::xrpc_chat_bans_list))
        .merge(ChatBansModifyRequest::into_router(
            xrpc::xrpc_chat_bans_modify,
        ))
        .merge(ChatMessagesModifyRequest::into_router(
            xrpc::xrpc_chat_messages_modify,
        ))
        .merge(SubsonicSearchRequest::into_router(
            xrpc::xrpc_subsonic_search,
        ))
        .merge(SubsonicImportRequest::into_router(
            xrpc::xrpc_subsonic_import,
        ));

    let public_routes = Router::new()
        .route("/client-metadata.json", get(well_known::client_metadata))
        .route("/.well-known/atproto-did", get(well_known::atproto_did))
        .route("/.well-known/did.json", get(well_known::did_json))
        .route(
            "/.well-known/oauth-protected-resource",
            get(well_known::oauth_protected_resource),
        )
        .route("/health", get(well_known::health))
        .route("/api/health", get(well_known::health))
        .route("/api/radio/state", get(radio::get_radio_state))
        .route("/api/radio/seek", get(radio::get_radio_seek))
        .route("/api/radio/rotation-info", get(radio::get_rotation_info))
        .route("/api/radio/ws", get(radio::radio_ws))
        .route("/api/radio/chat/ws", get(chat::chat_ws))
        .route(
            "/api/syndication/announce",
            post(pds::announce_station_to_relays),
        )
        .route("/api/songs", get(songs::get_songs))
        .route("/api/songs/{song_id}/audio", get(songs::song_audio))
        .route("/api/songs/{song_id}/cover", get(songs::song_cover))
        .route(
            "/api/songs/{song_id}/cover/thumbnail",
            get(songs::song_cover_thumbnail),
        )
        .route("/api/{*path}", any(well_known::api_not_found))
        .merge(xrpc_routes)
        .layer(public_cors);

    let frontend = ServeDir::new("static").fallback(ServeFile::new("static/index.html"));

    Router::new()
        .nest("/rest", crate::subsonic::router())
        .merge(public_routes)
        .fallback_service(frontend)
        .layer(DefaultBodyLimit::max(100 * 1024 * 1024))
        .with_state(state)
}

pub(crate) fn api_error(status: StatusCode, error: &str) -> (StatusCode, Json<ErrorResponse>) {
    (
        status,
        Json(ErrorResponse {
            error: error.into(),
        }),
    )
}

pub(crate) fn internal_api_error(error: Error) -> (StatusCode, Json<ErrorResponse>) {
    tracing::error!(?error, "api request failed");
    api_error(StatusCode::INTERNAL_SERVER_ERROR, "internal_server_error")
}
