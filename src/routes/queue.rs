use axum::{Json, extract::{Path, State}, http::StatusCode};
use serde::Deserialize;

use super::{AppState, ErrorResponse, SessionToken, internal_api_error, admin_session};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EnqueueSongRequest {
    pub(crate) song_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EnqueueAlbumRequest {
    pub(crate) song_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReorderQueueRequest {
    pub(crate) queue_ids: Vec<String>,
}

pub(crate) async fn enqueue_song(
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

pub(crate) async fn enqueue_album(
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

pub(crate) async fn remove_queue_item(
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

pub(crate) async fn clear_queue(
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

pub(crate) async fn reorder_queue(
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
