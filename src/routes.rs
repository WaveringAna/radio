use std::sync::Arc;

use anyhow::Error;
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Multipart, Path, Query, State, WebSocketUpgrade, ws::WebSocket},
    http::{HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
    routing::{delete, get, post, put},
};
use tower_http::cors::CorsLayer;
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use jacquard::oauth::types::CallbackParams;
use serde::{Deserialize, Serialize};

use crate::{
    auth::{AppSession, AuthService},
    radio::{NewRadioAlbum, NewSongUpload, RadioControlAction, RadioService, event_message},
};

/// Shared application state for HTTP routes.
#[derive(Clone)]
pub(crate) struct AppState {
    auth: Arc<AuthService>,
    radio: Arc<RadioService>,
}

impl AppState {
    /// Creates route state from the auth and radio services.
    pub(crate) fn new(auth: AuthService, radio: RadioService) -> Self {
        Self {
            auth: Arc::new(auth),
            radio: Arc::new(radio),
        }
    }
}

/// Builds the application router.
pub(crate) fn app(state: AppState, app_url: &str) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(app_url.parse::<HeaderValue>().expect("invalid APP_URL for CORS origin"))
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE, Method::OPTIONS])
        .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION])
        .allow_credentials(true);

    Router::new()
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
        .route("/api/radio/state", get(get_radio_state))
        .route("/api/radio/ws", get(radio_ws))
        .route("/api/radio/queue", post(enqueue_song))
        .route("/api/radio/queue/{queue_id}", delete(remove_queue_item))
        .route("/api/radio/control/{action}", post(control_radio))
        .route("/api/songs", get(get_songs).post(upload_song))
        .route("/api/songs/{song_id}", delete(delete_song))
        .route("/api/songs/{song_id}/audio", get(song_audio))
        .route(
            "/api/songs/{song_id}/cover",
            get(song_cover).put(upload_song_cover),
        )
        .route("/api/logout", post(logout))
        .layer(DefaultBodyLimit::max(100 * 1024 * 1024))
        .layer(cors)
        .with_state(state)
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
struct AdminDidRequest {
    did: String,
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
                Redirect::to(&state.auth.config().success_redirect_url()),
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
    jar: CookieJar,
) -> Result<Json<SessionResponse>, (StatusCode, Json<ErrorResponse>)> {
    let session = state
        .auth
        .session(current_session_token(&state.auth, &jar).as_deref())
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
    jar: CookieJar,
) -> Result<Json<AdminPermissionsResponse>, (StatusCode, Json<ErrorResponse>)> {
    let session = state
        .auth
        .session(current_session_token(&state.auth, &jar).as_deref())
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
    jar: CookieJar,
    Json(payload): Json<AdminDidRequest>,
) -> Result<Json<AdminPermissionsResponse>, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, &jar).await?;
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
    jar: CookieJar,
    Path(did): Path<String>,
) -> Result<Json<AdminPermissionsResponse>, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, &jar).await?;
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
        .snapshot()
        .await
        .map(Json)
        .map_err(internal_api_error)
}

async fn get_albums(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Json<Vec<crate::radio::RadioAlbum>>, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, &jar).await?;
    state
        .radio
        .albums()
        .await
        .map(Json)
        .map_err(internal_api_error)
}

async fn create_album(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(payload): Json<AlbumRequest>,
) -> Result<Json<crate::radio::RadioAlbum>, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, &jar).await?;
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
    jar: CookieJar,
    Json(payload): Json<MetadataAlbumRequest>,
) -> Result<Json<crate::radio::RadioAlbum>, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, &jar).await?;
    state
        .radio
        .create_album_from_metadata(payload.album.trim())
        .await
        .map(Json)
        .map_err(internal_api_error)
}

async fn delete_album(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(album_id): Path<String>,
) -> Result<Json<Vec<crate::radio::RadioAlbum>>, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, &jar).await?;
    state
        .radio
        .delete_album(&album_id)
        .await
        .map(Json)
        .map_err(internal_api_error)
}

async fn set_album_enabled(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(album_id): Path<String>,
    Json(payload): Json<AlbumEnabledRequest>,
) -> Result<Json<Vec<crate::radio::RadioAlbum>>, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, &jar).await?;
    state
        .radio
        .set_album_enabled(&album_id, payload.enabled)
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

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from(bytes))
        .expect("file response should be valid"))
}

