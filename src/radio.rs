use std::{path::PathBuf, sync::Arc};

use anyhow::{Context, anyhow};
use axum::extract::ws::Message;
use serde::Serialize;
use sqlx::FromRow;
use time::OffsetDateTime;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::db::Database;

/// Radio playback status persisted by the backend.
#[derive(Clone, Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RadioState {
    /// Currently active song id, when one is selected.
    pub(crate) current_song_id: Option<String>,
    /// Current playback status.
    pub(crate) status: String,
    /// Unix timestamp when playback was last started.
    pub(crate) started_at: Option<i64>,
    /// Unix timestamp when playback was last paused.
    pub(crate) paused_at: Option<i64>,
    /// Stored playback offset in seconds.
    pub(crate) position_seconds: i64,
    /// DID of the last admin to update radio state.
    pub(crate) updated_by_did: Option<String>,
}

/// Song metadata stored by the backend.
#[derive(Clone, Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Song {
    /// Stable song id.
    pub(crate) id: String,
    /// Song title.
    pub(crate) title: String,
    /// Song artist.
    pub(crate) artist: String,
    /// Optional album title.
    pub(crate) album: Option<String>,
    /// Optional duration in seconds.
    pub(crate) duration_seconds: Option<i64>,
    /// MIME type of the stored audio file.
    pub(crate) mime_type: Option<String>,
    /// Whether the song has an uploaded album cover.
    pub(crate) has_cover: bool,
    /// DID that uploaded the song.
    pub(crate) added_by_did: String,
    /// Unix timestamp when the song was uploaded.
    pub(crate) created_at: i64,
}

#[derive(Clone, Debug, FromRow)]
pub(crate) struct SongFile {
    /// Local path to the stored file.
    pub(crate) file_path: String,
    /// Stored file MIME type.
    pub(crate) mime_type: Option<String>,
}

/// Queue item joined with song metadata.
#[derive(Clone, Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QueueItem {
    /// Stable queue item id.
    pub(crate) id: String,
    /// Queue position, lower values play first.
    pub(crate) position: i64,
    /// DID that queued the song.
    pub(crate) queued_by_did: String,
    /// Queued song id.
    pub(crate) song_id: String,
    /// Queued song title.
    pub(crate) title: String,
    /// Queued song artist.
    pub(crate) artist: String,
    /// Optional queued song album.
    pub(crate) album: Option<String>,
    /// DID that originally uploaded the queued song.
    pub(crate) added_by_did: String,
}

/// Admin-defined album loop metadata.
#[derive(Clone, Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RadioAlbum {
    /// Stable album id.
    pub(crate) id: String,
    /// Album title shown in admin tools.
    pub(crate) title: String,
    /// Loop ordering, lower values play first.
    pub(crate) position: i64,
    /// Whether this album participates in fallback looping.
    pub(crate) is_enabled: bool,
    /// Songs in album playback order.
    #[sqlx(skip)]
    pub(crate) tracks: Vec<Song>,
}

/// Combined radio view returned to clients.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RadioSnapshot {
    /// Current radio playback state.
    pub(crate) state: RadioState,
    /// Current song metadata, when selected.
    pub(crate) current_song: Option<Song>,
    /// Upcoming queued songs.
    pub(crate) queue: Vec<QueueItem>,
}

/// Realtime events broadcast to websocket clients.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub(crate) enum RadioEvent {
    /// Radio snapshot changed.
    SnapshotChanged { snapshot: RadioSnapshot },
}

/// Uploaded song input after multipart parsing.
pub(crate) struct NewRadioAlbum {
    /// Album title shown in admin tools.
    pub(crate) title: String,
    /// Ordered songs to loop for this album.
    pub(crate) song_ids: Vec<String>,
}

/// Uploaded song input after multipart parsing.
pub(crate) struct NewSongUpload {
    /// Original uploaded filename.
    pub(crate) filename: Option<String>,
    /// Uploaded audio MIME type.
    pub(crate) mime_type: Option<String>,
    /// Uploaded audio bytes.
    pub(crate) bytes: Vec<u8>,
    /// Song title supplied by the admin.
    pub(crate) title: String,
    /// Song artist supplied by the admin.
    pub(crate) artist: String,
    /// Optional album supplied by the admin.
    pub(crate) album: Option<String>,
    /// Optional duration in seconds extracted from the audio file.
    pub(crate) duration_seconds: Option<i64>,
    /// Whether the song should be queued immediately.
    pub(crate) add_to_queue: bool,
}

