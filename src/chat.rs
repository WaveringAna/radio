use std::sync::Arc;

use anyhow::Context;
use axum::extract::ws::Message;
use serde::Serialize;
use sqlx::FromRow;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::db::Database;

pub(crate) const MAX_CHAT_BODY_LEN: usize = 1000;
const CHAT_HISTORY_LIMIT: i64 = 100;

/// A single chat message persisted on the backend.
#[derive(Clone, Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChatMessage {
    pub(crate) id: String,
    pub(crate) sender_did: String,
    pub(crate) body: String,
    pub(crate) created_at: i64,
    /// Discriminator for rendering: "user" for posts, "now_playing" for
    /// system-generated track breadcrumbs.
    pub(crate) kind: String,
}

/// Realtime events broadcast to chat websocket clients.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub(crate) enum ChatEvent {
    /// Initial backlog sent on connect, oldest first.
    History { messages: Vec<ChatMessage> },
    /// A new message was relayed to all subscribers.
    Message { message: ChatMessage },
    /// A message was deleted by a moderator.
    MessageDeleted { id: String },
    /// Every message from this DID should be dropped (used on ban).
    MessagesPurged { sender_did: String },
}

/// Banned DID record returned to admin tooling.
#[derive(Clone, Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChatBan {
    pub(crate) did: String,
    pub(crate) banned_by_did: String,
    pub(crate) reason: Option<String>,
    pub(crate) created_at: i64,
}

/// Service that persists chat messages and relays them over a broadcast channel.
#[derive(Clone)]
pub(crate) struct ChatService {
    db: Database,
    events: Arc<broadcast::Sender<ChatEvent>>,
}

impl ChatService {
    pub(crate) fn new(db: Database) -> Self {
        let (events, _) = broadcast::channel(256);
        Self {
            db,
            events: Arc::new(events),
        }
    }

    /// Subscribes to relayed chat events.
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<ChatEvent> {
        self.events.subscribe()
    }

    /// Loads the most recent chat history, ordered oldest first for replay.
    /// Messages from banned DIDs are excluded so client scrollback matches
    /// what live subscribers see.
    ///
    /// # Errors
    /// Returns an error when sqlite queries fail.
    pub(crate) async fn recent(&self) -> anyhow::Result<Vec<ChatMessage>> {
        let mut rows = sqlx::query_as::<_, ChatMessage>(
            r#"
            select id, sender_did, body, created_at, kind
            from chat_messages
            where sender_did not in (select did from chat_bans)
            order by created_at desc, id desc
            limit ?
            "#,
        )
        .bind(CHAT_HISTORY_LIMIT)
        .fetch_all(self.db.pool())
        .await
        .context("loading chat history")?;
        rows.reverse();
        Ok(rows)
    }

    /// Persists a chat message and broadcasts it to subscribers.
    ///
    /// # Errors
    /// Returns an error when sqlite queries fail.
    pub(crate) async fn post(&self, sender_did: &str, body: &str) -> anyhow::Result<ChatMessage> {
        self.insert(sender_did, body, "user").await
    }

    /// Returns true when the supplied DID is on the ban list.
    ///
    /// # Errors
    /// Returns an error when sqlite queries fail.
    pub(crate) async fn is_banned(&self, did: &str) -> anyhow::Result<bool> {
        let row: Option<(String,)> = sqlx::query_as("select did from chat_bans where did = ?")
            .bind(did)
            .fetch_optional(self.db.pool())
            .await
            .context("checking chat ban")?;
        Ok(row.is_some())
    }

    /// Lists every banned DID, newest first.
    ///
    /// # Errors
    /// Returns an error when sqlite queries fail.
    pub(crate) async fn list_bans(&self) -> anyhow::Result<Vec<ChatBan>> {
        sqlx::query_as::<_, ChatBan>(
            r#"
            select did, banned_by_did, reason, created_at
            from chat_bans
            order by created_at desc
            "#,
        )
        .fetch_all(self.db.pool())
        .await
        .context("listing chat bans")
    }

