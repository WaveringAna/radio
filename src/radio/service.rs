use std::{path::PathBuf, sync::Arc};

use anyhow::{Context, anyhow};
use sqlx::FromRow;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::{chat::ChatService, db::Database, loudness, metadata};
use super::types::*;
use super::events::advance_duration_seconds;
use super::helpers::{extension, file_extension, is_unsupported_audio_file as is_unsupported_by_ext, now};

/// Service for radio state, queue, song storage, and realtime broadcasts.
#[derive(Clone)]
pub(crate) struct RadioService {
    db: Database,
    audio_dir: Arc<PathBuf>,
    cover_dir: Arc<PathBuf>,
    thumb_dir: Arc<PathBuf>,
    events: broadcast::Sender<RadioEvent>,
    chat: ChatService,
}

impl RadioService {
    /// Creates a radio service using local disk audio storage.
    pub(crate) fn new(db: Database, audio_dir: PathBuf, chat: ChatService) -> Self {
        let (events, _) = broadcast::channel(128);
        let data_dir = audio_dir
            .parent()
            .map(|p| p.to_owned())
            .unwrap_or_else(|| PathBuf::from("data"));
        let cover_dir = data_dir.join("covers");
        let thumb_dir = data_dir.join("thumbs");

        tokio::spawn(advance_loop(db.clone(), events.clone(), chat.clone()));

        Self {
            db,
            audio_dir: Arc::new(audio_dir),
            cover_dir: Arc::new(cover_dir),
            thumb_dir: Arc::new(thumb_dir),
            events,
            chat,
        }
    }

    /// Loads the current public radio snapshot without auto-advancing.
    pub(crate) async fn snapshot(&self) -> anyhow::Result<RadioSnapshot> {
        snapshot_from_db(&self.db).await
    }

    /// Loads the snapshot for a fresh external observer.
    pub(crate) async fn external_snapshot(&self) -> anyhow::Result<RadioSnapshot> {
        auto_advance(&self.db).await?;
        heal_empty_current_song(&self.db, "system").await?;
        heal_missing_current_song(&self.db, "system").await?;
        self.snapshot().await
    }

    /// Loads the current live seek position with auto-advance applied.
    pub(crate) async fn seek(&self) -> anyhow::Result<RadioSeek> {
        auto_advance(&self.db).await?;
        let state = radio_state(&self.db).await?;
        Ok(RadioSeek {
            position_seconds: state.position_seconds,
        })
    }

    /// Lists all uploaded songs.
    pub(crate) async fn songs(&self) -> anyhow::Result<Vec<Song>> {
        sqlx::query_as::<_, Song>(
            r#"
            select id, title, artist, album, genre, duration_seconds, mime_type,
                cover_path is not null as has_cover, added_by_did, created_at,
                loudness_lufs, loudness_peak
            from songs
            order by created_at desc
            "#,
        )
        .fetch_all(self.db.pool())
        .await
        .context("listing songs")
    }

    /// Removes non-audio playlist files that were previously accepted as songs.
    pub(crate) async fn cleanup_unsupported_audio_on_boot(&self) -> anyhow::Result<usize> {
        let removed = cleanup_unsupported_audio(&self.db).await?;
        if removed > 0 {
            heal_empty_current_song(&self.db, "system").await?;
            self.broadcast_snapshot().await;
        }
        Ok(removed)
    }

    /// Fetches and stores genres for songs where genre is currently empty.
    pub(crate) async fn backfill_missing_genres_on_boot(&self) -> anyhow::Result<usize> {
        let missing = sqlx::query_as::<_, MissingGenreSong>(
            r#"
            select id, title, artist, album
            from songs
            where genre is null or trim(genre) = ''
            "#,
        )
        .fetch_all(self.db.pool())
        .await
        .context("listing songs missing genre")?;

        let mut updated = 0usize;
        for song in missing {
            let Some(genre) =
                metadata::fetch_online_genre(&song.artist, song.album.as_deref(), &song.title)
                    .await
            else {
                continue;
            };
            let genre = genre.trim();
            if genre.is_empty() {
                continue;
            }
            let result = sqlx::query(
                r#"
                update songs
                set genre = ?
                where id = ? and (genre is null or trim(genre) = '')
                "#,
            )
            .bind(genre)
            .bind(&song.id)
            .execute(self.db.pool())
            .await
            .with_context(|| format!("updating genre for song {}", song.id))?;
            updated += result.rows_affected() as usize;
        }

        Ok(updated)
    }