async fn song_audio(
    State(state): State<AppState>,
    Path(song_id): Path<String>,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    let Some(song_file) = state
        .radio
        .song_file(&song_id)
        .await
        .map_err(internal_api_error)?
    else {
        return Err(api_error(StatusCode::NOT_FOUND, "song_not_found"));
    };

    read_song_file_response(song_file, "audio_not_found").await
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

    read_song_file_response(song_file, "cover_not_found").await
}

async fn upload_song_cover(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(song_id): Path<String>,
    mut multipart: Multipart,
) -> Result<Json<crate::radio::Song>, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, &jar).await?;
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

async fn delete_song(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(song_id): Path<String>,
) -> Result<Json<crate::radio::RadioSnapshot>, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, &jar).await?;
    state
        .radio
        .delete_song(&song_id)
        .await
        .map(Json)
        .map_err(internal_api_error)
}

async fn upload_song(
    State(state): State<AppState>,
    jar: CookieJar,
    multipart: Multipart,
) -> Result<Json<crate::radio::Song>, (StatusCode, Json<ErrorResponse>)> {
    let session = admin_session(&state, &jar).await?;
    let upload = parse_song_upload(multipart).await?;

    state
        .radio
        .add_song(upload, &session.account_did)
        .await
        .map(Json)
        .map_err(internal_api_error)
}

async fn enqueue_song(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(payload): Json<EnqueueSongRequest>,
) -> Result<Json<crate::radio::QueueItem>, (StatusCode, Json<ErrorResponse>)> {
    let session = admin_session(&state, &jar).await?;

    state
        .radio
        .enqueue_song(&payload.song_id, &session.account_did)
        .await
        .map(Json)
        .map_err(internal_api_error)
}

async fn remove_queue_item(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(queue_id): Path<String>,
) -> Result<Json<crate::radio::RadioSnapshot>, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, &jar).await?;

    state
        .radio
        .remove_queue_item(&queue_id)
        .await
        .map(Json)
        .map_err(internal_api_error)
}

async fn control_radio(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(action): Path<String>,
) -> Result<Json<crate::radio::RadioSnapshot>, (StatusCode, Json<ErrorResponse>)> {
    let session = admin_session(&state, &jar).await?;
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
    ws.on_upgrade(|socket| radio_socket(state.radio, socket))
}

async fn radio_socket(radio: Arc<RadioService>, mut socket: WebSocket) {
    match radio.snapshot().await {
        Ok(snapshot) => {
            let event = crate::radio::RadioEvent::SnapshotChanged { snapshot };
            if let Ok(message) = event_message(&event) {
                if socket.send(message).await.is_err() {
                    return;
                }
            }
        }
        Err(error) => tracing::error!(?error, "failed to send initial radio snapshot"),
    }

    let mut events = radio.subscribe();
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
            message = socket.recv() => match message {
                Some(Ok(_)) => {}
                Some(Err(error)) => {
                    tracing::debug!(?error, "radio websocket closed with error");
                    break;
                }
                None => break,
            },
        }
    }
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

    Ok(NewSongUpload {
        filename,
        mime_type,
        bytes,
        title,
        artist,
        album,
        duration_seconds,
        add_to_queue,
    })
}

async fn admin_session(
    state: &AppState,
    jar: &CookieJar,
) -> Result<AppSession, (StatusCode, Json<ErrorResponse>)> {
    let session = state
        .auth
        .session(current_session_token(&state.auth, jar).as_deref())
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
    jar: CookieJar,
) -> Result<(CookieJar, StatusCode), (StatusCode, Json<ErrorResponse>)> {
    state
        .auth
        .sign_out(current_session_token(&state.auth, &jar).as_deref())
        .await
        .map_err(internal_api_error)?;

    Ok((
        jar.add(clear_session_cookie(&state.auth)),
        StatusCode::NO_CONTENT,
    ))
}

fn current_session_token(auth: &AuthService, jar: &CookieJar) -> Option<String> {
    jar.get(&auth.config().session_cookie_name)
        .map(|cookie| cookie.value().to_owned())
}

fn build_session_cookie(auth: &AuthService, session_token: &str) -> Cookie<'static> {
    let mut cookie = Cookie::new(
        auth.config().session_cookie_name.clone(),
        session_token.to_owned(),
    );
    cookie.set_path("/");
    cookie.set_http_only(true);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_secure(auth.config().secure_cookies());
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
