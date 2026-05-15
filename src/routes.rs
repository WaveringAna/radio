use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::Error;
use axum::{
    Json, Router,
    body::Body,
    extract::{
        DefaultBodyLimit, FromRequestParts, Multipart, Path, Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, HeaderValue, Method, StatusCode, header, request::Parts},
    response::{IntoResponse, Redirect, Response},
    routing::{delete, get, post, put},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use jacquard::oauth::types::CallbackParams;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio::time::{Instant, MissedTickBehavior};
use tower_http::{
    cors::CorsLayer,
    services::{ServeDir, ServeFile},
};

use crate::{
    auth::{AppSession, AuthService},
    metadata::fetch_online_metadata,
    radio::{
        NewRadioAlbum, NewSongUpload, RadioControlAction, RadioEvent, RadioSeek, RadioService,
        SongMetadataUpdate, event_message,
    },
};

const VIEWER_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(60);
const VIEWER_KEEPALIVE_GRACE: Duration = Duration::from_secs(10);
const MAX_VIEWER_ID_LEN: usize = 128;

/// Shared application state for HTTP routes.
#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) auth: Arc<AuthService>,
    pub(crate) radio: Arc<RadioService>,
    viewers: ViewerTracker,
}

impl AppState {
    /// Creates route state from the auth and radio services.
    pub(crate) fn new(auth: AuthService, radio: RadioService) -> Self {
        Self {
            auth: Arc::new(auth),
            radio: Arc::new(radio),
            viewers: ViewerTracker::new(),
        }
    }
}

#[derive(Clone)]
struct ViewerTracker {
    inner: Arc<Mutex<HashMap<String, usize>>>,
    events: broadcast::Sender<usize>,
}

