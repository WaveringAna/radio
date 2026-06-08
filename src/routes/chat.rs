use axum::{Json, extract::{Path, State, ws::{Message, WebSocket}, WebSocketUpgrade}, http::StatusCode, response::IntoResponse};
use serde::Deserialize;

use super::{AppState, ErrorResponse, SessionToken, api_error, internal_api_error, admin_session};
use super::helpers::valid_listener_did;
use crate::chat::{ChatBan, ChatEvent, MAX_CHAT_BODY_LEN, chat_event_message};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChatBanRequest {
    pub(crate) did: String,
    #[serde(default)]
    pub(crate) reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub(crate) enum ChatClientMessage {
    Send {
        text: String,
        #[serde(default)]
        token: Option<String>,
    },
}

pub(crate) async fn delete_chat_message(
    State(state): State<AppState>,
    session_token: SessionToken,
    Path(message_id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, session_token.0.as_deref()).await?;
    let removed = state
        .chat
        .delete_message(&message_id)
        .await
        .map_err(internal_api_error)?;
    if removed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(api_error(StatusCode::NOT_FOUND, "chat_message_not_found"))
    }
}

pub(crate) async fn list_chat_bans(
    State(state): State<AppState>,
    session_token: SessionToken,
) -> Result<Json<Vec<ChatBan>>, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, session_token.0.as_deref()).await?;
    state
        .chat
        .list_bans()
        .await
        .map(Json)
        .map_err(internal_api_error)
}

pub(crate) async fn create_chat_ban(
    State(state): State<AppState>,
    session_token: SessionToken,
    Json(payload): Json<ChatBanRequest>,
) -> Result<Json<ChatBan>, (StatusCode, Json<ErrorResponse>)> {
    let session = admin_session(&state, session_token.0.as_deref()).await?;
    let did = payload.did.trim();
    if !valid_listener_did(did) {
        return Err(api_error(StatusCode::BAD_REQUEST, "invalid_did"));
    }
    let reason = payload
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    state
        .chat
        .ban_did(did, &session.account_did, reason)
        .await
        .map(Json)
        .map_err(internal_api_error)
}

pub(crate) async fn remove_chat_ban(
    State(state): State<AppState>,
    session_token: SessionToken,
    Path(did): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, session_token.0.as_deref()).await?;
    let removed = state
        .chat
        .unban_did(&did)
        .await
        .map_err(internal_api_error)?;
    if removed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(api_error(StatusCode::NOT_FOUND, "ban_not_found"))
    }
}

pub(crate) async fn chat_ws(
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(|socket| chat_socket(state, socket))
}

pub(crate) async fn chat_socket(state: AppState, mut socket: WebSocket) {
    let chat = state.chat.clone();

    match chat.recent().await {
        Ok(messages) => {
            let event = ChatEvent::History { messages };
            if let Ok(message) = chat_event_message(&event) {
                if socket.send(message).await.is_err() {
                    return;
                }
            }
        }
        Err(error) => tracing::error!(?error, "failed to load chat history"),
    }

    let mut events = chat.subscribe();

    loop {
        tokio::select! {
            event = events.recv() => match event {
                Ok(event) => match chat_event_message(&event) {
                    Ok(message) => {
                        if socket.send(message).await.is_err() {
                            break;
                        }
                    }
                    Err(error) => tracing::error!(?error, "failed to serialize chat event"),
                },
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
            message = socket.recv() => match message {
                Some(Ok(Message::Text(text))) => {
                    match serde_json::from_str::<ChatClientMessage>(&text) {
                        Ok(ChatClientMessage::Send { text, token }) => {
                            let session = match state.auth.session(token.as_deref()).await {
                                Ok(Some(session)) => session,
                                Ok(None) => {
                                    tracing::debug!("rejected unauthenticated chat send");
                                    continue;
                                }
                                Err(error) => {
                                    tracing::error!(?error, "failed to verify chat session");
                                    continue;
                                }
                            };
                            let body = text.trim();
                            if body.is_empty() || body.chars().count() > MAX_CHAT_BODY_LEN {
                                continue;
                            }
                            match chat.is_banned(&session.account_did).await {
                                Ok(true) => {
                                    tracing::debug!(did = %session.account_did, "rejected chat send from banned did");
                                    continue;
                                }
                                Ok(false) => {}
                                Err(error) => {
                                    tracing::error!(?error, "failed to check chat ban");
                                    continue;
                                }
                            }
                            if let Err(error) = chat.post(&session.account_did, body).await {
                                tracing::error!(?error, "failed to persist chat message");
                            }
                        }
                        Err(error) => tracing::debug!(?error, "ignored malformed chat message"),
                    }
                }
                Some(Ok(Message::Close(_))) => break,
                Some(Ok(_)) => {}
                Some(Err(error)) => {
                    tracing::debug!(?error, "chat websocket closed with error");
                    break;
                }
                None => break,
            },
        }
    }
}
