use axum::{
    Json,
    extract::{Path, State, ws::{Message, WebSocket}, WebSocketUpgrade},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;
use tokio::time::{Instant, MissedTickBehavior};

use super::{AppState, ErrorResponse, SessionToken, api_error, internal_api_error, admin_session, RadioClientMessage};
use super::helpers::valid_viewer_id;
use super::helpers::valid_listener_did;
use super::VIEWER_KEEPALIVE_INTERVAL;
use super::VIEWER_KEEPALIVE_GRACE;
use crate::radio::{RadioControlAction, RadioEvent, RadioSeek, event_message};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ControlRadioRequest {
    pub(crate) intent: String,
}

pub(crate) async fn get_radio_state(
    State(state): State<AppState>,
) -> Result<Json<crate::radio::RadioSnapshot>, (StatusCode, Json<ErrorResponse>)> {
    state
        .radio
        .external_snapshot()
        .await
        .map(Json)
        .map_err(internal_api_error)
}

pub(crate) async fn get_radio_seek(
    State(state): State<AppState>,
) -> Result<Json<RadioSeek>, (StatusCode, Json<ErrorResponse>)> {
    state
        .radio
        .seek()
        .await
        .map(Json)
        .map_err(internal_api_error)
}

pub(crate) async fn control_radio(
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

pub(crate) async fn radio_ws(
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(|socket| radio_socket(state, socket))
}

pub(crate) async fn radio_socket(state: AppState, mut socket: WebSocket) {
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
    let initial_stats = state.viewers.stats();
    let event = RadioEvent::ViewerCountChanged {
        viewer_count: initial_stats.count,
        listener_dids: initial_stats.listener_dids,
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
    let mut last_listener_did: Option<String> = None;

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
                Ok(stats) => {
                    let event = RadioEvent::ViewerCountChanged {
                        viewer_count: stats.count,
                        listener_dids: stats.listener_dids,
                    };
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
                        Ok(RadioClientMessage::ViewerHello { viewer_id, did })
                        | Ok(RadioClientMessage::ViewerKeepalive { viewer_id, did }) => {
                            let normalized_did = did
                                .map(|value| value.trim().to_owned())
                                .filter(|value| valid_listener_did(value));
                            if valid_viewer_id(&viewer_id) && registered_viewer_id.as_deref() != Some(viewer_id.as_str()) {
                                if let Some(previous_viewer_id) = registered_viewer_id.replace(viewer_id.clone()) {
                                    state.viewers.unregister(&previous_viewer_id);
                                }
                                let stats = state.viewers.register(&viewer_id, normalized_did.clone());
                                last_listener_did = normalized_did.clone();
                                let event = RadioEvent::ViewerCountChanged {
                                    viewer_count: stats.count,
                                    listener_dids: stats.listener_dids,
                                };
                                match event_message(&event) {
                                    Ok(message) => {
                                        if socket.send(message).await.is_err() {
                                            break;
                                        }
                                    }
                                    Err(error) => tracing::error!(?error, "failed to serialize viewer count event"),
                                }
                            } else if valid_viewer_id(&viewer_id)
                                && registered_viewer_id.as_deref() == Some(viewer_id.as_str())
                                && last_listener_did != normalized_did
                            {
                                state.viewers.update_did(&viewer_id, normalized_did.clone());
                                last_listener_did = normalized_did;
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