    /// Adds a DID to the ban list. Idempotent on the (did) primary key.
    ///
    /// # Errors
    /// Returns an error when sqlite queries fail.
    pub(crate) async fn ban_did(
        &self,
        did: &str,
        banned_by_did: &str,
        reason: Option<&str>,
    ) -> anyhow::Result<ChatBan> {
        let ban = ChatBan {
            did: did.to_owned(),
            banned_by_did: banned_by_did.to_owned(),
            reason: reason.map(str::to_owned),
            created_at: time::OffsetDateTime::now_utc().unix_timestamp(),
        };
        sqlx::query(
            r#"
            insert into chat_bans (did, banned_by_did, reason, created_at)
            values (?, ?, ?, ?)
            on conflict(did) do update set
                banned_by_did = excluded.banned_by_did,
                reason = excluded.reason,
                created_at = excluded.created_at
            "#,
        )
        .bind(&ban.did)
        .bind(&ban.banned_by_did)
        .bind(&ban.reason)
        .bind(ban.created_at)
        .execute(self.db.pool())
        .await
        .context("inserting chat ban")?;

        // Messages stay in the DB (history is useful for moderation review);
        // we just tell live clients to drop them from view and exclude them
        // from future history sends.
        let _ = self.events.send(ChatEvent::MessagesPurged {
            sender_did: ban.did.clone(),
        });

        Ok(ban)
    }

    /// Lifts a ban for the supplied DID, if one is set.
    ///
    /// # Errors
    /// Returns an error when sqlite queries fail.
    pub(crate) async fn unban_did(&self, did: &str) -> anyhow::Result<bool> {
        let result = sqlx::query("delete from chat_bans where did = ?")
            .bind(did)
            .execute(self.db.pool())
            .await
            .context("removing chat ban")?;
        Ok(result.rows_affected() > 0)
    }

    /// Deletes a chat message and broadcasts the deletion to subscribers.
    ///
    /// # Errors
    /// Returns an error when sqlite queries fail.
    pub(crate) async fn delete_message(&self, message_id: &str) -> anyhow::Result<bool> {
        let result = sqlx::query("delete from chat_messages where id = ?")
            .bind(message_id)
            .execute(self.db.pool())
            .await
            .context("deleting chat message")?;
        let removed = result.rows_affected() > 0;
        if removed {
            let _ = self.events.send(ChatEvent::MessageDeleted {
                id: message_id.to_owned(),
            });
        }
        Ok(removed)
    }

    /// Persists a "now playing" breadcrumb and broadcasts it to subscribers.
    ///
    /// # Errors
    /// Returns an error when sqlite queries fail.
    pub(crate) async fn post_now_playing(&self, body: &str) -> anyhow::Result<ChatMessage> {
        self.insert("system", body, "now_playing").await
    }

    async fn insert(
        &self,
        sender_did: &str,
        body: &str,
        kind: &str,
    ) -> anyhow::Result<ChatMessage> {
        let message = ChatMessage {
            id: Uuid::new_v4().to_string(),
            sender_did: sender_did.to_owned(),
            body: body.to_owned(),
            created_at: time::OffsetDateTime::now_utc().unix_timestamp(),
            kind: kind.to_owned(),
        };

        sqlx::query(
            r#"
            insert into chat_messages (id, sender_did, body, created_at, kind)
            values (?, ?, ?, ?, ?)
            "#,
        )
        .bind(&message.id)
        .bind(&message.sender_did)
        .bind(&message.body)
        .bind(message.created_at)
        .bind(&message.kind)
        .execute(self.db.pool())
        .await
        .context("inserting chat message")?;

        let _ = self.events.send(ChatEvent::Message {
            message: message.clone(),
        });

        Ok(message)
    }
}

/// Serializes a chat event into a websocket text message.
pub(crate) fn chat_event_message(event: &ChatEvent) -> anyhow::Result<Message> {
    Ok(Message::Text(serde_json::to_string(event)?.into()))
}