impl ViewerTracker {
    fn new() -> Self {
        let (events, _) = broadcast::channel(32);
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            events,
        }
    }

    fn count(&self) -> usize {
        self.inner
            .lock()
            .expect("viewer tracker mutex poisoned")
            .len()
    }

    fn subscribe(&self) -> broadcast::Receiver<usize> {
        self.events.subscribe()
    }

    fn register(&self, viewer_id: &str) -> usize {
        let mut viewers = self.inner.lock().expect("viewer tracker mutex poisoned");
        let previous_count = viewers.len();
        *viewers.entry(viewer_id.to_owned()).or_insert(0) += 1;
        let count = viewers.len();
        drop(viewers);

        if count != previous_count {
            let _ = self.events.send(count);
        }

        count
    }

    fn unregister(&self, viewer_id: &str) {
        let mut viewers = self.inner.lock().expect("viewer tracker mutex poisoned");
        let previous_count = viewers.len();
        if let Some(connections) = viewers.get_mut(viewer_id) {
            *connections = connections.saturating_sub(1);
            if *connections == 0 {
                viewers.remove(viewer_id);
            }
        }
        let count = viewers.len();
        drop(viewers);

        if count != previous_count {
            let _ = self.events.send(count);
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
enum RadioClientMessage {
    ViewerHello {
        #[serde(alias = "viewerId")]
        viewer_id: String,
    },
    ViewerKeepalive {
        #[serde(alias = "viewerId")]
        viewer_id: String,
    },
}

/// Builds the application router.
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
        .route("/api/health", get(health))
        .route("/api/oauth/start", get(start_oauth))
        .route("/api/oauth/callback", get(oauth_callback))
        .route("/api/session", get(get_session))
        .route("/api/admin/permissions", get(get_admin_permissions))
        .route("/api/admin/dids", post(add_admin_did))
        .route("/api/admin/dids/{did}", delete(remove_admin_did))
        .route("/api/radio/albums", get(get_albums).post(create_album))
        .route(
            "/api/radio/albums/from-metadata",
            post(create_album_from_metadata),
        )
        .route("/api/radio/albums/{album_id}", delete(delete_album))
        .route(
            "/api/radio/albums/{album_id}/enabled",
            put(set_album_enabled),
        )
        .route(
            "/api/radio/albums/{album_id}/songs",
            post(add_songs_to_album),
        )
        .route("/api/radio/state", get(get_radio_state))
        .route("/api/radio/seek", get(get_radio_seek))
        .route("/api/radio/ws", get(radio_ws))
        .route("/api/radio/queue", post(enqueue_song).delete(clear_queue))
        .route("/api/radio/queue/album", post(enqueue_album))
        .route("/api/radio/queue/reorder", post(reorder_queue))
        .route("/api/radio/queue/{queue_id}", delete(remove_queue_item))
        .route("/api/radio/control/{action}", post(control_radio))
        .route("/api/songs", get(get_songs).post(upload_song))
        .route("/api/songs/from-url", post(upload_song_from_url))
        .route("/api/songs/from-subsonic", post(import_from_subsonic))
        .route(
            "/api/songs/from-subsonic-share",
            post(import_from_subsonic_share),
        )
        .route("/api/subsonic/search", post(subsonic_search))
        .route("/api/songs/{song_id}", put(update_song).delete(delete_song))
        .route("/api/songs/{song_id}/audio", get(song_audio))
        .route(
            "/api/songs/{song_id}/cover",
            get(song_cover).put(upload_song_cover),
        )
        .route(
            "/api/songs/{song_id}/cover/thumbnail",
            get(song_cover_thumbnail),
        )
        .route("/api/logout", post(logout))
        .layer(cors);

    let frontend = ServeDir::new("static").fallback(ServeFile::new("static/index.html"));

    Router::new()
        .route("/client-metadata.json", get(client_metadata))
        .nest("/rest", crate::subsonic::router())
        .merge(api)
        .fallback_service(frontend)
        .layer(DefaultBodyLimit::max(100 * 1024 * 1024))
        .with_state(state)
}

/// Extracts the session token from `Authorization: Bearer <token>` header or the session cookie.
struct SessionToken(Option<String>);

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

async fn client_metadata(State(state): State<AppState>) -> Response {
    match state.auth.client_metadata_json() {
        Ok(json) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            json,
        )
            .into_response(),
        Err(error) => {
            tracing::error!(%error, "failed to serialize client metadata");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Serialize)]
struct HealthResponse {
    ok: bool,
}

#[derive(Deserialize)]
struct StartOauthQuery {
    input: Option<String>,
}

#[derive(Deserialize)]
struct OAuthCallbackQuery {
    code: Option<String>,
    state: Option<String>,
    iss: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionResponse {
    authenticated: bool,
    account_did: Option<String>,
    is_admin: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AdminPermissionsResponse {
    whitelisted_dids: Vec<String>,
    permissions: Vec<AdminPermission>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AdminPermission {
    key: &'static str,
    description: &'static str,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EnqueueSongRequest {
    song_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SongMetadataRequest {
    title: String,
    artist: String,
    album: Option<String>,
    genre: Option<String>,
    duration_seconds: Option<i64>,
}

#[derive(Deserialize)]
struct AdminDidRequest {
    did: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EnqueueAlbumRequest {
    song_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ControlRadioRequest {
    intent: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AlbumRequest {
    title: String,
    song_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MetadataAlbumRequest {
    album: String,
}

#[derive(Deserialize)]
struct AlbumEnabledRequest {
    enabled: bool,
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { ok: true })
}

async fn start_oauth(
    State(state): State<AppState>,
    Query(query): Query<StartOauthQuery>,
) -> Redirect {
    let Some(input) = query.input else {
        return Redirect::temporary(&state.auth.config().error_redirect_url("missing_input"));
    };

    match state.auth.start_sign_in(&input).await {
        Ok(url) => Redirect::temporary(&url),
        Err(error) => {
            tracing::error!(?error, "failed to start oauth flow");
            Redirect::temporary(&state.auth.config().error_redirect_url("oauth_start_failed"))
        }
    }
}

async fn oauth_callback(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<OAuthCallbackQuery>,
) -> impl IntoResponse {
    let Some(code) = query.code else {
        return Redirect::temporary(&state.auth.config().error_redirect_url("missing_code"))
            .into_response();
    };

    let params = CallbackParams {
        code: code.into(),
        state: query.state.map(Into::into),
        iss: query.iss.map(Into::into),
    };

    match state.auth.finish_sign_in(params).await {
        Ok(sign_in) => {
            let jar = jar.add(build_session_cookie(&state.auth, sign_in.session_token()));
            (
                jar,
                Redirect::to(
                    &state
                        .auth
                        .config()
                        .success_redirect_with_token(sign_in.session_token()),
                ),
            )
                .into_response()
        }
        Err(error) => {
            tracing::error!(?error, "oauth callback failed");
            Redirect::temporary(
                &state
                    .auth
                    .config()
                    .error_redirect_url("oauth_callback_failed"),
            )
            .into_response()
        }
    }
}

async fn get_session(
    State(state): State<AppState>,
    session_token: SessionToken,
) -> Result<Json<SessionResponse>, (StatusCode, Json<ErrorResponse>)> {
    let session = state
        .auth
        .session(session_token.0.as_deref())
        .await
        .map_err(internal_api_error)?;

    if let Some(session) = session {
        let is_admin = state
            .auth
            .is_admin_did(&session.account_did)
            .await
            .map_err(internal_api_error)?;

        return Ok(Json(SessionResponse {
            authenticated: true,
            account_did: Some(session.account_did),
            is_admin,
        }));
    }

    Ok(Json(SessionResponse {
        authenticated: false,
        account_did: None,
        is_admin: false,
    }))
}

async fn get_admin_permissions(
    State(state): State<AppState>,
    session_token: SessionToken,
) -> Result<Json<AdminPermissionsResponse>, (StatusCode, Json<ErrorResponse>)> {
    let session = state
        .auth
        .session(session_token.0.as_deref())
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

    Ok(Json(AdminPermissionsResponse {
        whitelisted_dids: state.auth.admin_dids().await.map_err(internal_api_error)?,
        permissions: admin_permissions(),
    }))
}

async fn add_admin_did(
    State(state): State<AppState>,
    session_token: SessionToken,
    Json(payload): Json<AdminDidRequest>,
) -> Result<Json<AdminPermissionsResponse>, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, session_token.0.as_deref()).await?;
    state
        .auth
        .add_admin_did(&payload.did)
        .await
        .map_err(internal_api_error)?;

    Ok(Json(AdminPermissionsResponse {
        whitelisted_dids: state.auth.admin_dids().await.map_err(internal_api_error)?,
        permissions: admin_permissions(),
    }))
}

async fn remove_admin_did(
    State(state): State<AppState>,
    session_token: SessionToken,
    Path(did): Path<String>,
) -> Result<Json<AdminPermissionsResponse>, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, session_token.0.as_deref()).await?;
    state
        .auth
        .remove_admin_did(&did)
        .await
        .map_err(internal_api_error)?;

    Ok(Json(AdminPermissionsResponse {
        whitelisted_dids: state.auth.admin_dids().await.map_err(internal_api_error)?,
        permissions: admin_permissions(),
    }))
}

fn admin_permissions() -> Vec<AdminPermission> {
    vec![
        AdminPermission {
            key: "songs:add",
            description: "add songs to the radio catalog",
        },
        AdminPermission {
            key: "radio:control",
            description: "control radio playback and queue state",
        },
    ]
}

async fn get_radio_state(
    State(state): State<AppState>,
) -> Result<Json<crate::radio::RadioSnapshot>, (StatusCode, Json<ErrorResponse>)> {
    state
        .radio
        .external_snapshot()
        .await
        .map(Json)
        .map_err(internal_api_error)
}

async fn get_radio_seek(
    State(state): State<AppState>,
) -> Result<Json<RadioSeek>, (StatusCode, Json<ErrorResponse>)> {
    state
        .radio
        .seek()
        .await
        .map(Json)
        .map_err(internal_api_error)
}

async fn get_albums(
    State(state): State<AppState>,
    session_token: SessionToken,
) -> Result<Json<Vec<crate::radio::RadioAlbum>>, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, session_token.0.as_deref()).await?;
    state
        .radio
        .albums()
        .await
        .map(Json)
        .map_err(internal_api_error)
}

async fn create_album(
    State(state): State<AppState>,
    session_token: SessionToken,
    Json(payload): Json<AlbumRequest>,
) -> Result<Json<crate::radio::RadioAlbum>, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, session_token.0.as_deref()).await?;
    state
        .radio
        .create_album(NewRadioAlbum {
            title: payload.title,
            song_ids: payload.song_ids,
        })
        .await
        .map(Json)
        .map_err(internal_api_error)
}

async fn create_album_from_metadata(
    State(state): State<AppState>,
    session_token: SessionToken,
    Json(payload): Json<MetadataAlbumRequest>,
) -> Result<Json<crate::radio::RadioAlbum>, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, session_token.0.as_deref()).await?;
    state
        .radio
        .create_album_from_metadata(payload.album.trim())
        .await
        .map(Json)
        .map_err(internal_api_error)
}

async fn delete_album(
    State(state): State<AppState>,
    session_token: SessionToken,
    Path(album_id): Path<String>,
) -> Result<Json<Vec<crate::radio::RadioAlbum>>, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, session_token.0.as_deref()).await?;
    state
        .radio
        .delete_album(&album_id)
        .await
        .map(Json)
        .map_err(internal_api_error)
}

async fn set_album_enabled(
    State(state): State<AppState>,
    session_token: SessionToken,
    Path(album_id): Path<String>,
    Json(payload): Json<AlbumEnabledRequest>,
) -> Result<Json<Vec<crate::radio::RadioAlbum>>, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, session_token.0.as_deref()).await?;
    state
        .radio
        .set_album_enabled(&album_id, payload.enabled)
        .await
        .map(Json)
        .map_err(internal_api_error)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddAlbumSongsRequest {
    song_ids: Vec<String>,
}

