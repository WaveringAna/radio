use axum::extract::ws::Message;
use super::types::*;

/// Serializes a radio event into a websocket text message.
pub(crate) fn event_message(event: &RadioEvent) -> anyhow::Result<Message> {
    Ok(Message::Text(serde_json::to_string(event)?.into()))
}

/// Returns the effective advance duration for a song, falling back to a long
/// timeout when no duration is stored.
pub(crate) fn advance_duration_seconds(song: &Song) -> i64 {
    song.duration_seconds
        .filter(|duration| *duration > 0)
        .unwrap_or(UNKNOWN_DURATION_ADVANCE_AFTER_SECONDS)
}