/// Service for radio state, queue, song storage, and realtime broadcasts.
#[derive(Clone)]
pub(crate) struct RadioService {
    db: Database,
    audio_dir: Arc<PathBuf>,
    cover_dir: Arc<PathBuf>,
    events: broadcast::Sender<RadioEvent>,
}

impl RadioService {
    /// Creates a radio service using local disk audio storage.
    pub(crate) fn new(db: Database, audio_dir: PathBuf) -> Self {
        let (events, _) = broadcast::channel(128);
        let cover_dir = audio_dir
            .parent()
            .map(|parent| parent.join("covers"))
            .unwrap_or_else(|| PathBuf::from("data/covers"));
        Self {
            db,
            audio_dir: Arc::new(audio_dir),
            cover_dir: Arc::new(cover_dir),
            events,
        }
    }

    /// Loads the current public radio snapshot.
    ///
    /// # Errors
    /// Returns an error when sqlite queries fail.
    pub(crate) async fn snapshot(&self) -> anyhow::Result<RadioSnapshot> {
        let state = radio_state(&self.db).await?;
        let current_song = match &state.current_song_id {
            Some(song_id) => find_song(&self.db, song_id).await?,
            None => None,
        };
        let queue = queue_items(&self.db).await?;

        Ok(RadioSnapshot {
            state,
            current_song,
            queue,
        })
    }

    /// Lists all uploaded songs.
    ///
    /// # Errors
    /// Returns an error when sqlite queries fail.
    pub(crate) async fn songs(&self) -> anyhow::Result<Vec<Song>> {
        sqlx::query_as::<_, Song>(
            r#"
            select id, title, artist, album, duration_seconds, mime_type,
                cover_path is not null as has_cover, added_by_did, created_at
            from songs
            order by created_at desc
            "#,
        )
        .fetch_all(self.db.pool())
        .await
        .context("listing songs")
    }

    /// Lists admin-defined album loops.
    ///
    /// # Errors
    /// Returns an error when sqlite queries fail.
    pub(crate) async fn albums(&self) -> anyhow::Result<Vec<RadioAlbum>> {
        album_loops(&self.db).await
    }

    /// Creates an album loop from explicit song ids.
    ///
    /// # Errors
    /// Returns an error when sqlite persistence fails or no songs are supplied.
    pub(crate) async fn create_album(&self, album: NewRadioAlbum) -> anyhow::Result<RadioAlbum> {
        let song_ids: Vec<String> = album
            .song_ids
            .into_iter()
            .map(|song_id| song_id.trim().to_owned())
            .filter(|song_id| !song_id.is_empty())
            .collect();
        if song_ids.is_empty() {
            return Err(anyhow!("album needs at least one song"));
        }

        let id = Uuid::new_v4().to_string();
        let position =
            sqlx::query_scalar::<_, Option<i64>>("select max(position) from radio_albums")
                .fetch_one(self.db.pool())
                .await
                .context("loading max album position")?
                .unwrap_or(0)
                + 1;
        let title = album.title.trim();
        if title.is_empty() {
            return Err(anyhow!("album title is required"));
        }

        let mut tx = self
            .db
            .pool()
            .begin()
            .await
            .context("starting album transaction")?;
        sqlx::query("insert into radio_albums (id, title, position) values (?, ?, ?)")
            .bind(&id)
            .bind(title)
            .bind(position)
            .execute(&mut *tx)
            .await
            .context("creating album")?;
        for (index, song_id) in song_ids.iter().enumerate() {
            sqlx::query(
                "insert into radio_album_tracks (album_id, song_id, position) values (?, ?, ?)",
            )
            .bind(&id)
            .bind(song_id)
            .bind(index as i64 + 1)
            .execute(&mut *tx)
            .await
            .context("adding album track")?;
        }
        tx.commit().await.context("committing album")?;
        self.albums()
            .await?
            .into_iter()
            .find(|album| album.id == id)
            .ok_or_else(|| anyhow!("created album disappeared"))
    }

