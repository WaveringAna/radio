mod admin;
mod auth;
mod chat;
mod helpers;
mod queue;
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
    extract::{DefaultBodyLimit, FromRequestParts},
    http::{HeaderValue, Method, StatusCode, header, request::Parts},
    routing::{delete, get, post, put},
};
use axum_extra::extract::cookie::CookieJar;
use jacquard::{
    identity::{JacquardResolver, resolver::ResolverOptions},
    types::did::Did,
};
use jacquard_axum::{
    IntoRouter,
    service_auth::ServiceAuth,
};
use radio_lexicons::pet_nkp::radio::{
    queue::list::ListRequest as QueueListRequest,
    queue::modify::ModifyRequest as QueueModifyRequest,
    songs::add::AddRequest as SongsAddRequest,
    songs::list::ListRequest as SongsListRequest,
    songs::upload::UploadRequest as SongsUploadRequest,
};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tower_http::{
    cors::CorsLayer,
    services::{ServeDir, ServeFile},
};

use crate::{
    auth::{AppSession, AuthService},
    chat::ChatService,
    radio::RadioService,
};

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
    ) -> Self {
        Self {
            auth: Arc::new(auth),
            radio: Arc::new(radio),
            chat: Arc::new(chat),
            service_did,
            service_endpoint,
            service_ids,
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

// ── Session token extraction ──

/// Extracts the session token from `Authorization: Bearer <token>` header or the session cookie.
pub(crate) struct SessionToken(pub(crate) Option<String>);

impl FromRequestParts<AppState> for SessionToken {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let bearer = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
            .map(|s| s.to_owned());

        if bearer.is_some() {
            return Ok(SessionToken(bearer));
        }

        let jar = CookieJar::from_request_parts(parts, state).await.unwrap();
        let token = jar
            .get(&state.auth.config().session_cookie_name)
            .map(|c| c.value().to_owned());

        Ok(SessionToken(token))
    }
}

// ── Common error types ──

#[derive(Serialize)]
pub(crate) struct ErrorResponse {
    pub(crate) error: String,
}

// ── Router ──

pub(crate) fn app(state: AppState, app_url: &str) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(
            app_url
                .parse::<HeaderValue>()
                .expect("invalid APP_URL for CORS origin"),
        )
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION])
        .allow_credentials(true);

    let api = Router::new()
        .route("/api/health", get(well_known::health))
        .route("/api/oauth/start", get(auth::start_oauth))
        .route("/api/oauth/callback", get(auth::oauth_callback))
        .route("/api/session", get(auth::get_session))
        .route("/api/admin/permissions", get(admin::get_admin_permissions))
        .route("/api/admin/dids", post(admin::add_admin_did))
        .route("/api/admin/dids/{did}", delete(admin::remove_admin_did))
        .route("/api/radio/albums", get(songs::get_albums).post(songs::create_album))
        .route(
            "/api/radio/albums/from-metadata",
            post(songs::create_album_from_metadata),
        )
        .route("/api/radio/albums/{album_id}", delete(songs::delete_album))
        .route(
            "/api/radio/albums/{album_id}/enabled",
            put(songs::set_album_enabled),
        )
        .route(
            "/api/radio/albums/{album_id}/songs",
            post(songs::add_songs_to_album),
        )
        .route("/api/radio/state", get(radio::get_radio_state))
        .route("/api/radio/seek", get(radio::get_radio_seek))
        .route("/api/radio/ws", get(radio::radio_ws))
        .route("/api/radio/chat/ws", get(chat::chat_ws))
        .route(
            "/api/radio/chat/messages/{message_id}",
            delete(chat::delete_chat_message),
        )
        .route(
            "/api/radio/chat/bans",
            get(chat::list_chat_bans).post(chat::create_chat_ban),
        )
        .route("/api/radio/chat/bans/{did}", delete(chat::remove_chat_ban))
        .route("/api/radio/queue", post(queue::enqueue_song).delete(queue::clear_queue))
        .route("/api/radio/queue/album", post(queue::enqueue_album))
        .route("/api/radio/queue/reorder", post(queue::reorder_queue))
        .route("/api/radio/queue/{queue_id}", delete(queue::remove_queue_item))
        .route("/api/radio/control/{action}", post(radio::control_radio))
        .route("/api/songs", get(songs::get_songs).post(songs::upload_song))
        .route("/api/songs/from-url", post(upload::upload_song_from_url))
        .route("/api/songs/from-subsonic", post(subsonic_import::import_from_subsonic))
        .route(
            "/api/songs/from-subsonic-share",
            post(subsonic_import::import_from_subsonic_share),
        )
        .route("/api/subsonic/search", post(subsonic_import::subsonic_search))
        .route("/api/songs/{song_id}", put(songs::update_song).delete(songs::delete_song))
        .route("/api/songs/{song_id}/audio", get(songs::song_audio))
        .route(
            "/api/songs/{song_id}/cover",
            get(songs::song_cover).put(songs::upload_song_cover),
        )
        .route(
            "/api/songs/{song_id}/cover/thumbnail",
            get(songs::song_cover_thumbnail),
        )
        .route("/api/logout", post(auth::logout))
        .merge(QueueListRequest::into_router(xrpc::xrpc_queue_list))
        .merge(QueueModifyRequest::into_router(xrpc::xrpc_queue_modify))
        .merge(SongsListRequest::into_router(xrpc::xrpc_songs_list))
        .merge(SongsAddRequest::into_router(xrpc::xrpc_songs_add))
        .merge(SongsUploadRequest::into_router(xrpc::xrpc_songs_upload))
        .layer(cors);

    let frontend = ServeDir::new("static").fallback(ServeFile::new("static/index.html"));

    Router::new()
        .route("/client-metadata.json", get(well_known::client_metadata))
        .route("/.well-known/atproto-did", get(well_known::atproto_did))
        .route("/.well-known/did.json", get(well_known::did_json))
        .route(
            "/.well-known/oauth-protected-resource",
            get(well_known::oauth_protected_resource),
        )
        .nest("/rest", crate::subsonic::router())
        .merge(api)
        .fallback_service(frontend)
        .layer(DefaultBodyLimit::max(100 * 1024 * 1024))
        .with_state(state)
}

// ── Shared helpers ──

pub(crate) async fn admin_session(
    state: &AppState,
    token: Option<&str>,
) -> Result<AppSession, (StatusCode, Json<ErrorResponse>)> {
    let session = state
        .auth
        .session(token)
        .await
        .map_err(internal_api_error)?
        .ok_or_else(|| api_error(StatusCode::UNAUTHORIZED, "unauthenticated"))?;

    if !state
        .auth
        .is_admin_did(&session.account_did)
        .await
        .map_err(internal_api_error)?
    {
        return Err(api_error(StatusCode::FORBIDDEN, "admin_required"));
    }

    Ok(session)
}

pub(crate) fn api_error(
    status: StatusCode,
    error: &str,
) -> (StatusCode, Json<ErrorResponse>) {
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
