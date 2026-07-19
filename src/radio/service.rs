use std::{path::PathBuf, sync::Arc};

use anyhow::{Context, anyhow};
use sqlx::FromRow;
use tokio::sync::broadcast;
use uuid::Uuid;

use super::events::advance_duration_seconds;
use super::helpers::{
    extension, file_extension, is_unsupported_audio_file as is_unsupported_by_ext, now,
};
use super::types::*;
use crate::{chat::ChatService, db::Database, loudness, metadata};

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
        let service = Self::build(db, audio_dir, chat);
        tokio::spawn(advance_loop(
            service.db.clone(),
            service.events.clone(),
            service.chat.clone(),
        ));
        service
    }

    /// Creates a radio service without the background advance loop. Used by the
    /// one-shot local importer so it can insert songs without racing the running
    /// server's playback advancement against the shared sqlite database.
    pub(crate) fn new_offline(db: Database, audio_dir: PathBuf, chat: ChatService) -> Self {
        Self::build(db, audio_dir, chat)
    }

    /// Shared field initialization for [`Self::new`] and [`Self::new_offline`].
    fn build(db: Database, audio_dir: PathBuf, chat: ChatService) -> Self {
        let (events, _) = broadcast::channel(128);
        let data_dir = audio_dir
            .parent()
            .map(|p| p.to_owned())
            .unwrap_or_else(|| PathBuf::from("data"));
        let cover_dir = data_dir.join("covers");
        let thumb_dir = data_dir.join("thumbs");

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
        let missing = sqlx::query_as::<_, MissingMetadataSong>(
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

    /// Searches for and stores artwork for songs where cover art is missing.
    pub(crate) async fn backfill_missing_covers_on_boot(&self) -> anyhow::Result<usize> {
        let missing = sqlx::query_as::<_, MissingMetadataSong>(
            r#"
            select id, title, artist, album
            from songs
            where cover_path is null
            "#,
        )
        .fetch_all(self.db.pool())
        .await
        .context("listing songs missing cover art")?;

        let total = missing.len();
        let client = reqwest::Client::builder()
            .user_agent("radio/0.1")
            .build()
            .context("building cover backfill client")?;
        let mut updated = 0usize;

        for (index, song) in missing.into_iter().enumerate() {
            if let Some((cover_bytes, cover_mime)) =
                metadata::fetch_ytdlp_cover(&client, &song.artist, &song.title).await
            {
                match self
                    .set_song_cover(&song.id, None, Some(cover_mime), cover_bytes)
                    .await
                {
                    Ok(_) => updated += 1,
                    Err(error) => tracing::warn!(
                        %error,
                        song_id = %song.id,
                        "failed to store backfilled song cover"
                    ),
                }
            }

            let processed = index + 1;
            if processed % 25 == 0 || processed == total {
                tracing::info!(processed, total, updated, "cover backfill progress");
            }
        }

        Ok(updated)
    }

    /// Automatically generates or updates album loops from song metadata.
    pub(crate) async fn auto_sync_albums(&self) -> anyhow::Result<()> {
        #[derive(FromRow)]
        struct AlbumSongInfo {
            id: String,
            album: Option<String>,
        }

        let songs = sqlx::query_as::<_, AlbumSongInfo>(
            r#"
            select id, album
            from songs
            where album is not null and trim(album) != ''
            order by created_at asc, title asc
            "#,
        )
        .fetch_all(self.db.pool())
        .await
        .context("loading all songs with albums for sync")?;

        let mut grouped: std::collections::HashMap<String, (Vec<String>, Vec<String>)> =
            std::collections::HashMap::new();
        for song in songs {
            if let Some(album) = song.album {
                let album_name = album.trim().to_owned();
                if !album_name.is_empty() {
                    let key = crate::radio::helpers::normalize_album_title(&album_name);
                    let entry = grouped
                        .entry(key)
                        .or_insert_with(|| (Vec::new(), Vec::new()));
                    entry.0.push(album_name);
                    entry.1.push(song.id);
                }
            }
        }

        let mut tx = self
            .db
            .pool()
            .begin()
            .await
            .context("starting auto-sync transaction")?;

        #[derive(FromRow)]
        struct ExistingAlbum {
            id: String,
            title: String,
        }
        let all_db_albums = sqlx::query_as::<_, ExistingAlbum>("select id, title from radio_albums")
            .fetch_all(&mut *tx)
            .await
            .context("loading all existing albums")?;

        for (key, (candidate_titles, song_ids)) in grouped {
            let album_name = Self::resolve_album_title_conflict(&candidate_titles);

            let mut matched_album_id = None;
            for db_album in &all_db_albums {
                if crate::radio::helpers::normalize_album_title(&db_album.title) == key {
                    matched_album_id = Some(db_album.id.clone());
                    break;
                }
            }

            let album_id = if let Some(id) = matched_album_id {
                sqlx::query("update radio_albums set title = ? where id = ?")
                    .bind(&album_name)
                    .bind(&id)
                    .execute(&mut *tx)
                    .await
                    .context("updating existing album title")?;
                id
            } else {
                let id = Uuid::new_v4().to_string();
                let position =
                    sqlx::query_scalar::<_, Option<i64>>("select max(position) from radio_albums")
                        .fetch_one(&mut *tx)
                        .await
                        .context("loading max album position")?
                        .unwrap_or(0)
                        + 1;

                sqlx::query("insert into radio_albums (id, title, position) values (?, ?, ?)")
                    .bind(&id)
                    .bind(&album_name)
                    .bind(position)
                    .execute(&mut *tx)
                    .await
                    .context("inserting new auto-generated album")?;
                id
            };

            // Update all songs in this group to have the exact resolved album name.
            for song_id in &song_ids {
                sqlx::query("update songs set album = ? where id = ?")
                    .bind(&album_name)
                    .bind(song_id)
                    .execute(&mut *tx)
                    .await
                    .context("updating song album metadata to resolved value")?;
            }

            // Sync tracks for this album.
            sqlx::query("delete from radio_album_tracks where album_id = ?")
                .bind(&album_id)
                .execute(&mut *tx)
                .await
                .context("clearing album tracks for sync")?;

            for (index, song_id) in song_ids.iter().enumerate() {
                sqlx::query(
                    "insert into radio_album_tracks (album_id, song_id, position) values (?, ?, ?)",
                )
                .bind(&album_id)
                .bind(song_id)
                .bind(index as i64 + 1)
                .execute(&mut *tx)
                .await
                .context("inserting album track")?;
            }
        }

        // Delete any empty albums.
        sqlx::query(
            "delete from radio_albums where id not in (select distinct album_id from radio_album_tracks)"
        )
        .execute(&mut *tx)
        .await
        .context("deleting empty albums")?;

        // Clean up radio_loop_state if its cursor points to a deleted album.
        sqlx::query(
            r#"
            update radio_loop_state
            set last_album_id = null, last_track_position = 0
            where last_album_id is not null
              and last_album_id not in (select id from radio_albums)
            "#,
        )
        .execute(&mut *tx)
        .await
        .context("cleaning up invalid radio_loop_state cursor")?;

        tx.commit()
            .await
            .context("committing auto-sync transaction")?;

        Ok(())
    }

    fn resolve_album_title_conflict(titles: &[String]) -> String {
        if titles.is_empty() {
            return String::new();
        }

        let mut counts = std::collections::HashMap::new();
        for t in titles {
            *counts.entry(t.as_str()).or_insert(0) += 1;
        }

        let mut best_title = titles[0].as_str();
        let mut best_count = 0;

        for (&t, &count) in &counts {
            if count > best_count {
                best_title = t;
                best_count = count;
            } else if count == best_count {
                if t < best_title {
                    best_title = t;
                }
            }
        }
        best_title.to_string()
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

    /// Lists all saved playlists.
    pub(crate) async fn playlists(&self) -> anyhow::Result<Vec<Playlist>> {
        let mut playlists = sqlx::query_as::<_, Playlist>(
            r#"
            select id, name, created_at
            from playlists
            order by created_at desc
            "#,
        )
        .fetch_all(self.db.pool())
        .await
        .context("listing playlists")?;

        for playlist in &mut playlists {
            let tracks = sqlx::query_as::<_, Song>(
                r#"
                select s.id, s.title, s.artist, s.album, s.genre, s.duration_seconds, s.mime_type,
                    s.cover_path is not null as has_cover, s.added_by_did, s.created_at,
                    s.loudness_lufs, s.loudness_peak
                from playlist_tracks pt
                join songs s on s.id = pt.song_id
                where pt.playlist_id = ?
                order by pt.position asc
                "#,
            )
            .bind(&playlist.id)
            .fetch_all(self.db.pool())
            .await
            .context("loading playlist tracks")?;
            playlist.tracks = tracks;
        }

        Ok(playlists)
    }

    /// Creates a new playlist from explicit song ids.
    pub(crate) async fn create_playlist(
        &self,
        name: &str,
        song_ids: &[String],
    ) -> anyhow::Result<Playlist> {
        let song_ids: Vec<String> = song_ids
            .iter()
            .map(|song_id| song_id.trim().to_owned())
            .filter(|song_id| !song_id.is_empty())
            .collect();
        if song_ids.is_empty() {
            return Err(anyhow!("playlist needs at least one song"));
        }

        let name = name.trim();
        if name.is_empty() {
            return Err(anyhow!("playlist name is required"));
        }

        let id = Uuid::new_v4().to_string();
        let mut tx = self
            .db
            .pool()
            .begin()
            .await
            .context("starting playlist transaction")?;

        sqlx::query("insert into playlists (id, name) values (?, ?)")
            .bind(&id)
            .bind(name)
            .execute(&mut *tx)
            .await
            .context("creating playlist")?;

        for (index, song_id) in song_ids.iter().enumerate() {
            sqlx::query(
                "insert into playlist_tracks (playlist_id, song_id, position) values (?, ?, ?)",
            )
            .bind(&id)
            .bind(song_id)
            .bind(index as i64 + 1)
            .execute(&mut *tx)
            .await
            .context("adding playlist track")?;
        }

        tx.commit().await.context("committing playlist")?;
        tracing::info!(playlist_id = %id, name, song_count = song_ids.len(), "playlist created");

        self.playlists()
            .await?
            .into_iter()
            .find(|pl| pl.id == id)
            .ok_or_else(|| anyhow!("created playlist disappeared"))
    }

    /// Deletes a playlist.
    pub(crate) async fn delete_playlist(&self, id: &str) -> anyhow::Result<()> {
        sqlx::query("delete from playlists where id = ?")
            .bind(id)
            .execute(self.db.pool())
            .await
            .context("deleting playlist")?;
        tracing::info!(playlist_id = %id, "playlist deleted");
        Ok(())
    }

    /// Loads playlist tracks into the queue.
    pub(crate) async fn load_playlist(
        &self,
        id: &str,
        replace: bool,
        admin_did: &str,
    ) -> anyhow::Result<RadioSnapshot> {
        let song_ids = sqlx::query_scalar::<_, String>(
            "select song_id from playlist_tracks where playlist_id = ? order by position asc",
        )
        .bind(id)
        .fetch_all(self.db.pool())
        .await
        .context("loading playlist track IDs")?;

        if song_ids.is_empty() {
            return Err(anyhow!("playlist is empty or not found"));
        }

        if replace {
            sqlx::query("delete from radio_queue")
                .execute(self.db.pool())
                .await
                .context("clearing queue for playlist load")?;
        }

        self.enqueue_songs(&song_ids, admin_did).await
    }

    /// Deletes an album loop and clears the album field from associated songs.
    pub(crate) async fn delete_album(&self, album_id: &str) -> anyhow::Result<Vec<RadioAlbum>> {
        let mut tx = self
            .db
            .pool()
            .begin()
            .await
            .context("starting delete_album transaction")?;

        let album_title: Option<String> =
            sqlx::query_scalar("select title from radio_albums where id = ?")
                .bind(album_id)
                .fetch_optional(&mut *tx)
                .await
                .context("finding album title for delete")?;

        if let Some(title) = album_title {
            sqlx::query("update songs set album = null where trim(album) = ? or lower(album) = ?")
                .bind(&title)
                .bind(title.to_lowercase())
                .execute(&mut *tx)
                .await
                .context("clearing album tag from songs")?;
        }

        sqlx::query("delete from radio_albums where id = ?")
            .bind(album_id)
            .execute(&mut *tx)
            .await
            .context("deleting album")?;

        tx.commit()
            .await
            .context("committing delete_album transaction")?;

        tracing::info!(
            album_id,
            "album deleted and associated songs' album tags cleared"
        );
        self.albums().await
    }

    /// Merges a duplicate source album into a target album.
    /// Combines their songs and updates loop state.
    pub(crate) async fn merge_albums(
        &self,
        source_id: &str,
        target_id: &str,
    ) -> anyhow::Result<Vec<RadioAlbum>> {
        if source_id == target_id {
            return Err(anyhow::anyhow!("cannot merge an album into itself"));
        }

        let mut tx = self
            .db
            .pool()
            .begin()
            .await
            .context("starting merge_albums transaction")?;

        // 1. Get the title of the target album
        let target_title: Option<String> =
            sqlx::query_scalar("select title from radio_albums where id = ?")
                .bind(target_id)
                .fetch_optional(&mut *tx)
                .await
                .context("finding target album title")?;

        let target_title = match target_title {
            Some(t) => t,
            None => return Err(anyhow::anyhow!("target album not found")),
        };

        // 2. Get the title of the source album
        let source_title: Option<String> =
            sqlx::query_scalar("select title from radio_albums where id = ?")
                .bind(source_id)
                .fetch_optional(&mut *tx)
                .await
                .context("finding source album title")?;

        let source_title = match source_title {
            Some(t) => t,
            None => return Err(anyhow::anyhow!("source album not found")),
        };

        // 3. Update all songs that have the source album title to use the target title
        sqlx::query("update songs set album = ? where trim(album) = ? or lower(album) = ?")
            .bind(&target_title)
            .bind(&source_title)
            .bind(source_title.to_lowercase())
            .execute(&mut *tx)
            .await
            .context("updating songs from source album to target album title")?;

        // Also update any song that is linked to the source album but might have a different/missing title
        sqlx::query(
            r#"
            update songs
            set album = ?
            where id in (
                select song_id from radio_album_tracks where album_id = ?
            )
            "#,
        )
        .bind(&target_title)
        .bind(source_id)
        .execute(&mut *tx)
        .await
        .context("updating songs linked to source album tracks")?;

        // 4. Update the radio loop state if it references the source album to the target album
        sqlx::query("update radio_loop_state set last_album_id = ? where last_album_id = ?")
            .bind(target_id)
            .bind(source_id)
            .execute(&mut *tx)
            .await
            .context("updating loop state cursor from source to target album")?;

        // 5. Delete the source album
        sqlx::query("delete from radio_albums where id = ?")
            .bind(source_id)
            .execute(&mut *tx)
            .await
            .context("deleting source album")?;

        tx.commit()
            .await
            .context("committing merge_albums transaction")?;

        // Run auto-sync to rebuild tracks for the target album and do cleanup
        self.auto_sync_albums().await?;

        tracing::info!(source_id, target_id, "albums merged successfully");
        self.albums().await
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
        self.auto_sync_albums().await?;
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
        self.auto_sync_albums().await?;
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
        self.auto_sync_albums().await?;
        self.broadcast_snapshot().await;

        tokio::spawn(measure_and_store_loudness(
            self.db.clone(),
            id,
            PathBuf::from(file_path_string),
        ));

        Ok(song)
    }

    /// Inserts a song whose audio stays where it already lives on disk, instead
    /// of copying bytes into `audio_dir`. Used by the local bulk importer to
    /// reference an existing library (e.g. files on an external drive) in place.
    ///
    /// Unlike [`add_song`], this never writes audio, never touches the queue,
    /// and does not spawn per-song loudness/album/broadcast work — the caller is
    /// expected to run those in batch (or let the boot-time backfills handle
    /// loudness). Returns `Ok(None)` when a song with the same
    /// title/artist/album already exists so the caller can count skips.
    ///
    /// [`add_song`]: Self::add_song
    pub(crate) async fn add_referenced_song(
        &self,
        file_path: PathBuf,
        upload: NewSongUpload,
        admin_did: &str,
    ) -> anyhow::Result<Option<Song>> {
        let dedup_title = upload.title.trim();
        let dedup_artist = upload.artist.trim();
        let dedup_album = upload
            .album
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());

        let existing = sqlx::query_scalar::<_, String>(
            r#"
            select id from songs
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

        if existing.is_some() {
            return Ok(None);
        }

        let id = Uuid::new_v4().to_string();
        let file_path_string = file_path.to_string_lossy().into_owned();
        let created_at = now();
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
        .context("inserting referenced song")?;

        let song = find_song(&self.db, &id)
            .await?
            .ok_or_else(|| anyhow!("inserted song disappeared"))?;
        Ok(Some(song))
    }

    /// Re-syncs album loops after a batch import. Exposed so the local importer
    /// can run it once at the end instead of per song.
    pub(crate) async fn sync_albums_after_import(&self) -> anyhow::Result<()> {
        self.auto_sync_albums().await
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
            RadioControlAction::Shuffle => "shuffle",
        };
        match action {
            RadioControlAction::Play => play_or_resume(&self.db, admin_did).await?,
            RadioControlAction::Pause => set_status(&self.db, "paused", admin_did).await?,
            RadioControlAction::Stop => set_status(&self.db, "stopped", admin_did).await?,
            RadioControlAction::Skip => skip_to_next(&self.db, admin_did).await?,
            RadioControlAction::Previous => reset_current(&self.db, admin_did).await?,
            RadioControlAction::Shuffle => toggle_shuffle(&self.db, admin_did).await?,
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

    /// Repopulates the shuffle lookahead on startup if shuffle is enabled, so a
    /// restart doesn't leave the queue empty while shuffle mode is on.
    pub(crate) async fn reconcile_shuffle_on_boot(&self) -> anyhow::Result<()> {
        if !shuffle_enabled(&self.db).await? {
            return Ok(());
        }
        let admin = sqlx::query_scalar::<_, Option<String>>(
            "select updated_by_did from radio_state where id = 1",
        )
        .fetch_one(self.db.pool())
        .await
        .context("loading shuffle reconcile admin")?
        .unwrap_or_else(|| "system".to_owned());
        refill_shuffle_queue(&self.db, &admin).await?;
        self.broadcast_snapshot().await;
        Ok(())
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
        select current_song_id, status, started_at, paused_at, position_seconds, updated_by_did, shuffle
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
        select current_song_id, status, started_at, paused_at, position_seconds, updated_by_did, shuffle
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
            select current_song_id, status, started_at, paused_at, position_seconds, updated_by_did, shuffle
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
            radio_queue.is_shuffle,
            songs.id as song_id, songs.title, songs.artist, songs.album, songs.genre,
            songs.duration_seconds, songs.mime_type,
            songs.cover_path is not null as has_cover,
            songs.added_by_did, songs.created_at,
            songs.loudness_lufs, songs.loudness_peak
        from radio_queue
        join songs on songs.id = radio_queue.song_id
        order by radio_queue.is_shuffle asc, radio_queue.position asc, radio_queue.created_at asc
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
            radio_queue.is_shuffle,
            songs.id as song_id, songs.title, songs.artist, songs.album, songs.genre,
            songs.duration_seconds, songs.mime_type,
            songs.cover_path is not null as has_cover,
            songs.added_by_did, songs.created_at,
            songs.loudness_lufs, songs.loudness_peak
        from radio_queue
        join songs on songs.id = radio_queue.song_id
        order by radio_queue.is_shuffle asc, radio_queue.position asc, radio_queue.created_at asc
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
        None => {
            // Queue is empty. In shuffle mode, pick a random track from the
            // whole library; otherwise step through album loops. Either way we
            // funnel through a single radio_state update below.
            let next_song_id = if shuffle_enabled(db).await? {
                random_shuffle_song(db).await?
            } else if let Some(track) = next_album_loop_track(db).await? {
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
                Some(track.song_id)
            } else {
                None
            };

            match next_song_id {
                Some(song_id) => {
                    sqlx::query(
                        r#"
                        update radio_state
                        set current_song_id = ?, status = 'playing', started_at = ?, paused_at = null,
                            position_seconds = 0, updated_by_did = ?, updated_at = ?
                        where id = 1
                        "#,
                    )
                    .bind(&song_id)
                    .bind(timestamp)
                    .bind(admin_did)
                    .bind(timestamp)
                    .execute(db.pool())
                    .await
                    .context("advancing radio state from fallback")?;
                    tracing::info!(song_id = %song_id, "now playing from fallback (shuffle/album loop)");
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
                    tracing::info!("queue empty and no fallback track, radio stopped");
                }
            }
        }
    }

    // Keep the visible shuffle lookahead topped up after consuming a track.
    if shuffle_enabled(db).await? {
        refill_shuffle_queue(db, admin_did).await?;
    }

    Ok(())
}

/// How many upcoming shuffle songs to keep visible in the queue.
const SHUFFLE_LOOKAHEAD: i64 = 50;

/// Reads the persisted station-wide shuffle flag.
async fn shuffle_enabled(db: &Database) -> anyhow::Result<bool> {
    sqlx::query_scalar::<_, bool>("select shuffle from radio_state where id = 1")
        .fetch_one(db.pool())
        .await
        .context("loading shuffle flag")
}

/// Shuffle respects the album rotation flags: a song is eligible when it sits
/// in at least one enabled album, or belongs to no album at all. Songs whose
/// every album is disabled stay out of shuffle, matching album-loop mode.
const SHUFFLE_ELIGIBLE: &str = r#"
    (
        not exists (
            select 1 from radio_album_tracks
            where radio_album_tracks.song_id = songs.id
        )
        or exists (
            select 1 from radio_album_tracks
            join radio_albums on radio_albums.id = radio_album_tracks.album_id
            where radio_album_tracks.song_id = songs.id
              and radio_albums.is_enabled = 1
        )
    )
"#;

/// Picks a random song for shuffle mode, avoiding an immediate repeat of the
/// current song when the library has more than one track.
async fn random_shuffle_song(db: &Database) -> anyhow::Result<Option<String>> {
    let avoiding_current = sqlx::query_scalar::<_, String>(&format!(
        r#"
        select id from songs
        where id != coalesce((select current_song_id from radio_state where id = 1), '')
          and {SHUFFLE_ELIGIBLE}
        order by random()
        limit 1
        "#,
    ))
    .fetch_optional(db.pool())
    .await
    .context("selecting random shuffle song")?;
    if avoiding_current.is_some() {
        return Ok(avoiding_current);
    }
    // Single eligible song: replaying the only track is the best we can do.
    sqlx::query_scalar::<_, String>(&format!(
        "select id from songs where {SHUFFLE_ELIGIBLE} order by random() limit 1",
    ))
    .fetch_optional(db.pool())
    .await
    .context("selecting any shuffle song")
}

/// Tops the queue up to `SHUFFLE_LOOKAHEAD` upcoming shuffle rows so the coming
/// random songs are visible in the queue. Skips songs already queued and the
/// current song to avoid near-term repeats.
async fn refill_shuffle_queue(db: &Database, queued_by_did: &str) -> anyhow::Result<()> {
    let existing = sqlx::query_scalar::<_, i64>("select count(*) from radio_queue where is_shuffle = 1")
        .fetch_one(db.pool())
        .await
        .context("counting shuffle queue rows")?;
    let need = SHUFFLE_LOOKAHEAD - existing;
    if need <= 0 {
        return Ok(());
    }

    let candidates = sqlx::query_scalar::<_, String>(&format!(
        r#"
        select id from songs
        where id not in (select song_id from radio_queue)
          and id != coalesce((select current_song_id from radio_state where id = 1), '')
          and {SHUFFLE_ELIGIBLE}
        order by random()
        limit ?
        "#,
    ))
    .bind(need)
    .fetch_all(db.pool())
    .await
    .context("selecting shuffle candidates")?;

    let mut position = sqlx::query_scalar::<_, Option<i64>>("select max(position) from radio_queue")
        .fetch_one(db.pool())
        .await
        .context("loading max queue position")?
        .unwrap_or(0);
    for song_id in candidates {
        position += 1;
        sqlx::query(
            "insert into radio_queue (id, song_id, position, queued_by_did, is_shuffle) values (?, ?, ?, ?, 1)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(&song_id)
        .bind(position)
        .bind(queued_by_did)
        .execute(db.pool())
        .await
        .context("inserting shuffle queue row")?;
    }
    Ok(())
}

/// Removes shuffle-filled rows from the queue, leaving manual entries intact.
async fn clear_shuffle_queue(db: &Database) -> anyhow::Result<()> {
    sqlx::query("delete from radio_queue where is_shuffle = 1")
        .execute(db.pool())
        .await
        .context("clearing shuffle queue rows")?;
    Ok(())
}

/// Toggles station-wide shuffle. Turning it on fills the queue with the shuffle
/// lookahead (and starts playback if nothing loadable is playing); turning it
/// off clears those auto-filled rows while leaving manual entries intact.
async fn toggle_shuffle(db: &Database, admin_did: &str) -> anyhow::Result<()> {
    let timestamp = now();
    let next = !shuffle_enabled(db).await?;
    sqlx::query(
        r#"
        update radio_state
        set shuffle = ?, updated_by_did = ?, updated_at = ?
        where id = 1
        "#,
    )
    .bind(next)
    .bind(admin_did)
    .bind(timestamp)
    .execute(db.pool())
    .await
    .context("toggling shuffle")?;

    if next {
        refill_shuffle_queue(db, admin_did).await?;
        let state = radio_state(db).await?;
        if !current_song_is_loadable(db, &state).await? {
            skip_to_next(db, admin_did).await?;
        }
    } else {
        clear_shuffle_queue(db).await?;
    }
    tracing::info!(shuffle = next, admin_did, "shuffle toggled");
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
    use super::*;
    use crate::db::Database;
    use crate::chat::ChatService;
    use tempfile::tempdir;

    async fn setup_test_service() -> (RadioService, Database) {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.prepare().await.unwrap();

        // Initialize required radio_state and radio_loop_state
        sqlx::query("insert or ignore into radio_state (id, status) values (1, 'stopped')")
            .execute(db.pool())
            .await
            .unwrap();
        sqlx::query("insert or ignore into radio_loop_state (id) values (1)")
            .execute(db.pool())
            .await
            .unwrap();

        let chat = ChatService::new(db.clone());
        let tmp = tempdir().unwrap();
        let service = RadioService::new(db.clone(), tmp.into_path(), chat);
        (service, db)
    }

    #[tokio::test]
    async fn test_shuffle_plays_random_and_avoids_repeat() {
        let (service, db) = setup_test_service().await;

        // Three songs with no album, so the album-loop fallback is empty and
        // only shuffle can keep the station playing.
        for i in 1..=3 {
            sqlx::query("insert into songs (id, title, artist, file_path, added_by_did, created_at) values (?, ?, ?, ?, ?, ?)")
                .bind(format!("s{i}"))
                .bind(format!("Title {i}"))
                .bind("Artist")
                .bind(format!("p{i}.mp3"))
                .bind("did")
                .bind(100 + i)
                .execute(db.pool())
                .await
                .unwrap();
        }

        // Shuffle off + empty queue + no albums => station stops.
        skip_to_next(&db, "did").await.unwrap();
        let state = radio_state(&db).await.unwrap();
        assert_eq!(state.status, "stopped");
        assert!(state.current_song_id.is_none());
        assert!(!state.shuffle);

        // Toggling shuffle on from idle should immediately start a random track
        // and fill the queue with the upcoming shuffle lookahead (all songs
        // except the one now playing, since the library only has three).
        service.control(RadioControlAction::Shuffle, "did").await.unwrap();
        let state = radio_state(&db).await.unwrap();
        assert!(state.shuffle);
        assert_eq!(state.status, "playing");
        let queue = queue_items(&db).await.unwrap();
        assert!(!queue.is_empty(), "shuffle should show upcoming songs in the queue");
        assert!(queue.iter().all(|item| item.is_shuffle), "auto-filled rows are marked shuffle");
        let mut prev = state.current_song_id.clone().expect("shuffle should start playback");

        // Advancing keeps playing and never immediately repeats the same song.
        for _ in 0..8 {
            skip_to_next(&db, "did").await.unwrap();
            let state = radio_state(&db).await.unwrap();
            assert_eq!(state.status, "playing");
            let next = state.current_song_id.clone().unwrap();
            assert_ne!(
                prev, next,
                "shuffle must not replay the current song back-to-back"
            );
            prev = next;
        }

        // Toggling shuffle off again clears the flag and the auto-filled rows.
        service.control(RadioControlAction::Shuffle, "did").await.unwrap();
        let state = radio_state(&db).await.unwrap();
        assert!(!state.shuffle);
        let queue = queue_items(&db).await.unwrap();
        assert!(queue.is_empty(), "disabling shuffle clears auto-filled rows");
    }

    #[tokio::test]
    async fn test_album_normalization() {
        assert_eq!(crate::radio::helpers::normalize_album_title("À cause des garçons"), "a cause des garcons");
        assert_eq!(crate::radio::helpers::normalize_album_title("À cause des garçons"), "a cause des garcons"); // NFD
        assert_eq!(crate::radio::helpers::normalize_album_title("Ægætis byrjun"), "ægætis byrjun");
        assert_eq!(crate::radio::helpers::normalize_album_title("  Ægætis byrjun   "), "ægætis byrjun");
    }

    #[tokio::test]
    async fn test_auto_sync_deduplication() {
        let (service, db) = setup_test_service().await;

        // Insert songs with conflicting metadata (casing/accents/NFC/NFD)
        // Note: they should converge on one album.
        sqlx::query("insert into songs (id, title, artist, album, file_path, added_by_did, created_at) values (?, ?, ?, ?, ?, ?, ?)")
            .bind("song-1")
            .bind("Track 1")
            .bind("Artist")
            .bind("À cause des garçons")
            .bind("path1.mp3")
            .bind("did")
            .bind(100)
            .execute(db.pool())
            .await
            .unwrap();

        sqlx::query("insert into songs (id, title, artist, album, file_path, added_by_did, created_at) values (?, ?, ?, ?, ?, ?, ?)")
            .bind("song-2")
            .bind("Track 2")
            .bind("Artist")
            .bind("À cause des garçons") // NFD spelling
            .bind("path2.mp3")
            .bind("did")
            .bind(101)
            .execute(db.pool())
            .await
            .unwrap();

        service.auto_sync_albums().await.unwrap();

        // Verify there is only one album record in the database
        let albums = service.albums().await.unwrap();
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].tracks.len(), 2);

        // Verify that the songs' album tags have been converged to the resolved title
        let resolved_title: String = sqlx::query_scalar("select album from songs where id = 'song-1'")
            .fetch_one(db.pool())
            .await
            .unwrap();
        let resolved_title_2: String = sqlx::query_scalar("select album from songs where id = 'song-2'")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(resolved_title, resolved_title_2);
    }

    #[tokio::test]
    async fn test_merge_albums() {
        let (service, db) = setup_test_service().await;

        // Create two duplicate albums in the database:
        // We do this by inserting songs with different album tags to force two separate albums,
        // then we merge them.
        sqlx::query("insert into songs (id, title, artist, album, file_path, added_by_did, created_at) values (?, ?, ?, ?, ?, ?, ?)")
            .bind("song-1")
            .bind("Track 1")
            .bind("Artist")
            .bind("Album A")
            .bind("path1.mp3")
            .bind("did")
            .bind(100)
            .execute(db.pool())
            .await
            .unwrap();

        sqlx::query("insert into songs (id, title, artist, album, file_path, added_by_did, created_at) values (?, ?, ?, ?, ?, ?, ?)")
            .bind("song-2")
            .bind("Track 2")
            .bind("Artist")
            .bind("Album B")
            .bind("path2.mp3")
            .bind("did")
            .bind(101)
            .execute(db.pool())
            .await
            .unwrap();

        service.auto_sync_albums().await.unwrap();

        let initial_albums = service.albums().await.unwrap();
        assert_eq!(initial_albums.len(), 2);

        let album_a = initial_albums.iter().find(|a| a.title == "Album A").unwrap();
        let album_b = initial_albums.iter().find(|a| a.title == "Album B").unwrap();

        // Point loop state cursor to Album B
        sqlx::query("update radio_loop_state set last_album_id = ?, last_track_position = 1 where id = 1")
            .bind(&album_b.id)
            .execute(db.pool())
            .await
            .unwrap();

        // Merge Album B into Album A
        let remaining_albums = service.merge_albums(&album_b.id, &album_a.id).await.unwrap();
        assert_eq!(remaining_albums.len(), 1);
        assert_eq!(remaining_albums[0].title, "Album A");
        assert_eq!(remaining_albums[0].tracks.len(), 2);

        // Verify loop state was updated to target album (Album A)
        let loop_cursor: (Option<String>, i64) = sqlx::query_as("select last_album_id, last_track_position from radio_loop_state where id = 1")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(loop_cursor.0.unwrap(), album_a.id);
        assert_eq!(loop_cursor.1, 1);
    }
}