async fn add_songs_to_album(
    State(state): State<AppState>,
    session_token: SessionToken,
    Path(album_id): Path<String>,
    Json(payload): Json<AddAlbumSongsRequest>,
) -> Result<Json<crate::radio::RadioAlbum>, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, session_token.0.as_deref()).await?;
    state
        .radio
        .add_songs_to_album(&album_id, payload.song_ids)
        .await
        .map(Json)
        .map_err(internal_api_error)
}

async fn get_songs(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::radio::Song>>, (StatusCode, Json<ErrorResponse>)> {
    state
        .radio
        .songs()
        .await
        .map(Json)
        .map_err(internal_api_error)
}

async fn read_song_file_response(
    song_file: crate::radio::SongFile,
    not_found_error: &str,
    range_header: Option<&str>,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    let bytes = tokio::fs::read(&song_file.file_path)
        .await
        .map_err(|error| {
            tracing::error!(?error, path = %song_file.file_path, "failed to read song file");
            api_error(StatusCode::NOT_FOUND, not_found_error)
        })?;

    let content_type = song_file
        .mime_type
        .unwrap_or_else(|| "application/octet-stream".into());
    let total_len = bytes.len() as u64;

    if let Some(range_header) = range_header {
        if let Some((start, end)) = parse_byte_range(range_header, total_len) {
            let chunk = bytes[start as usize..=end as usize].to_vec();
            return Ok(Response::builder()
                .status(StatusCode::PARTIAL_CONTENT)
                .header(header::CONTENT_TYPE, &content_type)
                .header(header::ACCEPT_RANGES, "bytes")
                .header(
                    header::CONTENT_RANGE,
                    format!("bytes {start}-{end}/{total_len}"),
                )
                .header(header::CONTENT_LENGTH, chunk.len().to_string())
                .body(Body::from(chunk))
                .expect("partial file response should be valid"));
        }

        return Ok(Response::builder()
            .status(StatusCode::RANGE_NOT_SATISFIABLE)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CONTENT_RANGE, format!("bytes */{total_len}"))
            .body(Body::empty())
            .expect("range error response should be valid"));
    }

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, total_len.to_string())
        .body(Body::from(bytes))
        .expect("file response should be valid"))
}

fn parse_byte_range(range_header: &str, total_len: u64) -> Option<(u64, u64)> {
    if total_len == 0 {
        return None;
    }

    let bytes_spec = range_header.trim().strip_prefix("bytes=")?;
    if bytes_spec.contains(',') {
        return None;
    }

    let (start_text, end_text) = bytes_spec.split_once('-')?;
    if start_text.is_empty() {
        let suffix_len = end_text.parse::<u64>().ok()?;
        if suffix_len == 0 {
            return None;
        }
        let clamped = suffix_len.min(total_len);
        let start = total_len.saturating_sub(clamped);
        return Some((start, total_len - 1));
    }

    let start = start_text.parse::<u64>().ok()?;
    if start >= total_len {
        return None;
    }

    let end = if end_text.is_empty() {
        total_len - 1
    } else {
        let parsed_end = end_text.parse::<u64>().ok()?;
        if parsed_end < start {
            return None;
        }
        parsed_end.min(total_len - 1)
    };

    Some((start, end))
}

async fn song_audio(
    State(state): State<AppState>,
    Path(song_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    let Some(song_file) = state
        .radio
        .song_file(&song_id)
        .await
        .map_err(internal_api_error)?
    else {
        return Err(api_error(StatusCode::NOT_FOUND, "song_not_found"));
    };

    read_song_file_response(
        song_file,
        "audio_not_found",
        headers
            .get(header::RANGE)
            .and_then(|value| value.to_str().ok()),
    )
    .await
}

async fn song_cover(
    State(state): State<AppState>,
    Path(song_id): Path<String>,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    let Some(song_file) = state
        .radio
        .cover_file(&song_id)
        .await
        .map_err(internal_api_error)?
    else {
        return Err(api_error(StatusCode::NOT_FOUND, "cover_not_found"));
    };

    let mut response = read_song_file_response(song_file, "cover_not_found", None).await?;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    Ok(response)
}

async fn song_cover_thumbnail(
    State(state): State<AppState>,
    Path(song_id): Path<String>,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    let Some(thumb_path) = state
        .radio
        .cover_thumbnail(&song_id)
        .await
        .map_err(internal_api_error)?
    else {
        return Err(api_error(StatusCode::NOT_FOUND, "cover_not_found"));
    };

    let bytes = tokio::fs::read(&thumb_path).await.map_err(|error| {
        tracing::error!(?error, "failed to read thumbnail");
        api_error(StatusCode::INTERNAL_SERVER_ERROR, "thumbnail_read_error")
    })?;

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "image/jpeg")
        .header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")
        .header(header::CONTENT_LENGTH, bytes.len().to_string())
        .body(Body::from(bytes))
        .expect("thumbnail response should be valid"))
}

async fn upload_song_cover(
    State(state): State<AppState>,
    session_token: SessionToken,
    Path(song_id): Path<String>,
    mut multipart: Multipart,
) -> Result<Json<crate::radio::Song>, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, session_token.0.as_deref()).await?;
    let mut filename = None;
    let mut mime_type = None;
    let mut bytes = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "invalid_multipart"))?
    {
        if field.name() == Some("cover") {
            filename = field.file_name().map(ToOwned::to_owned);
            mime_type = field.content_type().map(ToString::to_string);
            bytes = Some(
                field
                    .bytes()
                    .await
                    .map_err(|_| api_error(StatusCode::BAD_REQUEST, "invalid_cover_file"))?
                    .to_vec(),
            );
        }
    }

    state
        .radio
        .set_song_cover(
            &song_id,
            filename,
            mime_type,
            bytes.ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "missing_cover_file"))?,
        )
        .await
        .map(Json)
        .map_err(internal_api_error)
}

async fn update_song(
    State(state): State<AppState>,
    session_token: SessionToken,
    Path(song_id): Path<String>,
    Json(input): Json<SongMetadataRequest>,
) -> Result<Json<crate::radio::Song>, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, session_token.0.as_deref()).await?;
    state
        .radio
        .update_song_metadata(
            &song_id,
            SongMetadataUpdate {
                title: input.title,
                artist: input.artist,
                album: input.album,
                genre: input.genre,
                duration_seconds: input.duration_seconds,
            },
        )
        .await
        .map(Json)
        .map_err(internal_api_error)
}

