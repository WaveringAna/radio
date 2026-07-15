use axum::{
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    response::IntoResponse,
};

use super::AppState;
use crate::chat::{ChatEvent, chat_event_message};

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
                Some(Ok(Message::Text(_))) => {}
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