    /// Measures and stores loudness for every song missing a `loudness_lufs` value.
    pub(crate) async fn backfill_missing_loudness_on_boot(&self) -> anyhow::Result<usize> {
        #[derive(FromRow)]
        struct MissingLoudness {
            id: String,
            file_path: String,
        }

        let missing = sqlx::query_as::<_, MissingLoudness>(
            r#"
            select id, file_path
            from songs
            where loudness_lufs is null
            "#,
        )
        .fetch_all(self.db.pool())
        .await
        .context("listing songs missing loudness")?;

        let total = missing.len();
        if total > 0 {
            tracing::info!(total, "measuring loudness for songs missing values");
        }

        let mut updated = 0usize;
        for (index, song) in missing.into_iter().enumerate() {
            if store_loudness(&self.db, &song.id, PathBuf::from(song.file_path)).await {
                updated += 1;
                tracing::debug!(
                    song_id = %song.id,
                    progress = format_args!("{}/{}", index + 1, total),
                    "loudness measured"
                );
            }
        }
        Ok(updated)
    }

    /// Lists admin-defined album loops.
    pub(crate) async fn albums(&self) -> anyhow::Result<Vec<RadioAlbum>> {
        album_loops(&self.db).await
    }

    /// Creates an album loop from explicit song ids.
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
        tracing::info!(album_id = %id, title, song_count = song_ids.len(), "album created");
        self.albums()
            .await?
            .into_iter()
            .find(|album| album.id == id)
            .ok_or_else(|| anyhow!("created album disappeared"))
    }

    /// Creates an album loop from songs with matching metadata album.
    pub(crate) async fn create_album_from_metadata(
        &self,
        album_title: &str,
    ) -> anyhow::Result<RadioAlbum> {
        let songs = sqlx::query_as::<_, Song>(
            r#"
            select id, title, artist, album, genre, duration_seconds, mime_type,
                cover_path is not null as has_cover, added_by_did, created_at,
                loudness_lufs, loudness_peak
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
    pub(crate) async fn delete_album(&self, album_id: &str) -> anyhow::Result<Vec<RadioAlbum>> {
        sqlx::query("delete from radio_albums where id = ?")
            .bind(album_id)
            .execute(self.db.pool())
            .await
            .context("deleting album")?;
        tracing::info!(album_id, "album deleted");
        self.albums().await
    }

    /// Appends songs to an existing album loop.
    pub(crate) async fn add_songs_to_album(
        &self,
        album_id: &str,
        song_ids: Vec<String>,
    ) -> anyhow::Result<RadioAlbum> {
        let song_ids: Vec<String> = song_ids
            .into_iter()
            .map(|id| id.trim().to_owned())
            .filter(|id| !id.is_empty())
            .collect();
        if song_ids.is_empty() {
            return Err(anyhow!("no songs supplied"));
        }

        let existing_ids: Vec<String> =
            sqlx::query_scalar("select song_id from radio_album_tracks where album_id = ?")
                .bind(album_id)
                .fetch_all(self.db.pool())
                .await
                .context("loading existing tracks")?;

        let max_pos: i64 = sqlx::query_scalar(
            "select coalesce(max(position), 0) from radio_album_tracks where album_id = ?",
        )
        .bind(album_id)
        .fetch_one(self.db.pool())
        .await
        .context("loading max track position")?;

        let new_ids: Vec<String> = song_ids
            .into_iter()
            .filter(|id| !existing_ids.contains(id))
            .collect();

        for (offset, song_id) in new_ids.iter().enumerate() {
            sqlx::query(
                "insert into radio_album_tracks (album_id, song_id, position) values (?, ?, ?)",
            )
            .bind(album_id)
            .bind(song_id)
            .bind(max_pos + offset as i64 + 1)
            .execute(self.db.pool())
            .await
            .context("adding track")?;
        }

        self.albums()
            .await?
            .into_iter()
            .find(|a| a.id == album_id)
            .ok_or_else(|| anyhow!("album not found"))
    }

    /// Enables or disables an album loop.
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
    pub(crate) async fn song_file(&self, song_id: &str) -> anyhow::Result<Option<SongFile>> {
        sqlx::query_as::<_, SongFile>("select file_path, mime_type from songs where id = ?")
            .bind(song_id)
            .fetch_optional(self.db.pool())
            .await
            .context("loading song file")
    }

    /// Loads the local cover file backing a song.
    pub(crate) async fn cover_file(&self, song_id: &str) -> anyhow::Result<Option<SongFile>> {
        sqlx::query_as::<_, SongFile>(
            "select cover_path as file_path, cover_mime_type as mime_type from songs where id = ? and cover_path is not null",
        )
        .bind(song_id)
        .fetch_optional(self.db.pool())
        .await
        .context("loading song cover")
    }

    /// Returns the path to a 128×128 JPEG thumbnail for a song cover.
    pub(crate) async fn cover_thumbnail(&self, song_id: &str) -> anyhow::Result<Option<PathBuf>> {
        let cover = self.cover_file(song_id).await?;
        let Some(cover) = cover else {
            return Ok(None);
        };

        let thumb_path = self.thumb_dir.join(format!("{song_id}.jpg"));
        if thumb_path.exists() {
            return Ok(Some(thumb_path));
        }

        let cover_bytes = tokio::fs::read(&cover.file_path)
            .await
            .context("reading cover for thumbnail")?;

        let thumb_dir = self.thumb_dir.clone();
        let thumb_path_clone = thumb_path.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            use image::imageops::FilterType;
            std::fs::create_dir_all(&*thumb_dir).context("creating thumb dir")?;
            let img = image::load_from_memory(&cover_bytes).context("decoding cover image")?;
            let thumb = img.resize(128, 128, FilterType::Lanczos3);
            thumb
                .save_with_format(&thumb_path_clone, image::ImageFormat::Jpeg)
                .context("saving thumbnail")?;
            Ok(())
        })
        .await
        .context("thumbnail task panicked")??;

        Ok(Some(thumb_path))
    }

    /// Updates editable metadata for an existing song.
    pub(crate) async fn update_song_metadata(
        &self,
        song_id: &str,
        update: SongMetadataUpdate,
    ) -> anyhow::Result<Song> {
        let title = update.title.trim();
        let artist = update.artist.trim();
        if title.is_empty() || artist.is_empty() {
            return Err(anyhow!("song title and artist are required"));
        }

        let album = update
            .album
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let genre = update
            .genre
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());

        let result = sqlx::query(
            r#"
            update songs
            set title = ?, artist = ?, album = ?, genre = ?, duration_seconds = ?
            where id = ?
            "#,
        )
        .bind(title)
        .bind(artist)
        .bind(album)
        .bind(genre)
        .bind(update.duration_seconds)
        .bind(song_id)
        .execute(self.db.pool())
        .await
        .context("updating song metadata")?;

        if result.rows_affected() == 0 {
            return Err(anyhow!("song not found"));
        }

        let song = find_song(&self.db, song_id)
            .await?
            .ok_or_else(|| anyhow!("song not found"))?;
        self.broadcast_snapshot().await;
        Ok(song)
    }

    /// Stores an uploaded cover for a song.
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

        let thumb_path = self.thumb_dir.join(format!("{song_id}.jpg"));
        let _ = tokio::fs::remove_file(thumb_path).await;

        let song = find_song(&self.db, song_id)
            .await?
            .ok_or_else(|| anyhow!("song not found"))?;
        self.broadcast_snapshot().await;
        Ok(song)
    }

    /// Deletes a song and its queue entries.
    pub(crate) async fn delete_song(&self, song_id: &str) -> anyhow::Result<RadioSnapshot> {
        sqlx::query("delete from songs where id = ?")
            .bind(song_id)
            .execute(self.db.pool())
            .await
            .context("deleting song")?;
        tracing::info!(song_id, "song deleted");
        let snapshot = self.snapshot().await?;
        let _ = self.events.send(RadioEvent::SnapshotChanged {
            snapshot: snapshot.clone(),
        });
        Ok(snapshot)
    }

    /// Stores an uploaded song and optionally appends it to the queue.
    pub(crate) async fn add_song(
        &self,
        upload: NewSongUpload,
        admin_did: &str,
    ) -> anyhow::Result<Song> {
        if upload.bytes.is_empty() {
            return Err(anyhow!("uploaded audio file is empty"));
        }

        let dedup_title = upload.title.trim();
        let dedup_artist = upload.artist.trim();
        let dedup_album = upload
            .album
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());

        let existing = sqlx::query_as::<_, Song>(
            r#"
            select id, title, artist, album, genre, duration_seconds, mime_type,
                cover_path is not null as has_cover, added_by_did, created_at,
                loudness_lufs, loudness_peak
            from songs
            where lower(title) = lower(?)
              and lower(artist) = lower(?)
              and (
                (album is null and ? is null)
                or lower(album) = lower(?)
              )
            limit 1
            "#,
        )
        .bind(dedup_title)
        .bind(dedup_artist)
        .bind(dedup_album)
        .bind(dedup_album)
        .fetch_optional(self.db.pool())
        .await
        .context("checking for existing song")?;

        if let Some(existing) = existing {
            if upload.add_to_queue {
                append_queue_item(&self.db, &existing.id, admin_did).await?;
                play_next_if_idle(&self.db, admin_did).await?;
                self.broadcast_snapshot().await;
            }
            return Ok(existing);
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
        let genre = upload.genre.filter(|g| !g.trim().is_empty());

        sqlx::query(
            r#"
            insert into songs (id, title, artist, album, genre, duration_seconds, file_path, mime_type, added_by_did, created_at)
            values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&id)
        .bind(upload.title.trim())
        .bind(upload.artist.trim())
        .bind(album.as_deref())
        .bind(genre.as_deref())
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
        tracing::info!(
            song_id = %song.id,
            title = %song.title,
            artist = %song.artist,
            album = song.album.as_deref(),
            admin_did,
            added_to_queue = upload.add_to_queue,
            "song uploaded"
        );
        self.broadcast_snapshot().await;

        tokio::spawn(measure_and_store_loudness(
            self.db.clone(),
            id,
            PathBuf::from(file_path_string),
        ));

        Ok(song)
    }

    /// Adds an existing song to the bottom of the queue.
    pub(crate) async fn enqueue_song(
        &self,
        song_id: &str,
        admin_did: &str,
    ) -> anyhow::Result<RadioSnapshot> {
        if find_song(&self.db, song_id).await?.is_none() {
            return Err(anyhow!("song not found"));
        }

        append_queue_item(&self.db, song_id, admin_did).await?;
        play_next_if_idle(&self.db, admin_did).await?;
        tracing::info!(song_id, admin_did, "queued song");
        self.broadcast_snapshot().await;
        self.snapshot().await
    }

    /// Appends multiple songs to the queue in order with a single broadcast.
    pub(crate) async fn enqueue_songs(
        &self,
        song_ids: &[String],
        admin_did: &str,
    ) -> anyhow::Result<RadioSnapshot> {
        for song_id in song_ids {
            if find_song(&self.db, song_id).await?.is_none() {
                return Err(anyhow!("song not found: {song_id}"));
            }
        }

        let base_position =
            sqlx::query_scalar::<_, Option<i64>>("select max(position) from radio_queue")
                .fetch_one(self.db.pool())
                .await
                .context("loading max queue position")?
                .unwrap_or(0);

        for (i, song_id) in song_ids.iter().enumerate() {
            let id = Uuid::new_v4().to_string();
            let position = base_position + i as i64 + 1;
            sqlx::query(
                "insert into radio_queue (id, song_id, position, queued_by_did) values (?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(song_id)
            .bind(position)
            .bind(admin_did)
            .execute(self.db.pool())
            .await
            .context("inserting queue item")?;
        }

        play_next_if_idle(&self.db, admin_did).await?;
        tracing::info!(count = song_ids.len(), admin_did, "queued songs in bulk");
        self.broadcast_snapshot().await;
        self.snapshot().await
    }

    /// Removes an item from the queue.
    pub(crate) async fn remove_queue_item(&self, queue_id: &str) -> anyhow::Result<RadioSnapshot> {
        sqlx::query("delete from radio_queue where id = ?")
            .bind(queue_id)
            .execute(self.db.pool())
            .await
            .context("removing queue item")?;
        tracing::info!(queue_id, "queue item removed");

        let snapshot = self.snapshot().await?;
        let _ = self.events.send(RadioEvent::SnapshotChanged {
            snapshot: snapshot.clone(),
        });
        Ok(snapshot)
    }

    /// Removes all queued items.
    pub(crate) async fn clear_queue(&self) -> anyhow::Result<RadioSnapshot> {
        let removed = sqlx::query("delete from radio_queue")
            .execute(self.db.pool())
            .await
            .context("clearing queue")?
            .rows_affected();
        tracing::info!(removed, "queue cleared");

        let snapshot = self.snapshot().await?;
        let _ = self.events.send(RadioEvent::SnapshotChanged {
            snapshot: snapshot.clone(),
        });
        Ok(snapshot)
    }

    /// Reorders the queue based on the supplied ordered list of queue ids.
    pub(crate) async fn reorder_queue(
        &self,
        queue_ids: &[String],
    ) -> anyhow::Result<RadioSnapshot> {
        let mut tx = self
            .db
            .pool()
            .begin()
            .await
            .context("starting reorder transaction")?;

        for (index, queue_id) in queue_ids.iter().enumerate() {
            sqlx::query("update radio_queue set position = ? where id = ?")
                .bind(index as i64 + 1)
                .bind(queue_id)
                .execute(&mut *tx)
                .await
                .context("updating queue position")?;
        }

        tx.commit().await.context("committing reorder")?;

        let snapshot = self.snapshot().await?;
        let _ = self.events.send(RadioEvent::SnapshotChanged {
            snapshot: snapshot.clone(),
        });
        Ok(snapshot)
    }

    /// Updates playback status or advances the queue.
    pub(crate) async fn control(
        &self,
        action: RadioControlAction,
        admin_did: &str,
    ) -> anyhow::Result<RadioSnapshot> {
        auto_advance(&self.db).await?;
        let before = current_song_id(&self.db).await.ok().flatten();
        let action_name = match action {
            RadioControlAction::Play => "play",
            RadioControlAction::Pause => "pause",
            RadioControlAction::Stop => "stop",
            RadioControlAction::Skip => "skip",
            RadioControlAction::Previous => "previous",
        };
        match action {
            RadioControlAction::Play => play_or_resume(&self.db, admin_did).await?,
            RadioControlAction::Pause => set_status(&self.db, "paused", admin_did).await?,
            RadioControlAction::Stop => set_status(&self.db, "stopped", admin_did).await?,
            RadioControlAction::Skip => skip_to_next(&self.db, admin_did).await?,
            RadioControlAction::Previous => reset_current(&self.db, admin_did).await?,
        }
        tracing::info!(action = action_name, admin_did, "playback control");

        let snapshot = self.snapshot().await?;
        let after = snapshot.current_song.as_ref().map(|song| song.id.clone());
        if before != after {
            if let Some(song_id) = after {
                spawn_now_playing_announcement(self.db.clone(), self.chat.clone(), song_id);
            }
        }
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

// ── Internal helper functions ──

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

async fn advance_loop(db: Database, events: broadcast::Sender<RadioEvent>, chat: ChatService) {
    let mut rx = events.subscribe();
    loop {
        let sleep_for = match next_advance_in(&db).await {
            Ok(Some(duration)) => duration,
            Ok(None) => std::time::Duration::from_secs(3600),
            Err(error) => {
                tracing::error!(?error, "advance loop sleep calc failed");
                std::time::Duration::from_secs(60)
            }
        };

        tokio::select! {
            _ = tokio::time::sleep(sleep_for) => {
                let before = current_song_id(&db).await.ok().flatten();
                if let Err(error) = auto_advance(&db).await {
                    tracing::error!(?error, "advance loop failed to advance");
                    continue;
                }
                let after = current_song_id(&db).await.ok().flatten();
                if before != after {
                    if let Some(song_id) = after {
                        spawn_now_playing_announcement(db.clone(), chat.clone(), song_id);
                    }
                }
            }
            res = rx.recv() => {
                match res {
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        }
    }
}

async fn snapshot_from_db(db: &Database) -> anyhow::Result<RadioSnapshot> {
    let state = radio_state(db).await?;
    let current_song = match &state.current_song_id {
        Some(song_id) => find_song(db, song_id).await?,
        None => None,
    };
    let queue = queue_items(db).await?;

    Ok(RadioSnapshot {
        state,
        current_song,
        queue,
    })
}

async fn cleanup_unsupported_audio(db: &Database) -> anyhow::Result<usize> {
    let songs = stored_audio_files(db).await?;
    let mut removed = 0usize;

    for song in songs {
        if is_unsupported_audio_file(&song.file_path, song.mime_type.as_deref()).await {
            quarantine_unsupported_song(db, &song).await?;
            removed += 1;
        }
    }

    Ok(removed)
}

async fn stored_audio_files(db: &Database) -> anyhow::Result<Vec<StoredAudioFile>> {
    sqlx::query_as::<_, StoredAudioFile>(
        r#"
        select id, title, artist, file_path, mime_type
        from songs
        "#,
    )
    .fetch_all(db.pool())
    .await
    .context("loading stored audio files")
}

async fn stored_audio_file(
    db: &Database,
    song_id: &str,
) -> anyhow::Result<Option<StoredAudioFile>> {
    sqlx::query_as::<_, StoredAudioFile>(
        r#"
        select id, title, artist, file_path, mime_type
        from songs
        where id = ?
        "#,
    )
    .bind(song_id)
    .fetch_optional(db.pool())
    .await
    .context("loading stored audio file")
}

async fn unsupported_audio_song(
    db: &Database,
    song_id: &str,
) -> anyhow::Result<Option<StoredAudioFile>> {
    let Some(song) = stored_audio_file(db, song_id).await? else {
        return Ok(None);
    };
    if is_unsupported_audio_file(&song.file_path, song.mime_type.as_deref()).await {
        return Ok(Some(song));
    }

    Ok(None)
}

async fn quarantine_unsupported_song(db: &Database, song: &StoredAudioFile) -> anyhow::Result<()> {
    let removed = sqlx::query("delete from songs where id = ?")
        .bind(&song.id)
        .execute(db.pool())
        .await
        .context("deleting unsupported audio song")?
        .rows_affected();

    if removed == 0 {
        return Ok(());
    }

    match tokio::fs::remove_file(&song.file_path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(%error, path = %song.file_path, "failed to remove unsupported audio file")
        }
    }

    tracing::warn!(
        song_id = %song.id,
        title = %song.title,
        artist = %song.artist,
        path = %song.file_path,
        "removed unsupported audio song"
    );

    Ok(())
}

async fn heal_empty_current_song(db: &Database, admin_did: &str) -> anyhow::Result<()> {
    let state = radio_state(db).await?;
    if state.current_song_id.is_some() {
        return Ok(());
    }

    match state.status.as_str() {
        "playing" => skip_to_next(db, admin_did).await,
        "paused" => set_status(db, "stopped", admin_did).await,
        _ => Ok(()),
    }
}

async fn heal_missing_current_song(db: &Database, admin_did: &str) -> anyhow::Result<()> {
    let state = radio_state(db).await?;
    let Some(song_id) = state.current_song_id.as_deref() else {
        return Ok(());
    };
    if find_song(db, song_id).await?.is_some() {
        return Ok(());
    }

    tracing::warn!(
        song_id,
        status = %state.status,
        "current song row missing; healing playback state"
    );
    match state.status.as_str() {
        "playing" => skip_to_next(db, admin_did).await,
        "paused" => set_status(db, "stopped", admin_did).await,
        _ => {
            let timestamp = now();
            sqlx::query(
                r#"
                update radio_state
                set current_song_id = null, position_seconds = 0, updated_by_did = ?,
                    updated_at = ?
                where id = 1
                "#,
            )
            .bind(admin_did)
            .bind(timestamp)
            .execute(db.pool())
            .await
            .context("clearing missing current song")?;
            Ok(())
        }
    }
}

async fn is_unsupported_audio_file(file_path: &str, mime_type: Option<&str>) -> bool {
    is_unsupported_by_ext(file_path, mime_type)
        || super::helpers::file_starts_with_extm3u(file_path).await
}

async fn current_song_id(db: &Database) -> anyhow::Result<Option<String>> {
    let row: Option<(Option<String>,)> =
        sqlx::query_as("select current_song_id from radio_state where id = 1")
            .fetch_optional(db.pool())
            .await
            .context("loading current song id")?;
    Ok(row.and_then(|(id,)| id))
}

fn spawn_now_playing_announcement(db: Database, chat: ChatService, song_id: String) {
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        let song = match find_song(&db, &song_id).await {
            Ok(Some(song)) => song,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(?error, "failed to load song for now-playing announcement");
                return;
            }
        };
        let body = format!("{} — {}", song.title, song.artist);
        if let Err(error) = chat.post_now_playing(&body).await {
            tracing::warn!(%error, "failed to post now-playing chat row");
        }
    });
}