async fn delete_song(
    State(state): State<AppState>,
    session_token: SessionToken,
    Path(song_id): Path<String>,
) -> Result<Json<crate::radio::RadioSnapshot>, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, session_token.0.as_deref()).await?;
    state
        .radio
        .delete_song(&song_id)
        .await
        .map(Json)
        .map_err(internal_api_error)
}

async fn upload_song(
    State(state): State<AppState>,
    session_token: SessionToken,
    multipart: Multipart,
) -> Result<Json<crate::radio::Song>, (StatusCode, Json<ErrorResponse>)> {
    let session = admin_session(&state, session_token.0.as_deref()).await?;
    let mut upload = parse_song_upload(multipart).await?;

    let embedded = extract_embedded_metadata(&upload.bytes).await;
    if upload.genre.is_none() {
        upload.genre = embedded.genre;
    }

    let artist = upload.artist.clone();
    let album = upload.album.clone();
    let title = upload.title.clone();
    let embedded_cover = embedded.cover;

    // Fetch from MusicBrainz before saving so genre can be included in the INSERT
    let online = if upload.genre.is_none() || embedded_cover.is_none() {
        Some(fetch_online_metadata(&artist, album.as_deref(), &title).await)
    } else {
        None
    };

    if upload.genre.is_none() {
        upload.genre = online.as_ref().and_then(|ol| ol.genre.clone());
    }

    let mut song = state
        .radio
        .add_song(upload, &session.account_did)
        .await
        .map_err(internal_api_error)?;

    let cover = embedded_cover.or_else(|| online.and_then(|ol| ol.cover));

    if let Some((cover_bytes, cover_mime)) = cover {
        match state
            .radio
            .set_song_cover(&song.id, None, Some(cover_mime), cover_bytes)
            .await
        {
            Ok(updated) => song = updated,
            Err(error) => {
                tracing::warn!(%error, song_id = %song.id, "failed to set auto-fetched cover")
            }
        }
    }

    Ok(Json(song))
}

async fn enqueue_song(
    State(state): State<AppState>,
    session_token: SessionToken,
    Json(payload): Json<EnqueueSongRequest>,
) -> Result<Json<crate::radio::RadioSnapshot>, (StatusCode, Json<ErrorResponse>)> {
    let session = admin_session(&state, session_token.0.as_deref()).await?;

    state
        .radio
        .enqueue_song(&payload.song_id, &session.account_did)
        .await
        .map(Json)
        .map_err(internal_api_error)
}

async fn enqueue_album(
    State(state): State<AppState>,
    session_token: SessionToken,
    Json(payload): Json<EnqueueAlbumRequest>,
) -> Result<Json<crate::radio::RadioSnapshot>, (StatusCode, Json<ErrorResponse>)> {
    let session = admin_session(&state, session_token.0.as_deref()).await?;
    state
        .radio
        .enqueue_songs(&payload.song_ids, &session.account_did)
        .await
        .map(Json)
        .map_err(internal_api_error)
}

async fn remove_queue_item(
    State(state): State<AppState>,
    session_token: SessionToken,
    Path(queue_id): Path<String>,
) -> Result<Json<crate::radio::RadioSnapshot>, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, session_token.0.as_deref()).await?;

    state
        .radio
        .remove_queue_item(&queue_id)
        .await
        .map(Json)
        .map_err(internal_api_error)
}

async fn clear_queue(
    State(state): State<AppState>,
    session_token: SessionToken,
) -> Result<Json<crate::radio::RadioSnapshot>, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, session_token.0.as_deref()).await?;

    state
        .radio
        .clear_queue()
        .await
        .map(Json)
        .map_err(internal_api_error)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReorderQueueRequest {
    queue_ids: Vec<String>,
}

async fn reorder_queue(
    State(state): State<AppState>,
    session_token: SessionToken,
    Json(payload): Json<ReorderQueueRequest>,
) -> Result<Json<crate::radio::RadioSnapshot>, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, session_token.0.as_deref()).await?;

    state
        .radio
        .reorder_queue(&payload.queue_ids)
        .await
        .map(Json)
        .map_err(internal_api_error)
}

async fn control_radio(
    State(state): State<AppState>,
    session_token: SessionToken,
    Path(action): Path<String>,
    Json(payload): Json<ControlRadioRequest>,
) -> Result<Json<crate::radio::RadioSnapshot>, (StatusCode, Json<ErrorResponse>)> {
    let session = admin_session(&state, session_token.0.as_deref()).await?;
    if payload.intent != "explicit_admin_action" {
        return Err(api_error(StatusCode::BAD_REQUEST, "invalid_control_intent"));
    }

    let action = match action.as_str() {
        "play" => RadioControlAction::Play,
        "pause" => RadioControlAction::Pause,
        "stop" => RadioControlAction::Stop,
        "skip" => RadioControlAction::Skip,
        "previous" => RadioControlAction::Previous,
        _ => return Err(api_error(StatusCode::BAD_REQUEST, "unknown_radio_action")),
    };

    state
        .radio
        .control(action, &session.account_did)
        .await
        .map(Json)
        .map_err(internal_api_error)
}

async fn radio_ws(State(state): State<AppState>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(|socket| radio_socket(state, socket))
}