    /// Creates an album loop from songs with matching metadata album.
    ///
    /// # Errors
    /// Returns an error when sqlite persistence fails or no songs match.
    pub(crate) async fn create_album_from_metadata(
        &self,
        album_title: &str,
    ) -> anyhow::Result<RadioAlbum> {
        let songs = sqlx::query_as::<_, Song>(
            r#"
            select id, title, artist, album, duration_seconds, mime_type,
                cover_path is not null as has_cover, added_by_did, created_at
            from songs
            where album = ?
            order by created_at asc, title asc
            "#,
        )
        .bind(album_title)
        .fetch_all(self.db.pool())
        .await
        .context("loading metadata album songs")?;

        self.create_album(NewRadioAlbum {
            title: album_title.to_owned(),
            song_ids: songs.into_iter().map(|song| song.id).collect(),
        })
        .await
    }

    /// Deletes an album loop.
    ///
    /// # Errors
    /// Returns an error when sqlite deletion fails.
    pub(crate) async fn delete_album(&self, album_id: &str) -> anyhow::Result<Vec<RadioAlbum>> {
        sqlx::query("delete from radio_albums where id = ?")
            .bind(album_id)
            .execute(self.db.pool())
            .await
            .context("deleting album")?;
        self.albums().await
    }

    /// Enables or disables an album loop.
    ///
    /// # Errors
    /// Returns an error when sqlite update fails.
    pub(crate) async fn set_album_enabled(
        &self,
        album_id: &str,
        enabled: bool,
    ) -> anyhow::Result<Vec<RadioAlbum>> {
        sqlx::query("update radio_albums set is_enabled = ? where id = ?")
            .bind(enabled)
            .bind(album_id)
            .execute(self.db.pool())
            .await
            .context("setting album enabled")?;
        self.albums().await
    }

    /// Loads the local file backing a song.
    ///
    /// # Errors
    /// Returns an error when sqlite lookup fails.
    pub(crate) async fn song_file(&self, song_id: &str) -> anyhow::Result<Option<SongFile>> {
        sqlx::query_as::<_, SongFile>("select file_path, mime_type from songs where id = ?")
            .bind(song_id)
            .fetch_optional(self.db.pool())
            .await
            .context("loading song file")
    }

    /// Loads the local cover file backing a song.
    ///
    /// # Errors
    /// Returns an error when sqlite lookup fails.
    pub(crate) async fn cover_file(&self, song_id: &str) -> anyhow::Result<Option<SongFile>> {
        sqlx::query_as::<_, SongFile>("select cover_path as file_path, cover_mime_type as mime_type from songs where id = ? and cover_path is not null")
            .bind(song_id)
            .fetch_optional(self.db.pool())
            .await
            .context("loading song cover")
    }

    /// Stores an uploaded cover for a song.
    ///
    /// # Errors
    /// Returns an error when file storage or sqlite persistence fails.
    pub(crate) async fn set_song_cover(
        &self,
        song_id: &str,
        filename: Option<String>,
        mime_type: Option<String>,
        bytes: Vec<u8>,
    ) -> anyhow::Result<Song> {
        tokio::fs::create_dir_all(self.cover_dir.as_ref())
            .await
            .context("creating cover directory")?;
        let file_path = self.cover_dir.join(format!(
            "{song_id}{}",
            file_extension(filename.as_deref(), ".image")
        ));
        tokio::fs::write(&file_path, bytes)
            .await
            .with_context(|| format!("writing cover file {}", file_path.display()))?;
        let file_path = file_path.to_string_lossy().into_owned();

        sqlx::query("update songs set cover_path = ?, cover_mime_type = ? where id = ?")
            .bind(file_path)
            .bind(mime_type.as_deref())
            .bind(song_id)
            .execute(self.db.pool())
            .await
            .context("updating song cover")?;

        let song = find_song(&self.db, song_id)
            .await?
            .ok_or_else(|| anyhow!("song not found"))?;
        self.broadcast_snapshot().await;
        Ok(song)
    }