async fn next_advance_in(db: &Database) -> anyhow::Result<Option<std::time::Duration>> {
    let raw = sqlx::query_as::<_, RadioState>(
        r#"
        select current_song_id, status, started_at, paused_at, position_seconds, updated_by_did
        from radio_state
        where id = 1
        "#,
    )
    .fetch_one(db.pool())
    .await
    .context("loading state for advance scheduling")?;

    if raw.status != "playing" {
        return Ok(None);
    }
    let Some(started_at) = raw.started_at else {
        return Ok(None);
    };
    let Some(current_id) = raw.current_song_id.as_deref() else {
        return Ok(None);
    };
    if unsupported_audio_song(db, current_id).await?.is_some() {
        return Ok(Some(std::time::Duration::from_secs(1)));
    }
    let Some(song) = find_song(db, current_id).await? else {
        return Ok(Some(std::time::Duration::from_secs(1)));
    };
    let duration = advance_duration_seconds(&song);

    let elapsed = raw.position_seconds + now().saturating_sub(started_at);
    let remaining = duration.saturating_sub(elapsed).max(1);
    Ok(Some(std::time::Duration::from_secs(remaining as u64)))
}

async fn auto_advance(db: &Database) -> anyhow::Result<()> {
    loop {
        let raw = sqlx::query_as::<_, RadioState>(
            r#"
            select current_song_id, status, started_at, paused_at, position_seconds, updated_by_did
            from radio_state
            where id = 1
            "#,
        )
        .fetch_one(db.pool())
        .await
        .context("loading raw radio state for auto-advance")?;

        if raw.status != "playing" {
            return Ok(());
        }
        let Some(started_at) = raw.started_at else {
            return Ok(());
        };
        let Some(current_id) = raw.current_song_id.as_deref() else {
            return Ok(());
        };
        if let Some(song) = unsupported_audio_song(db, current_id).await? {
            let admin = raw.updated_by_did.as_deref().unwrap_or("system").to_owned();
            quarantine_unsupported_song(db, &song).await?;
            heal_empty_current_song(db, &admin).await?;
            continue;
        }
        let Some(song) = find_song(db, current_id).await? else {
            let admin = raw.updated_by_did.as_deref().unwrap_or("system").to_owned();
            tracing::warn!(
                from_song_id = current_id,
                "auto-advance: current song missing"
            );
            heal_missing_current_song(db, &admin).await?;
            continue;
        };
        let duration = advance_duration_seconds(&song);

        let elapsed = raw.position_seconds + now().saturating_sub(started_at);
        if elapsed < duration {
            return Ok(());
        }

        let overflow = elapsed - duration;
        let admin = raw.updated_by_did.as_deref().unwrap_or("system").to_owned();
        if song.duration_seconds.is_some_and(|stored| stored > 0) {
            tracing::info!(
                from_song_id = current_id,
                elapsed,
                duration,
                "auto-advance: track ended"
            );
        } else {
            tracing::warn!(
                from_song_id = current_id,
                elapsed,
                fallback_duration = duration,
                "auto-advance: unknown-duration track timed out"
            );
        }
        skip_to_next(db, &admin).await?;

        sqlx::query(
            r#"
            update radio_state
            set started_at = ?, position_seconds = 0
            where id = 1 and status = 'playing'
            "#,
        )
        .bind(now() - overflow)
        .execute(db.pool())
        .await
        .context("backdating started_at after auto-advance")?;
    }
}