async fn radio_socket(state: AppState, mut socket: WebSocket) {
    let radio = state.radio.clone();
    match radio.external_snapshot().await {
        Ok(snapshot) => {
            let event = RadioEvent::SnapshotChanged { snapshot };
            if let Ok(message) = event_message(&event) {
                if socket.send(message).await.is_err() {
                    return;
                }
            }
        }
        Err(error) => tracing::error!(?error, "failed to send initial radio snapshot"),
    }
    let event = RadioEvent::ViewerCountChanged {
        viewer_count: state.viewers.count(),
    };
    if let Ok(message) = event_message(&event) {
        if socket.send(message).await.is_err() {
            return;
        }
    }

    let mut events = radio.subscribe();
    let mut viewer_counts = state.viewers.subscribe();
    let mut keepalive = tokio::time::interval_at(
        Instant::now() + VIEWER_KEEPALIVE_INTERVAL,
        VIEWER_KEEPALIVE_INTERVAL,
    );
    keepalive.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut registered_viewer_id: Option<String> = None;
    let mut last_viewer_seen: Option<Instant> = None;

    loop {
        tokio::select! {
            event = events.recv() => match event {
                Ok(event) => match event_message(&event) {
                    Ok(message) => {
                        if socket.send(message).await.is_err() {
                            break;
                        }
                    }
                    Err(error) => tracing::error!(?error, "failed to serialize radio event"),
                },
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
            count = viewer_counts.recv() => match count {
                Ok(viewer_count) => {
                    let event = RadioEvent::ViewerCountChanged { viewer_count };
                    match event_message(&event) {
                        Ok(message) => {
                            if socket.send(message).await.is_err() {
                                break;
                            }
                        }
                        Err(error) => tracing::error!(?error, "failed to serialize viewer count event"),
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
            _ = keepalive.tick() => {
                if registered_viewer_id.is_some()
                    && last_viewer_seen
                        .is_some_and(|seen| seen.elapsed() > VIEWER_KEEPALIVE_INTERVAL + VIEWER_KEEPALIVE_GRACE)
                {
                    break;
                }
                match event_message(&RadioEvent::ViewerKeepalive) {
                    Ok(message) => {
                        if socket.send(message).await.is_err() {
                            break;
                        }
                    }
                    Err(error) => tracing::error!(?error, "failed to serialize viewer keepalive"),
                }
            },
            message = socket.recv() => match message {
                Some(Ok(Message::Text(text))) => {
                    match serde_json::from_str::<RadioClientMessage>(&text) {
                        Ok(RadioClientMessage::ViewerHello { viewer_id })
                        | Ok(RadioClientMessage::ViewerKeepalive { viewer_id }) => {
                            if valid_viewer_id(&viewer_id) && registered_viewer_id.as_deref() != Some(viewer_id.as_str()) {
                                if let Some(previous_viewer_id) = registered_viewer_id.replace(viewer_id.clone()) {
                                    state.viewers.unregister(&previous_viewer_id);
                                }
                                let viewer_count = state.viewers.register(&viewer_id);
                                match event_message(&RadioEvent::ViewerCountChanged { viewer_count }) {
                                    Ok(message) => {
                                        if socket.send(message).await.is_err() {
                                            break;
                                        }
                                    }
                                    Err(error) => tracing::error!(?error, "failed to serialize viewer count event"),
                                }
                            }
                            if valid_viewer_id(&viewer_id) && registered_viewer_id.as_deref() == Some(viewer_id.as_str()) {
                                last_viewer_seen = Some(Instant::now());
                            }
                        }
                        Err(error) => tracing::debug!(?error, "ignored malformed radio websocket client message"),
                    }
                }
                Some(Ok(Message::Close(_))) => break,
                Some(Ok(_)) => {}
                Some(Err(error)) => {
                    tracing::debug!(?error, "radio websocket closed with error");
                    break;
                }
                None => break,
            },
        }
    }

    if let Some(viewer_id) = registered_viewer_id {
        state.viewers.unregister(&viewer_id);
    }
}

fn valid_viewer_id(viewer_id: &str) -> bool {
    !viewer_id.is_empty()
        && viewer_id.len() <= MAX_VIEWER_ID_LEN
        && viewer_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UrlSongRequest {
    url: String,
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    add_to_queue: Option<bool>,
}

fn is_ytdlp_url(url: &str) -> bool {
    url.contains("youtube.com/")
        || url.contains("youtu.be/")
        || url.contains("soundcloud.com/")
        || url.contains("bandcamp.com/")
        || url.contains("vimeo.com/")
}

struct YtdlpResult {
    bytes: Vec<u8>,
    title: String,
    artist: String,
    album: Option<String>,
    duration_seconds: Option<i64>,
    thumbnail_url: Option<String>,
}

async fn download_with_ytdlp(url: &str) -> anyhow::Result<YtdlpResult> {
    use tokio::process::Command;

    let meta_out = Command::new("yt-dlp")
        .args([
            "--no-update",
            "--extractor-args",
            "youtube:player_client=android_vr",
            "--dump-json",
            "--no-playlist",
            url,
        ])
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("yt-dlp not found (is it installed?): {e}"))?;

    if !meta_out.status.success() {
        let stderr = String::from_utf8_lossy(&meta_out.stderr);
        return Err(anyhow::anyhow!("yt-dlp metadata failed: {stderr}"));
    }

    let meta: serde_json::Value = serde_json::from_slice(&meta_out.stdout)
        .map_err(|_| anyhow::anyhow!("failed to parse yt-dlp output"))?;

    let title = meta["title"].as_str().unwrap_or("Unknown").to_owned();
    let artist = meta["artist"]
        .as_str()
        .or_else(|| meta["uploader"].as_str())
        .or_else(|| meta["channel"].as_str())
        .unwrap_or("Unknown")
        .to_owned();
    let album = meta["album"].as_str().map(ToOwned::to_owned);
    let duration_seconds = meta["duration"].as_f64().map(|d| d as i64);
    let thumbnail_url = meta["thumbnail"].as_str().map(ToOwned::to_owned);

    let tmp_path = format!("/tmp/radio-{}.mp3", uuid::Uuid::new_v4());

    let dl = Command::new("yt-dlp")
        .args([
            "--no-update",
            "--extractor-args",
            "youtube:player_client=android_vr",
            "-x",
            "--audio-format",
            "mp3",
            "--audio-quality",
            "0",
            "--no-playlist",
            "-o",
            &tmp_path,
            url,
        ])
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("yt-dlp download failed: {e}"))?;

    if !dl.status.success() {
        let stderr = String::from_utf8_lossy(&dl.stderr);
        return Err(anyhow::anyhow!("yt-dlp download failed: {stderr}"));
    }

    let bytes = tokio::fs::read(&tmp_path)
        .await
        .map_err(|_| anyhow::anyhow!("failed to read downloaded audio"))?;

    let _ = tokio::fs::remove_file(&tmp_path).await;

    Ok(YtdlpResult {
        bytes,
        title,
        artist,
        album,
        duration_seconds,
        thumbnail_url,
    })
}

async fn upload_song_from_url(
    State(state): State<AppState>,
    session_token: SessionToken,
    Json(payload): Json<UrlSongRequest>,
) -> Result<Json<crate::radio::Song>, (StatusCode, Json<ErrorResponse>)> {
    let session = admin_session(&state, session_token.0.as_deref()).await?;

    let url = payload.url.trim().to_owned();
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(api_error(StatusCode::BAD_REQUEST, "invalid_url"));
    }

    let title_override = payload
        .title
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    let artist_override = payload
        .artist
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());

    if is_ytdlp_url(&url) {
        let dl = download_with_ytdlp(&url).await.map_err(|error| {
            tracing::warn!(%error, %url, "yt-dlp download failed");
            api_error(StatusCode::BAD_GATEWAY, "ytdlp_failed")
        })?;

        let title = title_override.unwrap_or(dl.title);
        let artist = artist_override.unwrap_or(dl.artist);
        let album = payload.album.or(dl.album);

        let mut song = state
            .radio
            .add_song(
                crate::radio::NewSongUpload {
                    filename: None,
                    mime_type: Some("audio/mpeg".into()),
                    bytes: dl.bytes,
                    title,
                    artist,
                    album,
                    genre: None,
                    duration_seconds: dl.duration_seconds,
                    add_to_queue: payload.add_to_queue.unwrap_or(false),
                },
                &session.account_did,
            )
            .await
            .map_err(internal_api_error)?;

        // Attach thumbnail as cover art
        if let Some(thumb_url) = dl.thumbnail_url {
            if let Ok(thumb_resp) = reqwest::get(&thumb_url).await {
                if thumb_resp.status().is_success() {
                    let cover_mime = thumb_resp
                        .headers()
                        .get(reqwest::header::CONTENT_TYPE)
                        .and_then(|v| v.to_str().ok())
                        .map(ToOwned::to_owned);
                    if let Ok(cover_bytes) = thumb_resp.bytes().await {
                        if let Ok(updated) = state
                            .radio
                            .set_song_cover(&song.id, None, cover_mime, cover_bytes.to_vec())
                            .await
                        {
                            song = updated;
                        }
                    }
                }
            }
        }

        return Ok(Json(song));
    }

    // Plain URL download
    let title =
        title_override.ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "missing_title"))?;
    let artist =
        artist_override.ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "missing_artist"))?;

    let response = reqwest::get(&url).await.map_err(|error| {
        tracing::warn!(%error, %url, "failed to fetch url");
        api_error(StatusCode::BAD_REQUEST, "url_fetch_failed")
    })?;

    if !response.status().is_success() {
        return Err(api_error(StatusCode::BAD_REQUEST, "url_fetch_failed"));
    }

    let mime_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(';').next().unwrap_or(s).trim().to_owned());

    let filename = url
        .split('/')
        .last()
        .and_then(|s| s.split('?').next())
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);

    let bytes = response
        .bytes()
        .await
        .map_err(|error| {
            tracing::warn!(%error, %url, "failed to read url response body");
            api_error(StatusCode::BAD_REQUEST, "url_fetch_failed")
        })?
        .to_vec();

    let embedded = extract_embedded_metadata(&bytes).await;
    let embedded_cover = embedded.cover;
    let mut genre = embedded.genre;

    let online = if genre.is_none() || embedded_cover.is_none() {
        Some(fetch_online_metadata(&artist, payload.album.as_deref(), &title).await)
    } else {
        None
    };

    if genre.is_none() {
        genre = online.as_ref().and_then(|ol| ol.genre.clone());
    }

    let mut song = state
        .radio
        .add_song(
            crate::radio::NewSongUpload {
                filename,
                mime_type,
                bytes,
                title: title.clone(),
                artist: artist.clone(),
                album: payload.album.clone(),
                genre,
                duration_seconds: None,
                add_to_queue: payload.add_to_queue.unwrap_or(false),
            },
            &session.account_did,
        )
        .await
        .map_err(internal_api_error)?;

    let cover = embedded_cover.or_else(|| online.and_then(|ol| ol.cover));

    if let Some((cover_bytes, cover_mime)) = cover {
        match state
            .radio
            .set_song_cover(&song.id, None, Some(cover_mime), cover_bytes)
            .await
        {
            Ok(updated) => song = updated,
            Err(error) => {
                tracing::warn!(%error, song_id = %song.id, "failed to set auto-fetched cover")
            }
        }
    }

    Ok(Json(song))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubsonicCreds {
    server_url: String,
    username: String,
    password: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubsonicSearchRequest {
    #[serde(flatten)]
    creds: SubsonicCreds,
    query: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SubsonicSongResult {
    id: String,
    title: String,
    artist: String,
    album: Option<String>,
    duration_seconds: Option<u64>,
    cover_art_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubsonicImportRequest {
    #[serde(flatten)]
    creds: SubsonicCreds,
    song_id: String,
    cover_art_id: Option<String>,
    add_to_queue: Option<bool>,
}

fn subsonic_auth_params(creds: &SubsonicCreds) -> [(&'static str, String); 5] {
    let hex_pass = creds
        .password
        .bytes()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    [
        ("u", creds.username.clone()),
        ("p", format!("enc:{hex_pass}")),
        ("v", "1.16.1".into()),
        ("c", "radio".into()),
        ("f", "json".into()),
    ]
}

async fn subsonic_search(
    State(state): State<AppState>,
    session_token: SessionToken,
    Json(payload): Json<SubsonicSearchRequest>,
) -> Result<Json<Vec<SubsonicSongResult>>, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, session_token.0.as_deref()).await?;

    let base = payload.creds.server_url.trim_end_matches('/').to_owned();
    let auth = subsonic_auth_params(&payload.creds);

    let response = reqwest::Client::new()
        .get(format!("{base}/rest/search3.view"))
        .query(&auth)
        .query(&[
            ("query", payload.query.as_str()),
            ("songCount", "50"),
            ("artistCount", "0"),
            ("albumCount", "0"),
        ])
        .send()
        .await
        .map_err(|error| {
            tracing::warn!(%error, "subsonic search request failed");
            api_error(StatusCode::BAD_GATEWAY, "subsonic_unreachable")
        })?;

    let json: serde_json::Value = response
        .json()
        .await
        .map_err(|_| api_error(StatusCode::BAD_GATEWAY, "subsonic_parse_error"))?;

    let empty = vec![];
    let songs = json["subsonic-response"]["searchResult3"]["song"]
        .as_array()
        .unwrap_or(&empty);

    let results = songs
        .iter()
        .map(|s| SubsonicSongResult {
            id: s["id"].as_str().unwrap_or_default().to_owned(),
            title: s["title"].as_str().unwrap_or("Unknown").to_owned(),
            artist: s["artist"].as_str().unwrap_or("Unknown").to_owned(),
            album: s["album"].as_str().map(ToOwned::to_owned),
            duration_seconds: s["duration"].as_u64(),
            cover_art_id: s["coverArt"].as_str().map(ToOwned::to_owned),
        })
        .collect();

    Ok(Json(results))
}

async fn import_from_subsonic(
    State(state): State<AppState>,
    session_token: SessionToken,
    Json(payload): Json<SubsonicImportRequest>,
) -> Result<Json<crate::radio::Song>, (StatusCode, Json<ErrorResponse>)> {
    let session = admin_session(&state, session_token.0.as_deref()).await?;

    let base = payload.creds.server_url.trim_end_matches('/').to_owned();
    let auth = subsonic_auth_params(&payload.creds);
    let client = reqwest::Client::new();

    // Fetch song metadata
    let meta: serde_json::Value = client
        .get(format!("{base}/rest/getSong.view"))
        .query(&auth)
        .query(&[("id", payload.song_id.as_str())])
        .send()
        .await
        .map_err(|_| api_error(StatusCode::BAD_GATEWAY, "subsonic_unreachable"))?
        .json()
        .await
        .map_err(|_| api_error(StatusCode::BAD_GATEWAY, "subsonic_parse_error"))?;

    let song_meta = &meta["subsonic-response"]["song"];
    let title = song_meta["title"].as_str().unwrap_or("Unknown").to_owned();
    let artist = song_meta["artist"].as_str().unwrap_or("Unknown").to_owned();
    let album = song_meta["album"].as_str().map(ToOwned::to_owned);
    let duration_seconds = song_meta["duration"].as_i64();
    let cover_art_id = payload
        .cover_art_id
        .as_deref()
        .or_else(|| song_meta["coverArt"].as_str())
        .map(ToOwned::to_owned);

    // Stream audio
    let stream_resp = client
        .get(format!("{base}/rest/stream.view"))
        .query(&auth)
        .query(&[("id", payload.song_id.as_str())])
        .send()
        .await
        .map_err(|_| api_error(StatusCode::BAD_GATEWAY, "subsonic_unreachable"))?;

    if !stream_resp.status().is_success() {
        return Err(api_error(StatusCode::BAD_GATEWAY, "subsonic_stream_failed"));
    }

    let mime_type = stream_resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(';').next().unwrap_or(s).trim().to_owned());

    let audio_bytes = stream_resp
        .bytes()
        .await
        .map_err(|_| api_error(StatusCode::BAD_GATEWAY, "subsonic_stream_failed"))?
        .to_vec();

    let mut song = state
        .radio
        .add_song(
            crate::radio::NewSongUpload {
                filename: None,
                mime_type,
                bytes: audio_bytes,
                title,
                artist,
                album,
                genre: None,
                duration_seconds,
                add_to_queue: payload.add_to_queue.unwrap_or(false),
            },
            &session.account_did,
        )
        .await
        .map_err(internal_api_error)?;

    // Fetch and attach cover art
    if let Some(cover_id) = cover_art_id {
        if let Ok(cover_resp) = client
            .get(format!("{base}/rest/getCoverArt.view"))
            .query(&auth)
            .query(&[("id", cover_id.as_str())])
            .send()
            .await
        {
            if cover_resp.status().is_success() {
                let cover_mime = cover_resp
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .map(ToOwned::to_owned);
                if let Ok(cover_bytes) = cover_resp.bytes().await {
                    if let Ok(updated) = state
                        .radio
                        .set_song_cover(&song.id, None, cover_mime, cover_bytes.to_vec())
                        .await
                    {
                        song = updated;
                    }
                }
            }
        }
    }

    Ok(Json(song))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubsonicShareImportRequest {
    share_url: String,
    add_to_queue: Option<bool>,
}

async fn import_from_subsonic_share(
    State(state): State<AppState>,
    session_token: SessionToken,
    Json(payload): Json<SubsonicShareImportRequest>,
) -> Result<Json<crate::radio::Song>, (StatusCode, Json<ErrorResponse>)> {
    let session = admin_session(&state, session_token.0.as_deref()).await?;

    let (base, share_id) = parse_share_url(&payload.share_url)
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "share_url_invalid"))?;

    let client = reqwest::Client::new();

    let html = client
        .get(format!("{base}/share/{share_id}"))
        .send()
        .await
        .map_err(|_| api_error(StatusCode::BAD_GATEWAY, "subsonic_unreachable"))?
        .text()
        .await
        .map_err(|_| api_error(StatusCode::BAD_GATEWAY, "subsonic_parse_error"))?;

    let share_info_raw = extract_share_info(&html)
        .ok_or_else(|| api_error(StatusCode::BAD_GATEWAY, "share_info_missing"))?;
    let share_info: serde_json::Value = serde_json::from_str(&share_info_raw)
        .map_err(|_| api_error(StatusCode::BAD_GATEWAY, "share_info_invalid"))?;

    let track = share_info["tracks"]
        .as_array()
        .and_then(|a| a.first())
        .ok_or_else(|| api_error(StatusCode::BAD_GATEWAY, "share_has_no_tracks"))?;

    let track_id = track["id"]
        .as_str()
        .ok_or_else(|| api_error(StatusCode::BAD_GATEWAY, "share_track_id_missing"))?
        .to_owned();
    let title = track["title"].as_str().unwrap_or("Unknown").to_owned();
    let artist = track["artist"].as_str().unwrap_or("Unknown").to_owned();
    let album = track["album"].as_str().map(ToOwned::to_owned);
    let duration_seconds = track["duration"]
        .as_f64()
        .map(|d| d.round() as i64)
        .or_else(|| track["duration"].as_i64());

    let stream_resp = client
        .get(format!("{base}/share/s/{track_id}"))
        .send()
        .await
        .map_err(|_| api_error(StatusCode::BAD_GATEWAY, "subsonic_unreachable"))?;

    if !stream_resp.status().is_success() {
        return Err(api_error(StatusCode::BAD_GATEWAY, "subsonic_stream_failed"));
    }

    let mime_type = stream_resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(';').next().unwrap_or(s).trim().to_owned());

    let audio_bytes = stream_resp
        .bytes()
        .await
        .map_err(|_| api_error(StatusCode::BAD_GATEWAY, "subsonic_stream_failed"))?
        .to_vec();

    let mut song = state
        .radio
        .add_song(
            crate::radio::NewSongUpload {
                filename: None,
                mime_type,
                bytes: audio_bytes,
                title,
                artist,
                album,
                genre: None,
                duration_seconds,
                add_to_queue: payload.add_to_queue.unwrap_or(false),
            },
            &session.account_did,
        )
        .await
        .map_err(internal_api_error)?;

    if let Ok(cover_resp) = client
        .get(format!("{base}/share/img/{track_id}"))
        .query(&[("size", "600")])
        .send()
        .await
    {
        if cover_resp.status().is_success() {
            let cover_mime = cover_resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(ToOwned::to_owned);
            if let Ok(cover_bytes) = cover_resp.bytes().await {
                if !cover_bytes.is_empty() {
                    if let Ok(updated) = state
                        .radio
                        .set_song_cover(&song.id, None, cover_mime, cover_bytes.to_vec())
                        .await
                    {
                        song = updated;
                    }
                }
            }
        }
    }

    Ok(Json(song))
}