    /// Deletes a song and its queue entries.
    ///
    /// # Errors
    /// Returns an error when sqlite deletion fails.
    pub(crate) async fn delete_song(&self, song_id: &str) -> anyhow::Result<RadioSnapshot> {
        sqlx::query("delete from songs where id = ?")
            .bind(song_id)
            .execute(self.db.pool())
            .await
            .context("deleting song")?;
        let snapshot = self.snapshot().await?;
        let _ = self.events.send(RadioEvent::SnapshotChanged {
            snapshot: snapshot.clone(),
        });
        Ok(snapshot)
    }

    /// Stores an uploaded song and optionally appends it to the queue.
    ///
    /// # Errors
    /// Returns an error when file storage or sqlite persistence fails.
    pub(crate) async fn add_song(
        &self,
        upload: NewSongUpload,
        admin_did: &str,
    ) -> anyhow::Result<Song> {
        if upload.bytes.is_empty() {
            return Err(anyhow!("uploaded audio file is empty"));
        }

        tokio::fs::create_dir_all(self.audio_dir.as_ref())
            .await
            .context("creating audio directory")?;

        let id = Uuid::new_v4().to_string();
        let file_path = self.audio_dir.join(format!("{id}{}", extension(&upload)));
        tokio::fs::write(&file_path, upload.bytes)
            .await
            .with_context(|| format!("writing audio file {}", file_path.display()))?;

        let created_at = now();
        let file_path_string = file_path.to_string_lossy().into_owned();
        let album = upload.album.filter(|value| !value.trim().is_empty());

        sqlx::query(
            r#"
            insert into songs (id, title, artist, album, duration_seconds, file_path, mime_type, added_by_did, created_at)
            values (?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&id)
        .bind(upload.title.trim())
        .bind(upload.artist.trim())
        .bind(album.as_deref())
        .bind(upload.duration_seconds)
        .bind(&file_path_string)
        .bind(upload.mime_type.as_deref())
        .bind(admin_did)
        .bind(created_at)
        .execute(self.db.pool())
        .await
        .context("inserting song")?;

        if upload.add_to_queue {
            append_queue_item(&self.db, &id, admin_did).await?;
            play_next_if_idle(&self.db, admin_did).await?;
        }

        let song = find_song(&self.db, &id)
            .await?
            .ok_or_else(|| anyhow!("inserted song disappeared"))?;
        self.broadcast_snapshot().await;
        Ok(song)
    }

    /// Adds an existing song to the bottom of the queue.
    ///
    /// # Errors
    /// Returns an error when the song does not exist or sqlite persistence fails.
    pub(crate) async fn enqueue_song(
        &self,
        song_id: &str,
        admin_did: &str,
    ) -> anyhow::Result<QueueItem> {
        if find_song(&self.db, song_id).await?.is_none() {
            return Err(anyhow!("song not found"));
        }

        let queue_id = append_queue_item(&self.db, song_id, admin_did).await?;
        let item = queue_item(&self.db, &queue_id)
            .await?
            .ok_or_else(|| anyhow!("inserted queue item disappeared"))?;
        play_next_if_idle(&self.db, admin_did).await?;
        self.broadcast_snapshot().await;
        Ok(item)
    }

    /// Removes an item from the queue.
    ///
    /// # Errors
    /// Returns an error when sqlite deletion fails.
    pub(crate) async fn remove_queue_item(&self, queue_id: &str) -> anyhow::Result<RadioSnapshot> {
        sqlx::query("delete from radio_queue where id = ?")
            .bind(queue_id)
            .execute(self.db.pool())
            .await
            .context("removing queue item")?;

        let snapshot = self.snapshot().await?;
        let _ = self.events.send(RadioEvent::SnapshotChanged {
            snapshot: snapshot.clone(),
        });
        Ok(snapshot)
    }

    /// Updates playback status or advances the queue.
    ///
    /// # Errors
    /// Returns an error when sqlite updates fail.
    pub(crate) async fn control(
        &self,
        action: RadioControlAction,
        admin_did: &str,
    ) -> anyhow::Result<RadioSnapshot> {
        match action {
            RadioControlAction::Play => play_or_resume(&self.db, admin_did).await?,
            RadioControlAction::Pause => set_status(&self.db, "paused", admin_did).await?,
            RadioControlAction::Stop => set_status(&self.db, "stopped", admin_did).await?,
            RadioControlAction::Skip => skip_to_next(&self.db, admin_did).await?,
            RadioControlAction::Previous => reset_current(&self.db, admin_did).await?,
        }

        let snapshot = self.snapshot().await?;
        let _ = self.events.send(RadioEvent::SnapshotChanged {
            snapshot: snapshot.clone(),
        });
        Ok(snapshot)
    }

    /// Subscribes to realtime radio events.
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<RadioEvent> {
        self.events.subscribe()
    }

    async fn broadcast_snapshot(&self) {
        match self.snapshot().await {
            Ok(snapshot) => {
                let _ = self.events.send(RadioEvent::SnapshotChanged { snapshot });
            }
            Err(error) => tracing::error!(?error, "failed to broadcast radio snapshot"),
        }
    }
}

/// Admin radio control actions.
pub(crate) enum RadioControlAction {
    /// Start or resume playback.
    Play,
    /// Pause playback.
    Pause,
    /// Stop playback.
    Stop,
    /// Advance to the next queued song.
    Skip,
    /// Reset the current song to the beginning.
    Previous,
}

/// Serializes a radio event into a websocket text message.
pub(crate) fn event_message(event: &RadioEvent) -> anyhow::Result<Message> {
    Ok(Message::Text(serde_json::to_string(event)?.into()))
}

async fn radio_state(db: &Database) -> anyhow::Result<RadioState> {
    let mut state = sqlx::query_as::<_, RadioState>(
        r#"
        select current_song_id, status, started_at, paused_at, position_seconds, updated_by_did
        from radio_state
        where id = 1
        "#,
    )
    .fetch_one(db.pool())
    .await
    .context("loading radio state")?;

    if state.status == "playing" {
        if let Some(started_at) = state.started_at {
            state.position_seconds += now().saturating_sub(started_at);
        }
    }

    Ok(state)
}

async fn find_song(db: &Database, song_id: &str) -> anyhow::Result<Option<Song>> {
    sqlx::query_as::<_, Song>(
        r#"
        select id, title, artist, album, duration_seconds, mime_type,
            cover_path is not null as has_cover, added_by_did, created_at
        from songs
        where id = ?
        "#,
    )
    .bind(song_id)
    .fetch_optional(db.pool())
    .await
    .context("loading song")
}

async fn queue_items(db: &Database) -> anyhow::Result<Vec<QueueItem>> {
    sqlx::query_as::<_, QueueItem>(
        r#"
        select radio_queue.id, radio_queue.position, radio_queue.queued_by_did,
            songs.id as song_id, songs.title, songs.artist, songs.album, songs.added_by_did
        from radio_queue
        join songs on songs.id = radio_queue.song_id
        order by radio_queue.position asc, radio_queue.created_at asc
        "#,
    )
    .fetch_all(db.pool())
    .await
    .context("loading queue")
}

async fn album_loops(db: &Database) -> anyhow::Result<Vec<RadioAlbum>> {
    let mut albums = sqlx::query_as::<_, RadioAlbum>(
        r#"
        select id, title, position, is_enabled
        from radio_albums
        order by position asc, created_at asc
        "#,
    )
    .fetch_all(db.pool())
    .await
    .context("loading albums")?;

    for album in &mut albums {
        album.tracks = sqlx::query_as::<_, Song>(
            r#"
            select songs.id, songs.title, songs.artist, songs.album, songs.duration_seconds,
                songs.mime_type, songs.cover_path is not null as has_cover,
                songs.added_by_did, songs.created_at
            from radio_album_tracks
            join songs on songs.id = radio_album_tracks.song_id
            where radio_album_tracks.album_id = ?
            order by radio_album_tracks.position asc
            "#,
        )
        .bind(&album.id)
        .fetch_all(db.pool())
        .await
        .context("loading album tracks")?;
    }

    Ok(albums)
}

async fn queue_item(db: &Database, queue_id: &str) -> anyhow::Result<Option<QueueItem>> {
    sqlx::query_as::<_, QueueItem>(
        r#"
        select radio_queue.id, radio_queue.position, radio_queue.queued_by_did,
            songs.id as song_id, songs.title, songs.artist, songs.album, songs.added_by_did
        from radio_queue
        join songs on songs.id = radio_queue.song_id
        where radio_queue.id = ?
        "#,
    )
    .bind(queue_id)
    .fetch_optional(db.pool())
    .await
    .context("loading queue item")
}

async fn append_queue_item(
    db: &Database,
    song_id: &str,
    queued_by_did: &str,
) -> anyhow::Result<String> {
    let id = Uuid::new_v4().to_string();
    let position = sqlx::query_scalar::<_, Option<i64>>("select max(position) from radio_queue")
        .fetch_one(db.pool())
        .await
        .context("loading max queue position")?
        .unwrap_or(0)
        + 1;

    sqlx::query(
        r#"
        insert into radio_queue (id, song_id, position, queued_by_did)
        values (?, ?, ?, ?)
        "#,
    )
    .bind(&id)
    .bind(song_id)
    .bind(position)
    .bind(queued_by_did)
    .execute(db.pool())
    .await
    .context("appending queue item")?;

    Ok(id)
}

async fn play_or_resume(db: &Database, admin_did: &str) -> anyhow::Result<()> {
    let state = radio_state(db).await?;
    if state.current_song_id.is_none() {
        return skip_to_next(db, admin_did).await;
    }

    set_status(db, "playing", admin_did).await
}

async fn play_next_if_idle(db: &Database, admin_did: &str) -> anyhow::Result<()> {
    let state = radio_state(db).await?;
    if state.current_song_id.is_none() {
        skip_to_next(db, admin_did).await?;
    }

    Ok(())
}

async fn set_status(db: &Database, status: &str, admin_did: &str) -> anyhow::Result<()> {
    let state = radio_state(db).await?;
    let timestamp = now();
    let position_seconds = match status {
        "playing" => state.position_seconds,
        "paused" => state.position_seconds,
        "stopped" => 0,
        _ => state.position_seconds,
    };
    let started_at = (status == "playing").then_some(timestamp);
    let paused_at = (status == "paused").then_some(timestamp);

    sqlx::query(
        r#"
        update radio_state
        set status = ?, started_at = ?, paused_at = ?, position_seconds = ?,
            updated_by_did = ?, updated_at = ?
        where id = 1
        "#,
    )
    .bind(status)
    .bind(started_at)
    .bind(paused_at)
    .bind(position_seconds)
    .bind(admin_did)
    .bind(timestamp)
    .execute(db.pool())
    .await
    .context("setting radio status")?;

    Ok(())
}

#[derive(FromRow)]
struct LoopTrack {
    album_id: String,
    track_position: i64,
    song_id: String,
}

async fn next_album_loop_track(db: &Database) -> anyhow::Result<Option<LoopTrack>> {
    let cursor = sqlx::query_as::<_, (Option<String>, i64)>(
        "select last_album_id, last_track_position from radio_loop_state where id = 1",
    )
    .fetch_one(db.pool())
    .await
    .context("loading album loop cursor")?;

    let next = if let Some(last_album_id) = cursor.0.as_deref() {
        sqlx::query_as::<_, LoopTrack>(
            r#"
            select radio_albums.id as album_id,
                radio_album_tracks.position as track_position,
                radio_album_tracks.song_id as song_id
            from radio_albums
            join radio_album_tracks on radio_album_tracks.album_id = radio_albums.id
            join songs on songs.id = radio_album_tracks.song_id
            where radio_albums.is_enabled = 1
              and (radio_albums.position, radio_album_tracks.position) > (
                (select position from radio_albums where id = ?), ?
              )
            order by radio_albums.position asc, radio_album_tracks.position asc
            limit 1
            "#,
        )
        .bind(last_album_id)
        .bind(cursor.1)
        .fetch_optional(db.pool())
        .await
        .context("loading next album loop track")?
    } else {
        None
    };

    match next {
        Some(track) => Ok(Some(track)),
        None => sqlx::query_as::<_, LoopTrack>(
            r#"
            select radio_albums.id as album_id,
                radio_album_tracks.position as track_position,
                radio_album_tracks.song_id as song_id
            from radio_albums
            join radio_album_tracks on radio_album_tracks.album_id = radio_albums.id
            join songs on songs.id = radio_album_tracks.song_id
            where radio_albums.is_enabled = 1
            order by radio_albums.position asc, radio_album_tracks.position asc
            limit 1
            "#,
        )
        .fetch_optional(db.pool())
        .await
        .context("loading first album loop track"),
    }
}

async fn skip_to_next(db: &Database, admin_did: &str) -> anyhow::Result<()> {
    let next = sqlx::query_as::<_, QueueItem>(
        r#"
        select radio_queue.id, radio_queue.position, radio_queue.queued_by_did,
            songs.id as song_id, songs.title, songs.artist, songs.album, songs.added_by_did
        from radio_queue
        join songs on songs.id = radio_queue.song_id
        order by radio_queue.position asc, radio_queue.created_at asc
        limit 1
        "#,
    )
    .fetch_optional(db.pool())
    .await
    .context("loading next queue item")?;

    let timestamp = now();
    match next {
        Some(item) => {
            sqlx::query("delete from radio_queue where id = ?")
                .bind(item.id)
                .execute(db.pool())
                .await
                .context("removing skipped queue item")?;
            sqlx::query(
                r#"
                update radio_state
                set current_song_id = ?, status = 'playing', started_at = ?, paused_at = null,
                    position_seconds = 0, updated_by_did = ?, updated_at = ?
                where id = 1
                "#,
            )
            .bind(item.song_id)
            .bind(timestamp)
            .bind(admin_did)
            .bind(timestamp)
            .execute(db.pool())
            .await
            .context("advancing radio state")?;
        }
        None => match next_album_loop_track(db).await? {
            Some(track) => {
                sqlx::query(
                    r#"
                    update radio_loop_state
                    set last_album_id = ?, last_track_position = ?
                    where id = 1
                    "#,
                )
                .bind(&track.album_id)
                .bind(track.track_position)
                .execute(db.pool())
                .await
                .context("updating album loop cursor")?;
                sqlx::query(
                    r#"
                    update radio_state
                    set current_song_id = ?, status = 'playing', started_at = ?, paused_at = null,
                        position_seconds = 0, updated_by_did = ?, updated_at = ?
                    where id = 1
                    "#,
                )
                .bind(track.song_id)
                .bind(timestamp)
                .bind(admin_did)
                .bind(timestamp)
                .execute(db.pool())
                .await
                .context("advancing radio state from album loop")?;
            }
            None => {
                sqlx::query(
                    r#"
                    update radio_state
                    set current_song_id = null, status = 'stopped', started_at = null, paused_at = null,
                        position_seconds = 0, updated_by_did = ?, updated_at = ?
                    where id = 1
                    "#,
                )
                .bind(admin_did)
                .bind(timestamp)
                .execute(db.pool())
                .await
                .context("stopping empty radio")?;
            }
        },
    }

    Ok(())
}

async fn reset_current(db: &Database, admin_did: &str) -> anyhow::Result<()> {
    let timestamp = now();
    sqlx::query(
        r#"
        update radio_state
        set position_seconds = 0, started_at = ?, updated_by_did = ?, updated_at = ?
        where id = 1
        "#,
    )
    .bind(timestamp)
    .bind(admin_did)
    .bind(timestamp)
    .execute(db.pool())
    .await
    .context("resetting current song")?;

    Ok(())
}

fn extension(upload: &NewSongUpload) -> String {
    file_extension(upload.filename.as_deref(), ".audio")
}

fn file_extension(filename: Option<&str>, fallback: &str) -> String {
    filename
        .and_then(|filename| filename.rsplit_once('.').map(|(_, extension)| extension))
        .filter(|extension| !extension.contains('/'))
        .map(|extension| format!(".{extension}"))
        .unwrap_or_else(|| fallback.into())
}

fn now() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_db() -> anyhow::Result<(Database, tempfile::TempDir)> {
        let temp_dir = tempfile::tempdir()?;
        let db_path = temp_dir.path().join("test.db");
        let db = Database::connect(&format!("sqlite://{}", db_path.display())).await?;
        db.prepare().await?;
        Ok((db, temp_dir))
    }