async fn measure_and_store_loudness(db: Database, song_id: String, file_path: PathBuf) {
    store_loudness(&db, &song_id, file_path).await;
}

async fn store_loudness(db: &Database, song_id: &str, file_path: PathBuf) -> bool {
    let started = std::time::Instant::now();
    let measurement = match loudness::measure(&file_path).await {
        Ok(measurement) => measurement,
        Err(error) => {
            tracing::warn!(%error, song_id, "failed to measure loudness");
            return false;
        }
    };
    let result = sqlx::query(
        r#"
        update songs
        set loudness_lufs = ?, loudness_peak = ?
        where id = ?
        "#,
    )
    .bind(measurement.integrated_lufs)
    .bind(measurement.true_peak_dbfs)
    .bind(song_id)
    .execute(db.pool())
    .await;
    match result {
        Ok(_) => {
            tracing::info!(
                song_id,
                lufs = measurement.integrated_lufs,
                peak_dbfs = measurement.true_peak_dbfs,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "measured loudness"
            );
            true
        }
        Err(error) => {
            tracing::warn!(%error, song_id, "failed to persist loudness");
            false
        }
    }
}

async fn find_song(db: &Database, song_id: &str) -> anyhow::Result<Option<Song>> {
    sqlx::query_as::<_, Song>(
        r#"
        select id, title, artist, album, genre, duration_seconds, mime_type,
            cover_path is not null as has_cover, added_by_did, created_at,
                loudness_lufs, loudness_peak
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
            songs.id as song_id, songs.title, songs.artist, songs.album, songs.genre,
            songs.duration_seconds, songs.mime_type,
            songs.cover_path is not null as has_cover,
            songs.added_by_did, songs.created_at,
            songs.loudness_lufs, songs.loudness_peak
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
            select songs.id, songs.title, songs.artist, songs.album, songs.genre,
                songs.duration_seconds, songs.mime_type,
                songs.cover_path is not null as has_cover,
                songs.added_by_did, songs.created_at,
                songs.loudness_lufs, songs.loudness_peak
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
    if !current_song_is_loadable(db, &state).await? {
        return skip_to_next(db, admin_did).await;
    }

    set_status(db, "playing", admin_did).await
}

async fn play_next_if_idle(db: &Database, admin_did: &str) -> anyhow::Result<()> {
    let state = radio_state(db).await?;
    if !current_song_is_loadable(db, &state).await? {
        skip_to_next(db, admin_did).await?;
    }

    Ok(())
}

async fn current_song_is_loadable(db: &Database, state: &RadioState) -> anyhow::Result<bool> {
    let Some(song_id) = state.current_song_id.as_deref() else {
        return Ok(false);
    };
    let exists = find_song(db, song_id).await?.is_some();
    if !exists {
        tracing::warn!(
            song_id,
            status = %state.status,
            "current song row missing; treating radio as idle"
        );
    }
    Ok(exists)
}

async fn set_status(db: &Database, status: &str, admin_did: &str) -> anyhow::Result<()> {
    let state = radio_state(db).await?;
    if state.status == status {
        return Ok(());
    }

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
            songs.id as song_id, songs.title, songs.artist, songs.album, songs.genre,
            songs.duration_seconds, songs.mime_type,
            songs.cover_path is not null as has_cover,
            songs.added_by_did, songs.created_at,
            songs.loudness_lufs, songs.loudness_peak
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
                .bind(&item.id)
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
            .bind(&item.song_id)
            .bind(timestamp)
            .bind(admin_did)
            .bind(timestamp)
            .execute(db.pool())
            .await
            .context("advancing radio state")?;
            tracing::info!(
                song_id = %item.song_id,
                title = %item.title,
                artist = %item.artist,
                "now playing from queue"
            );
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
                .bind(&track.song_id)
                .bind(timestamp)
                .bind(admin_did)
                .bind(timestamp)
                .execute(db.pool())
                .await
                .context("advancing radio state from album loop")?;
                tracing::info!(
                    song_id = %track.song_id,
                    album_id = %track.album_id,
                    track_position = track.track_position,
                    "now playing from album loop"
                );
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
                tracing::info!("queue + album loop empty, radio stopped");
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

#[cfg(test)]
mod tests {
    // Tests moved to tests module
}