fn parse_share_url(input: &str) -> Option<(String, String)> {
    let trimmed = input.trim();
    let without_fragment = trimmed.split('#').next().unwrap_or(trimmed);
    let without_query = without_fragment.split('?').next().unwrap_or(without_fragment);
    let url = reqwest::Url::parse(without_query).ok()?;
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let host = url.host_str()?;
    let port = url
        .port()
        .map(|p| format!(":{p}"))
        .unwrap_or_default();
    let base = format!("{scheme}://{host}{port}");

    let segments: Vec<&str> = url
        .path_segments()?
        .filter(|s| !s.is_empty())
        .collect();
    let share_idx = segments.iter().rposition(|s| *s == "share")?;
    let id = segments.get(share_idx + 1)?.to_string();
    if id.is_empty() {
        return None;
    }
    Some((base, id))
}

fn extract_share_info(html: &str) -> Option<String> {
    let needle = "window.SHARE_INFO";
    let start = html.find(needle)? + needle.len();
    let after = &html[start..];
    let eq = after.find('=')?;
    let after_eq = after[eq + 1..].trim_start();
    if !after_eq.starts_with('"') {
        return None;
    }
    let body = &after_eq[1..];
    let line_end = body.find('\n').unwrap_or(body.len());
    let line = body[..line_end].trim_end();
    let line = line.trim_end_matches(';');
    let line = line.trim_end();
    let inner = line.strip_suffix('"')?;
    Some(html_decode(inner))
}