    async fn insert_song(db: &Database, id: &str, title: &str) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            insert into songs (id, title, artist, file_path, added_by_did)
            values (?, ?, 'artist', '/tmp/test.mp3', 'did:plc:uploader')
            "#,
        )
        .bind(id)
        .bind(title)
        .execute(db.pool())
        .await?;

        Ok(())
    }

    #[tokio::test]
    async fn skip_pops_next_queue_item_and_starts_it() -> anyhow::Result<()> {
        let (db, _temp_dir) = test_db().await?;
        insert_song(&db, "song-1", "first").await?;
        insert_song(&db, "song-2", "second").await?;
        insert_song(&db, "song-3", "third").await?;
        append_queue_item(&db, "song-2", "did:plc:admin").await?;
        append_queue_item(&db, "song-3", "did:plc:admin").await?;
        sqlx::query(
            "update radio_state set current_song_id = 'song-1', status = 'playing' where id = 1",
        )
        .execute(db.pool())
        .await?;

        skip_to_next(&db, "did:plc:admin").await?;
        let state = radio_state(&db).await?;
        let queue = queue_items(&db).await?;

        assert_eq!(state.current_song_id.as_deref(), Some("song-2"));
        assert_eq!(state.status, "playing");
        assert_eq!(state.position_seconds, 0);
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].song_id, "song-3");
        Ok(())
    }

    #[tokio::test]
    async fn skip_uses_album_loop_when_queue_is_empty() -> anyhow::Result<()> {
        let (db, _temp_dir) = test_db().await?;
        insert_song(&db, "song-1", "first").await?;
        insert_song(&db, "song-2", "second").await?;
        insert_song(&db, "song-3", "third").await?;
        sqlx::query("insert into radio_albums (id, title, position) values ('album-1', 'loop', 1)")
            .execute(db.pool())
            .await?;
        sqlx::query(
            "insert into radio_album_tracks (album_id, song_id, position) values ('album-1', 'song-2', 1), ('album-1', 'song-3', 2)",
        )
        .execute(db.pool())
        .await?;
        sqlx::query(
            "update radio_state set current_song_id = 'song-1', status = 'playing' where id = 1",
        )
        .execute(db.pool())
        .await?;

        skip_to_next(&db, "did:plc:admin").await?;
        let state = radio_state(&db).await?;
        assert_eq!(state.current_song_id.as_deref(), Some("song-2"));

        skip_to_next(&db, "did:plc:admin").await?;
        let state = radio_state(&db).await?;
        assert_eq!(state.current_song_id.as_deref(), Some("song-3"));

        skip_to_next(&db, "did:plc:admin").await?;
        let state = radio_state(&db).await?;
        assert_eq!(state.current_song_id.as_deref(), Some("song-2"));
        Ok(())
    }

    #[tokio::test]
    async fn skip_stops_when_queue_is_empty() -> anyhow::Result<()> {
        let (db, _temp_dir) = test_db().await?;
        insert_song(&db, "song-1", "first").await?;
        sqlx::query(
            "update radio_state set current_song_id = 'song-1', status = 'playing' where id = 1",
        )
        .execute(db.pool())
        .await?;

        skip_to_next(&db, "did:plc:admin").await?;
        let state = radio_state(&db).await?;

        assert_eq!(state.current_song_id, None);
        assert_eq!(state.status, "stopped");
        assert_eq!(state.position_seconds, 0);
        Ok(())
    }

    #[tokio::test]
    async fn paused_state_persists_elapsed_position() -> anyhow::Result<()> {
        let (db, _temp_dir) = test_db().await?;
        insert_song(&db, "song-1", "first").await?;
        let started_at = now() - 540;
        sqlx::query(
            "update radio_state set current_song_id = 'song-1', status = 'playing', started_at = ?, position_seconds = 0 where id = 1",
        )
        .bind(started_at)
        .execute(db.pool())
        .await?;

        set_status(&db, "paused", "did:plc:admin").await?;
        let state = radio_state(&db).await?;

        assert_eq!(state.status, "paused");
        assert!(state.position_seconds >= 540);
        assert!(state.started_at.is_none());
        assert!(state.paused_at.is_some());
        Ok(())
    }
}
