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

/// A song row loaded while scanning for duplicate uploads.
#[derive(FromRow)]
struct DuplicateSong {
    id: String,
    title: String,
    artist: String,
    album: Option<String>,
    file_path: String,
    cover_path: Option<String>,
}

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

    /// Removes duplicate song rows (same title/artist/album, uploaded more than
    /// once) keeping the oldest copy. Any queue entries or the current-song
    /// pointer that referenced a removed duplicate are repointed at the
    /// surviving song first, so playback and queue order are undisturbed.
    pub(crate) async fn cleanup_duplicate_songs_on_boot(&self) -> anyhow::Result<usize> {
        let songs = sqlx::query_as::<_, DuplicateSong>(
            r#"
            select id, title, artist, album, file_path, cover_path
            from songs
            order by created_at asc, id asc
            "#,
        )
        .fetch_all(self.db.pool())
        .await
        .context("listing songs for duplicate cleanup")?;

        let mut groups: std::collections::HashMap<
            (String, String, Option<String>),
            Vec<DuplicateSong>,
        > = std::collections::HashMap::new();
        for song in songs {
            let key = (
                song.title.trim().to_lowercase(),
                song.artist.trim().to_lowercase(),
                song.album
                    .as_deref()
                    .map(str::trim)
                    .filter(|album| !album.is_empty())
                    .map(str::to_lowercase),
            );
            groups.entry(key).or_default().push(song);
        }

        let mut removed = 0usize;
        for mut group in groups.into_values() {
            if group.len() < 2 {
                continue;
            }
            // Songs were loaded oldest-first, so the first entry is the
            // longest-standing copy and becomes the canonical survivor.
            let canonical = group.remove(0);
            for dupe in group {
                match self.remove_duplicate_song(&canonical.id, &dupe).await {
                    Ok(()) => removed += 1,
                    Err(error) => tracing::warn!(
                        %error,
                        duplicate_id = %dupe.id,
                        canonical_id = %canonical.id,
                        "failed to remove duplicate song"
                    ),
                }
            }
        }

        Ok(removed)
    }

    async fn remove_duplicate_song(
        &self,
        canonical_id: &str,
        dupe: &DuplicateSong,
    ) -> anyhow::Result<()> {
        let mut tx = self
            .db
            .pool()
            .begin()
            .await
            .context("starting duplicate-song removal transaction")?;

        sqlx::query("update radio_state set current_song_id = ? where current_song_id = ?")
            .bind(canonical_id)
            .bind(&dupe.id)
            .execute(&mut *tx)
            .await
            .context("repointing current song away from duplicate")?;

        sqlx::query("update radio_queue set song_id = ? where song_id = ?")
            .bind(canonical_id)
            .bind(&dupe.id)
            .execute(&mut *tx)
            .await
            .context("repointing queue entries away from duplicate")?;

        sqlx::query("delete from songs where id = ?")
            .bind(&dupe.id)
            .execute(&mut *tx)
            .await
            .context("deleting duplicate song row")?;

        tx.commit()
            .await
            .context("committing duplicate-song removal")?;

        if let Err(error) = tokio::fs::remove_file(&dupe.file_path).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(%error, path = %dupe.file_path, "failed to remove duplicate audio file");
            }
        }
        if let Some(cover_path) = &dupe.cover_path {
            let _ = tokio::fs::remove_file(cover_path).await;
        }
        let _ = tokio::fs::remove_file(self.thumb_dir.join(format!("{}.jpg", dupe.id))).await;

        tracing::info!(
            duplicate_id = %dupe.id,
            canonical_id,
            title = %dupe.title,
            artist = %dupe.artist,
            "removed duplicate song"
        );
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

    /// Lists albums derived from song metadata, grouping songs whose album
    /// tags match once case, accents, and whitespace are normalized.
    pub(crate) async fn albums(&self) -> anyhow::Result<Vec<RadioAlbum>> {
        grouped_albums(&self.db).await
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

    /// Clears the album tag from every song in a computed album group,
    /// ungrouping them back into standalone singles.
    pub(crate) async fn delete_album(&self, album_id: &str) -> anyhow::Result<Vec<RadioAlbum>> {
        let albums = self.albums().await?;
        let album = albums
            .iter()
            .find(|album| album.id == album_id)
            .ok_or_else(|| anyhow!("album not found"))?;

        let mut tx = self
            .db
            .pool()
            .begin()
            .await
            .context("starting delete_album transaction")?;

        for song in &album.tracks {
            sqlx::query("update songs set album = null where id = ?")
                .bind(&song.id)
                .execute(&mut *tx)
                .await
                .context("clearing album tag from song")?;
        }

        tx.commit()
            .await
            .context("committing delete_album transaction")?;

        tracing::info!(
            album_id,
            song_count = album.tracks.len(),
            "album ungrouped; song album tags cleared"
        );
        self.albums().await
    }

    /// Merges a source album group into a target group by rewriting every
    /// source-group song's album tag to match the target's resolved title.
    pub(crate) async fn merge_albums(
        &self,
        source_id: &str,
        target_id: &str,
    ) -> anyhow::Result<Vec<RadioAlbum>> {
        if source_id == target_id {
            return Err(anyhow!("cannot merge an album into itself"));
        }

        let albums = self.albums().await?;
        let source = albums
            .iter()
            .find(|album| album.id == source_id)
            .ok_or_else(|| anyhow!("source album not found"))?;
        let target = albums
            .iter()
            .find(|album| album.id == target_id)
            .ok_or_else(|| anyhow!("target album not found"))?;
        let target_title = target.title.clone();
        let song_ids: Vec<String> = source.tracks.iter().map(|song| song.id.clone()).collect();

        let mut tx = self
            .db
            .pool()
            .begin()
            .await
            .context("starting merge_albums transaction")?;

        for song_id in &song_ids {
            sqlx::query("update songs set album = ? where id = ?")
                .bind(&target_title)
                .bind(song_id)
                .execute(&mut *tx)
                .await
                .context("updating song album metadata for merge")?;
        }

        tx.commit()
            .await
            .context("committing merge_albums transaction")?;

        tracing::info!(
            source_id,
            target_id,
            song_count = song_ids.len(),
            "albums merged"
        );
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

/// Groups songs into ephemeral albums by normalized album title. There is no
/// persisted album table: this is recomputed from `songs` on every call, so
/// it always reflects current metadata and never needs syncing.
async fn grouped_albums(db: &Database) -> anyhow::Result<Vec<RadioAlbum>> {
    let songs = sqlx::query_as::<_, Song>(
        r#"
        select id, title, artist, album, genre, duration_seconds, mime_type,
            cover_path is not null as has_cover, added_by_did, created_at,
            loudness_lufs, loudness_peak
        from songs
        where album is not null and trim(album) != ''
        order by created_at asc
        "#,
    )
    .fetch_all(db.pool())
    .await
    .context("loading songs for album grouping")?;

    let mut order: Vec<String> = Vec::new();
    let mut groups: std::collections::HashMap<String, (Vec<String>, Vec<Song>)> =
        std::collections::HashMap::new();

    for song in songs {
        let album_name = song.album.clone().unwrap_or_default();
        let key = crate::radio::helpers::normalize_album_title(&album_name);
        let entry = groups.entry(key.clone()).or_insert_with(|| {
            order.push(key.clone());
            (Vec::new(), Vec::new())
        });
        entry.0.push(album_name);
        entry.1.push(song);
    }

    let mut albums = Vec::with_capacity(order.len());
    for key in order {
        let Some((titles, tracks)) = groups.remove(&key) else {
            continue;
        };
        albums.push(RadioAlbum {
            id: album_group_id(&key),
            title: RadioService::resolve_album_title_conflict(&titles),
            tracks,
        });
    }

    albums.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
    Ok(albums)
}

/// Derives a stable id for an album group from its normalized title, so
/// repeated requests within the same run address the same album.
fn album_group_id(normalized_key: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    normalized_key.hash(&mut hasher);
    format!("al:{:016x}", hasher.finish())
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

/// Picks a random song to autoqueue once the explicit queue runs dry. Excludes
/// the song that was just playing so a single-song library is the only case
/// that repeats immediately.
async fn random_next_song(
    db: &Database,
    exclude_song_id: Option<&str>,
) -> anyhow::Result<Option<Song>> {
    let song = sqlx::query_as::<_, Song>(
        r#"
        select id, title, artist, album, genre, duration_seconds, mime_type,
            cover_path is not null as has_cover, added_by_did, created_at,
            loudness_lufs, loudness_peak
        from songs
        where ?1 is null or id != ?1
        order by random()
        limit 1
        "#,
    )
    .bind(exclude_song_id)
    .fetch_optional(db.pool())
    .await
    .context("picking random autoqueue song")?;

    match song {
        Some(song) => Ok(Some(song)),
        // Only song in the library is the one we just excluded; play it again.
        None => sqlx::query_as::<_, Song>(
            r#"
            select id, title, artist, album, genre, duration_seconds, mime_type,
                cover_path is not null as has_cover, added_by_did, created_at,
                loudness_lufs, loudness_peak
            from songs
            order by random()
            limit 1
            "#,
        )
        .fetch_optional(db.pool())
        .await
        .context("picking random autoqueue song from full library"),
    }
}

async fn skip_to_next(db: &Database, admin_did: &str) -> anyhow::Result<()> {
    let previous_song_id = current_song_id(db).await?;
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
        None => match random_next_song(db, previous_song_id.as_deref()).await? {
            Some(song) => {
                sqlx::query(
                    r#"
                    update radio_state
                    set current_song_id = ?, status = 'playing', started_at = ?, paused_at = null,
                        position_seconds = 0, updated_by_did = ?, updated_at = ?
                    where id = 1
                    "#,
                )
                .bind(&song.id)
                .bind(timestamp)
                .bind(admin_did)
                .bind(timestamp)
                .execute(db.pool())
                .await
                .context("advancing radio state from random autoqueue")?;
                tracing::info!(
                    song_id = %song.id,
                    title = %song.title,
                    artist = %song.artist,
                    "now playing from random autoqueue"
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
                tracing::info!("queue empty and no songs available, radio stopped");
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
    use super::*;
    use crate::chat::ChatService;
    use crate::db::Database;
    use tempfile::tempdir;

    async fn setup_test_service() -> (RadioService, Database) {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.prepare().await.unwrap();

        // Initialize required radio_state row.
        sqlx::query("insert or ignore into radio_state (id, status) values (1, 'stopped')")
            .execute(db.pool())
            .await
            .unwrap();

        let chat = ChatService::new(db.clone());
        let tmp = tempdir().unwrap();
        let service = RadioService::new(db.clone(), tmp.into_path(), chat);
        (service, db)
    }

    async fn insert_song(
        db: &Database,
        id: &str,
        title: &str,
        artist: &str,
        album: Option<&str>,
        file_path: &str,
        created_at: i64,
    ) {
        sqlx::query("insert into songs (id, title, artist, album, file_path, added_by_did, created_at) values (?, ?, ?, ?, ?, ?, ?)")
            .bind(id)
            .bind(title)
            .bind(artist)
            .bind(album)
            .bind(file_path)
            .bind("did")
            .bind(created_at)
            .execute(db.pool())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_album_normalization() {
        assert_eq!(
            crate::radio::helpers::normalize_album_title("À cause des garçons"),
            "a cause des garcons"
        );
        assert_eq!(
            crate::radio::helpers::normalize_album_title("À cause des garçons"),
            "a cause des garcons"
        ); // NFD
        assert_eq!(
            crate::radio::helpers::normalize_album_title("Ægætis byrjun"),
            "ægætis byrjun"
        );
        assert_eq!(
            crate::radio::helpers::normalize_album_title("  Ægætis byrjun   "),
            "ægætis byrjun"
        );
    }

    #[tokio::test]
    async fn test_grouped_albums_deduplication() {
        let (service, db) = setup_test_service().await;

        // Insert songs with conflicting metadata (casing/whitespace).
        // They should be grouped into a single computed album.
        insert_song(
            &db,
            "song-1",
            "Track 1",
            "Artist",
            Some("Album Title"),
            "path1.mp3",
            100,
        )
        .await;
        insert_song(
            &db,
            "song-2",
            "Track 2",
            "Artist",
            Some("album title"),
            "path2.mp3",
            101,
        )
        .await;

        let albums = service.albums().await.unwrap();
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].tracks.len(), 2);
    }

    #[tokio::test]
    async fn test_grouped_albums_keeps_distinct_titles_separate() {
        let (service, db) = setup_test_service().await;

        insert_song(
            &db,
            "song-1",
            "Track 1",
            "Artist",
            Some("Album A"),
            "path1.mp3",
            100,
        )
        .await;
        insert_song(
            &db,
            "song-2",
            "Track 2",
            "Artist",
            Some("Album B"),
            "path2.mp3",
            101,
        )
        .await;

        let albums = service.albums().await.unwrap();
        assert_eq!(albums.len(), 2);
    }

    #[tokio::test]
    async fn test_delete_album_clears_song_tags() {
        let (service, db) = setup_test_service().await;

        insert_song(
            &db,
            "song-1",
            "Track 1",
            "Artist",
            Some("Album A"),
            "path1.mp3",
            100,
        )
        .await;
        insert_song(
            &db,
            "song-2",
            "Track 2",
            "Artist",
            Some("Album A"),
            "path2.mp3",
            101,
        )
        .await;

        let albums = service.albums().await.unwrap();
        assert_eq!(albums.len(), 1);
        let album_id = albums[0].id.clone();

        let remaining = service.delete_album(&album_id).await.unwrap();
        assert!(remaining.is_empty());

        let tags: Vec<Option<String>> = sqlx::query_scalar("select album from songs order by id")
            .fetch_all(db.pool())
            .await
            .unwrap();
        assert_eq!(tags, vec![None, None]);
    }

    #[tokio::test]
    async fn test_merge_albums_combines_song_tags() {
        let (service, db) = setup_test_service().await;

        insert_song(
            &db,
            "song-1",
            "Track 1",
            "Artist",
            Some("Album A"),
            "path1.mp3",
            100,
        )
        .await;
        insert_song(
            &db,
            "song-2",
            "Track 2",
            "Artist",
            Some("Album B"),
            "path2.mp3",
            101,
        )
        .await;

        let albums = service.albums().await.unwrap();
        assert_eq!(albums.len(), 2);
        let album_a = albums
            .iter()
            .find(|album| album.title == "Album A")
            .unwrap();
        let album_b = albums
            .iter()
            .find(|album| album.title == "Album B")
            .unwrap();

        let remaining = service
            .merge_albums(&album_b.id, &album_a.id)
            .await
            .unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].title, "Album A");
        assert_eq!(remaining[0].tracks.len(), 2);
    }

    #[tokio::test]
    async fn test_cleanup_duplicate_songs_keeps_oldest() {
        let (service, db) = setup_test_service().await;

        insert_song(
            &db,
            "song-old",
            "Same Track",
            "Same Artist",
            Some("Same Album"),
            "path1.mp3",
            100,
        )
        .await;
        insert_song(
            &db,
            "song-new",
            "same track",
            "same artist",
            Some("same album"),
            "path2.mp3",
            200,
        )
        .await;

        sqlx::query("update radio_state set current_song_id = 'song-new' where id = 1")
            .execute(db.pool())
            .await
            .unwrap();

        let removed = service.cleanup_duplicate_songs_on_boot().await.unwrap();
        assert_eq!(removed, 1);

        let remaining: Vec<String> = sqlx::query_scalar("select id from songs")
            .fetch_all(db.pool())
            .await
            .unwrap();
        assert_eq!(remaining, vec!["song-old".to_string()]);

        let current_song_id: Option<String> =
            sqlx::query_scalar("select current_song_id from radio_state where id = 1")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(current_song_id.as_deref(), Some("song-old"));
    }

    #[tokio::test]
    async fn test_random_autoqueue_when_queue_empty() {
        let (_service, db) = setup_test_service().await;

        insert_song(&db, "song-1", "Track 1", "Artist", None, "path1.mp3", 100).await;
        insert_song(&db, "song-2", "Track 2", "Artist", None, "path2.mp3", 101).await;

        skip_to_next(&db, "did").await.unwrap();
        let current_song_id: Option<String> =
            sqlx::query_scalar("select current_song_id from radio_state where id = 1")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert!(matches!(
            current_song_id.as_deref(),
            Some("song-1") | Some("song-2")
        ));
    }
}