fn html_decode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        if let Some(semi) = tail.find(';') {
            let entity = &tail[1..semi];
            let replacement = match entity {
                "quot" => Some('"'),
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "apos" => Some('\''),
                _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                    u32::from_str_radix(&entity[2..], 16)
                        .ok()
                        .and_then(char::from_u32)
                }
                _ if entity.starts_with('#') => entity[1..]
                    .parse::<u32>()
                    .ok()
                    .and_then(char::from_u32),
                _ => None,
            };
            if let Some(c) = replacement {
                out.push(c);
                rest = &tail[semi + 1..];
                continue;
            }
        }
        out.push('&');
        rest = &tail[1..];
    }
    out.push_str(rest);
    out
}

struct EmbeddedMetadata {
    cover: Option<(Vec<u8>, String)>,
    genre: Option<String>,
}

async fn extract_embedded_metadata(bytes: &[u8]) -> EmbeddedMetadata {
    let bytes = bytes.to_vec();
    tokio::task::spawn_blocking(move || -> EmbeddedMetadata {
        use lofty::prelude::*;
        use lofty::probe::Probe;
        use std::io::{BufReader, Cursor};
        let cursor = BufReader::new(Cursor::new(bytes));
        let Ok(probe) = Probe::new(cursor).guess_file_type() else {
            return EmbeddedMetadata {
                cover: None,
                genre: None,
            };
        };
        let Ok(tagged) = probe.read() else {
            return EmbeddedMetadata {
                cover: None,
                genre: None,
            };
        };
        let tag = tagged.primary_tag();
        let cover = tag.and_then(|t| t.pictures().first()).map(|pic| {
            let mime = pic
                .mime_type()
                .map(|m| m.to_string())
                .unwrap_or_else(|| "image/jpeg".to_owned());
            (pic.data().to_vec(), mime)
        });
        let genre = tag.and_then(|t| {
            t.genre()
                .map(|g| g.trim().to_owned())
                .filter(|g| !g.is_empty())
        });
        EmbeddedMetadata { cover, genre }
    })
    .await
    .unwrap_or(EmbeddedMetadata {
        cover: None,
        genre: None,
    })
}

async fn parse_song_upload(
    mut multipart: Multipart,
) -> Result<NewSongUpload, (StatusCode, Json<ErrorResponse>)> {
    let mut filename = None;
    let mut mime_type = None;
    let mut bytes = None;
    let mut title = None;
    let mut artist = None;
    let mut album = None;
    let mut genre = None;
    let mut duration_seconds = None;
    let mut add_to_queue = false;

    while let Some(field) = multipart.next_field().await.map_err(|error| {
        tracing::error!(?error, "failed to read multipart field");
        api_error(StatusCode::BAD_REQUEST, "invalid_multipart")
    })? {
        let name = field.name().unwrap_or_default().to_owned();
        match name.as_str() {
            "file" => {
                filename = field.file_name().map(ToOwned::to_owned);
                mime_type = field.content_type().map(ToString::to_string);
                bytes = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "invalid_audio_file"))?
                        .to_vec(),
                );
            }
            "title" => title = Some(field.text().await.unwrap_or_default()),
            "artist" => artist = Some(field.text().await.unwrap_or_default()),
            "album" => album = Some(field.text().await.unwrap_or_default()),
            "genre" => genre = Some(field.text().await.unwrap_or_default()),
            "durationSeconds" => {
                duration_seconds = field.text().await.unwrap_or_default().parse::<i64>().ok()
            }
            "addToQueue" => add_to_queue = field.text().await.unwrap_or_default() == "true",
            _ => {}
        }
    }

    let title = title
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "missing_title"))?;
    let artist = artist
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "missing_artist"))?;
    let bytes = bytes.ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "missing_audio_file"))?;
    let genre = genre.map(|g| g.trim().to_owned()).filter(|g| !g.is_empty());

    Ok(NewSongUpload {
        filename,
        mime_type,
        bytes,
        title,
        artist,
        album,
        genre,
        duration_seconds,
        add_to_queue,
    })
}

async fn admin_session(
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

async fn logout(
    State(state): State<AppState>,
    session_token: SessionToken,
    jar: CookieJar,
) -> Result<(CookieJar, StatusCode), (StatusCode, Json<ErrorResponse>)> {
    state
        .auth
        .sign_out(session_token.0.as_deref())
        .await
        .map_err(internal_api_error)?;

    Ok((
        jar.add(clear_session_cookie(&state.auth)),
        StatusCode::NO_CONTENT,
    ))
}

fn build_session_cookie(auth: &AuthService, session_token: &str) -> Cookie<'static> {
    let mut cookie = Cookie::new(
        auth.config().session_cookie_name.clone(),
        session_token.to_owned(),
    );
    cookie.set_path("/");
    cookie.set_http_only(true);
    cookie.set_same_site(SameSite::None);
    cookie.set_secure(true);
    cookie.set_max_age(time::Duration::days(auth.config().session_ttl_days));
    cookie
}

fn clear_session_cookie(auth: &AuthService) -> Cookie<'static> {
    let mut cookie = Cookie::new(auth.config().session_cookie_name.clone(), String::new());
    cookie.set_path("/");
    cookie.make_removal();
    cookie
}

fn api_error(status: StatusCode, error: &str) -> (StatusCode, Json<ErrorResponse>) {
    (
        status,
        Json(ErrorResponse {
            error: error.into(),
        }),
    )
}

fn internal_api_error(error: Error) -> (StatusCode, Json<ErrorResponse>) {
    tracing::error!(?error, "api request failed");
    api_error(StatusCode::INTERNAL_SERVER_ERROR, "internal_server_error")
}

#[cfg(test)]
mod tests {
    use super::parse_byte_range;

    #[test]
    fn parses_open_ended_byte_range() {
        assert_eq!(parse_byte_range("bytes=10-", 100), Some((10, 99)));
    }

    #[test]
    fn parses_suffix_byte_range() {
        assert_eq!(parse_byte_range("bytes=-25", 100), Some((75, 99)));
    }

    #[test]
    fn rejects_invalid_ranges() {
        assert_eq!(parse_byte_range("bytes=101-120", 100), None);
        assert_eq!(parse_byte_range("bytes=30-20", 100), None);
        assert_eq!(parse_byte_range("bytes=0-1,5-6", 100), None);
    }
}
