use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use anyhow::{Context, anyhow};
use sqlx::FromRow;
use tokio::sync::broadcast;
use uuid::Uuid;

use super::events::advance_duration_seconds;
use super::helpers::{
    extension, file_extension, is_unsupported_audio_file as is_unsupported_by_ext, now,
};
use super::types::*;
use crate::{chat::ChatService, db::Database, loudness, metadata, tempo};

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
        let all_db_albums =
            sqlx::query_as::<_, ExistingAlbum>("select id, title from radio_albums")
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

    /// Estimates and stores tempo for every song missing a `bpm` value.
    /// Unconfident tracks are stored as 0 so they are not rescanned each boot.
    pub(crate) async fn backfill_missing_bpm_on_boot(&self) -> anyhow::Result<usize> {
        #[derive(FromRow)]
        struct MissingBpm {
            id: String,
            file_path: String,
        }

        let missing = sqlx::query_as::<_, MissingBpm>(
            r#"
            select id, file_path
            from songs
            where bpm is null
            "#,
        )
        .fetch_all(self.db.pool())
        .await
        .context("listing songs missing bpm")?;

        let total = missing.len();
        if total > 0 {
            tracing::info!(total, "estimating tempo for songs missing values");
        }

        let mut updated = 0usize;
        for (index, song) in missing.into_iter().enumerate() {
            if store_bpm(&self.db, &song.id, PathBuf::from(song.file_path)).await {
                updated += 1;
                tracing::debug!(
                    song_id = %song.id,
                    progress = format_args!("{}/{}", index + 1, total),
                    "tempo estimated"
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
            select id, name, created_at, shuffle_on_load
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

    /// Renames a playlist.
    pub(crate) async fn rename_playlist(&self, id: &str, name: &str) -> anyhow::Result<Playlist> {
        let name = name.trim();
        if name.is_empty() {
            return Err(anyhow!("playlist name is required"));
        }
        let result = sqlx::query("update playlists set name = ? where id = ?")
            .bind(name)
            .bind(id)
            .execute(self.db.pool())
            .await
            .context("renaming playlist")?;
        if result.rows_affected() == 0 {
            return Err(anyhow!("playlist not found"));
        }
        self.find_playlist(id).await
    }

    /// Toggles whether loading this playlist randomizes its order.
    pub(crate) async fn set_playlist_shuffle_on_load(
        &self,
        id: &str,
        shuffle_on_load: bool,
    ) -> anyhow::Result<Playlist> {
        let result = sqlx::query("update playlists set shuffle_on_load = ? where id = ?")
            .bind(shuffle_on_load)
            .bind(id)
            .execute(self.db.pool())
            .await
            .context("updating playlist shuffle_on_load")?;
        if result.rows_affected() == 0 {
            return Err(anyhow!("playlist not found"));
        }
        self.find_playlist(id).await
    }

    /// Appends songs to the end of a playlist.
    pub(crate) async fn append_playlist_tracks(
        &self,
        id: &str,
        song_ids: &[String],
    ) -> anyhow::Result<Playlist> {
        let song_ids: Vec<String> = song_ids
            .iter()
            .map(|song_id| song_id.trim().to_owned())
            .filter(|song_id| !song_id.is_empty())
            .collect();
        if song_ids.is_empty() {
            return Err(anyhow!("no songs to add"));
        }
        for song_id in &song_ids {
            if find_song(&self.db, song_id).await?.is_none() {
                return Err(anyhow!("song not found: {song_id}"));
            }
        }

        let mut tx = self
            .db
            .pool()
            .begin()
            .await
            .context("starting playlist append transaction")?;

        let base = sqlx::query_scalar::<_, Option<i64>>(
            "select max(position) from playlist_tracks where playlist_id = ?",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .context("loading max playlist position")?
        .unwrap_or(0);

        for (index, song_id) in song_ids.iter().enumerate() {
            sqlx::query(
                "insert into playlist_tracks (playlist_id, song_id, position) values (?, ?, ?)",
            )
            .bind(id)
            .bind(song_id)
            .bind(base + index as i64 + 1)
            .execute(&mut *tx)
            .await
            .context("appending playlist track")?;
        }

        tx.commit().await.context("committing playlist append")?;
        self.find_playlist(id).await
    }

    /// Drops the track at a one-based position and closes the gap.
    ///
    /// `playlist_tracks` is keyed on `(playlist_id, position)`, so shifting the
    /// survivors in place would collide with rows that haven't moved yet. Every
    /// reshuffle therefore clears the playlist's rows and reinserts them.
    pub(crate) async fn remove_playlist_track(
        &self,
        id: &str,
        position: i64,
    ) -> anyhow::Result<Playlist> {
        let mut song_ids = sqlx::query_scalar::<_, String>(
            "select song_id from playlist_tracks where playlist_id = ? order by position asc",
        )
        .bind(id)
        .fetch_all(self.db.pool())
        .await
        .context("loading playlist tracks")?;

        let index = usize::try_from(position - 1).map_err(|_| anyhow!("invalid position"))?;
        if index >= song_ids.len() {
            return Err(anyhow!("no track at position {position}"));
        }
        song_ids.remove(index);
        self.rewrite_playlist_tracks(id, &song_ids).await?;
        self.find_playlist(id).await
    }

    /// Replaces a playlist's track order wholesale.
    pub(crate) async fn reorder_playlist_tracks(
        &self,
        id: &str,
        song_ids: &[String],
    ) -> anyhow::Result<Playlist> {
        let existing = sqlx::query_scalar::<_, String>(
            "select song_id from playlist_tracks where playlist_id = ?",
        )
        .bind(id)
        .fetch_all(self.db.pool())
        .await
        .context("loading playlist tracks")?;

        // A reorder must be a permutation: anything else would silently add or
        // drop tracks under the guise of moving them.
        let mut before: Vec<&String> = existing.iter().collect();
        let mut after: Vec<&String> = song_ids.iter().collect();
        before.sort();
        after.sort();
        if before != after {
            return Err(anyhow!("reorder must list exactly the playlist's tracks"));
        }

        self.rewrite_playlist_tracks(id, song_ids).await?;
        self.find_playlist(id).await
    }

    /// Rewrites a set's stored order into a well-sequenced one.
    pub(crate) async fn sequence_playlist_tracks(&self, id: &str) -> anyhow::Result<Playlist> {
        let song_ids = sqlx::query_scalar::<_, String>(
            "select song_id from playlist_tracks where playlist_id = ? order by position asc",
        )
        .bind(id)
        .fetch_all(self.db.pool())
        .await
        .context("loading playlist tracks")?;

        if song_ids.len() > 1 {
            let ordered = sequence_songs(&self.db, &song_ids).await?;
            self.rewrite_playlist_tracks(id, &ordered).await?;
        }
        self.find_playlist(id).await
    }

    /// Copies a playlist, tracks and all, under a new name.
    pub(crate) async fn duplicate_playlist(
        &self,
        id: &str,
        name: &str,
    ) -> anyhow::Result<Playlist> {
        let song_ids = sqlx::query_scalar::<_, String>(
            "select song_id from playlist_tracks where playlist_id = ? order by position asc",
        )
        .bind(id)
        .fetch_all(self.db.pool())
        .await
        .context("loading playlist tracks")?;

        if song_ids.is_empty() {
            return Err(anyhow!("playlist is empty or not found"));
        }
        self.create_playlist(name, &song_ids).await
    }

    /// Rewrites the whole `playlist_tracks` block for one playlist in order.
    async fn rewrite_playlist_tracks(&self, id: &str, song_ids: &[String]) -> anyhow::Result<()> {
        let mut tx = self
            .db
            .pool()
            .begin()
            .await
            .context("starting playlist rewrite transaction")?;

        sqlx::query("delete from playlist_tracks where playlist_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .context("clearing playlist tracks")?;

        for (index, song_id) in song_ids.iter().enumerate() {
            sqlx::query(
                "insert into playlist_tracks (playlist_id, song_id, position) values (?, ?, ?)",
            )
            .bind(id)
            .bind(song_id)
            .bind(index as i64 + 1)
            .execute(&mut *tx)
            .await
            .context("reinserting playlist track")?;
        }

        tx.commit().await.context("committing playlist rewrite")
    }

    async fn find_playlist(&self, id: &str) -> anyhow::Result<Playlist> {
        self.playlists()
            .await?
            .into_iter()
            .find(|playlist| playlist.id == id)
            .ok_or_else(|| anyhow!("playlist not found"))
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
    ///
    /// `shuffle` overrides the playlist's stored `shuffle_on_load`; passing
    /// `None` honours whatever the set was saved with.
    pub(crate) async fn load_playlist(
        &self,
        id: &str,
        replace: bool,
        shuffle: Option<bool>,
        admin_did: &str,
    ) -> anyhow::Result<RadioSnapshot> {
        let mut song_ids = sqlx::query_scalar::<_, String>(
            "select song_id from playlist_tracks where playlist_id = ? order by position asc",
        )
        .bind(id)
        .fetch_all(self.db.pool())
        .await
        .context("loading playlist track IDs")?;

        if song_ids.is_empty() {
            return Err(anyhow!("playlist is empty or not found"));
        }

        let shuffle = match shuffle {
            Some(explicit) => explicit,
            None => {
                sqlx::query_scalar::<_, bool>("select shuffle_on_load from playlists where id = ?")
                    .bind(id)
                    .fetch_optional(self.db.pool())
                    .await
                    .context("loading playlist shuffle_on_load")?
                    .unwrap_or(false)
            }
        };
        if shuffle {
            song_ids = sequence_songs(&self.db, &song_ids).await?;
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
    pub(crate) async fn set_album_weight(
        &self,
        album_id: &str,
        weight: i64,
    ) -> anyhow::Result<Vec<RadioAlbum>> {
        let weight = weight.clamp(1, 4);
        sqlx::query("update radio_albums set rotation_weight = ? where id = ?")
            .bind(weight)
            .bind(album_id)
            .execute(self.db.pool())
            .await
            .context("setting album rotation weight")?;
        self.albums().await
    }

    /// Rotation metadata for the admin UI: per-album weights and the airlog.
    pub(crate) async fn rotation_info(&self) -> anyhow::Result<crate::radio::RotationInfo> {
        let weights =
            sqlx::query_as::<_, (String, i64)>("select id, rotation_weight from radio_albums")
                .fetch_all(self.db.pool())
                .await
                .context("loading rotation weights")?;
        let recently_played = sqlx::query_as::<_, crate::radio::PlayHistoryItem>(
            r#"
            select song_id, title, artist, started_at
            from play_history
            order by started_at desc
            limit 10
            "#,
        )
        .fetch_all(self.db.pool())
        .await
        .context("loading play history")?;
        // Peek the deterministic next loop track — only meaningful when
        // shuffle is off (shuffle's picks already appear in the queue).
        let up_next = if shuffle_enabled(&self.db).await? {
            None
        } else {
            match next_album_loop_track(&self.db).await? {
                Some(track) => {
                    let song = find_song(&self.db, &track.song_id).await?;
                    let source = if track.album_id == SINGLES_ALBUM_ID {
                        "singles".to_string()
                    } else {
                        sqlx::query_scalar::<_, String>(
                            "select title from radio_albums where id = ?",
                        )
                        .bind(&track.album_id)
                        .fetch_optional(self.db.pool())
                        .await
                        .context("loading rotation album title")?
                        .unwrap_or_else(|| "album loop".to_string())
                    };
                    song.map(|song| crate::radio::RotationUpNext {
                        song_id: song.id,
                        title: song.title,
                        artist: song.artist,
                        source,
                    })
                }
                None => None,
            }
        };
        Ok(crate::radio::RotationInfo {
            weights: weights.into_iter().collect(),
            recently_played,
            up_next,
        })
    }

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
            id.clone(),
            PathBuf::from(file_path_string.clone()),
        ));
        tokio::spawn(measure_and_store_bpm(
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
        self.enqueue_songs_at(song_ids, admin_did, false, false)
            .await
    }

    /// Reorders the pending queue into a well-sequenced set.
    pub(crate) async fn sequence_queue(&self) -> anyhow::Result<RadioSnapshot> {
        let rows = sqlx::query_as::<_, (String, String)>(
            r#"
            select id, song_id from radio_queue
            order by is_shuffle asc, position asc, created_at asc
            "#,
        )
        .fetch_all(self.db.pool())
        .await
        .context("loading queue for sequencing")?;

        if rows.len() < 2 {
            return self.snapshot().await;
        }

        // Sequence by song, then map back to the queue rows. A song queued
        // twice keeps one row per copy.
        let song_ids: Vec<String> = rows.iter().map(|(_, song_id)| song_id.clone()).collect();
        let ordered = sequence_songs(&self.db, &song_ids).await?;

        let mut by_song: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (queue_id, song_id) in &rows {
            by_song
                .entry(song_id.clone())
                .or_default()
                .push(queue_id.clone());
        }
        let mut queue_ids = Vec::with_capacity(rows.len());
        for song_id in &ordered {
            if let Some(pending) = by_song.get_mut(song_id) {
                if !pending.is_empty() {
                    queue_ids.push(pending.remove(0));
                }
            }
        }

        self.reorder_queue(&queue_ids).await
    }

    /// Adds songs to the queue, either appended or jumped to the front.
    ///
    /// Queue rows sort by `position`, which is a free integer rather than a
    /// dense index, so "play next" needs no shuffling of existing rows: the new
    /// items simply take positions below the current minimum. Manual rows
    /// always sort ahead of shuffle-filled ones regardless.
    pub(crate) async fn enqueue_songs_at(
        &self,
        song_ids: &[String],
        admin_did: &str,
        at_top: bool,
        sequence: bool,
    ) -> anyhow::Result<RadioSnapshot> {
        for song_id in song_ids {
            if find_song(&self.db, song_id).await?.is_none() {
                return Err(anyhow!("song not found: {song_id}"));
            }
        }

        // "Shuffle these in" means sequence them, not randomize them: the same
        // transition scoring the station uses on itself.
        let sequenced;
        let song_ids = if sequence {
            sequenced = sequence_songs(&self.db, song_ids).await?;
            &sequenced[..]
        } else {
            song_ids
        };

        let base_position = if at_top {
            sqlx::query_scalar::<_, Option<i64>>(
                "select min(position) from radio_queue where is_shuffle = 0",
            )
            .fetch_one(self.db.pool())
            .await
            .context("loading min queue position")?
            .unwrap_or(1)
                - song_ids.len() as i64
                - 1
        } else {
            sqlx::query_scalar::<_, Option<i64>>("select max(position) from radio_queue")
                .fetch_one(self.db.pool())
                .await
                .context("loading max queue position")?
                .unwrap_or(0)
        };

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
            RadioControlAction::SetLoopMode(_) => "setLoopMode",
            RadioControlAction::SetLoopPlaylist(_) => "setLoopPlaylist",
        };
        match &action {
            RadioControlAction::Play => play_or_resume(&self.db, admin_did).await?,
            RadioControlAction::Pause => set_status(&self.db, "paused", admin_did).await?,
            RadioControlAction::Stop => set_status(&self.db, "stopped", admin_did).await?,
            RadioControlAction::Skip => skip_to_next(&self.db, admin_did).await?,
            RadioControlAction::Previous => reset_current(&self.db, admin_did).await?,
            RadioControlAction::Shuffle => toggle_shuffle(&self.db, admin_did).await?,
            RadioControlAction::SetLoopMode(mode) => {
                set_loop_mode(&self.db, LoopMode::parse(mode)?, admin_did).await?
            }
            RadioControlAction::SetLoopPlaylist(playlist_id) => {
                set_loop_playlist(&self.db, playlist_id.as_deref(), admin_did).await?
            }
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
        select current_song_id, status, started_at, paused_at, position_seconds, updated_by_did,
            shuffle, loop_mode, loop_playlist_id
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
                    // Natural advancement changes the snapshot just like an
                    // admin skip does; without this broadcast, pages fed by
                    // the websocket keep showing the previous song.
                    match snapshot_from_db(&db).await {
                        Ok(snapshot) => {
                            let _ = events.send(RadioEvent::SnapshotChanged { snapshot });
                        }
                        Err(error) => tracing::error!(?error, "failed to broadcast advanced snapshot"),
                    }
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
        select current_song_id, status, started_at, paused_at, position_seconds, updated_by_did,
            shuffle, loop_mode, loop_playlist_id
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
            select current_song_id, status, started_at, paused_at, position_seconds, updated_by_did,
                shuffle, loop_mode, loop_playlist_id
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

async fn measure_and_store_bpm(db: Database, song_id: String, file_path: PathBuf) {
    store_bpm(&db, &song_id, file_path).await;
}

/// Estimates and persists a song's tempo. Tracks without a confident pulse
/// are stored as 0 ("analyzed, no tempo") so boot backfills skip them; the
/// shuffle scoring treats non-positive values as unknown.
async fn store_bpm(db: &Database, song_id: &str, file_path: PathBuf) -> bool {
    let started = std::time::Instant::now();
    let estimate = match tempo::measure(&file_path).await {
        Ok(estimate) => estimate,
        Err(error) => {
            tracing::warn!(%error, song_id, "failed to estimate tempo");
            return false;
        }
    };
    let stored = estimate.unwrap_or(0.0);
    let result = sqlx::query("update songs set bpm = ? where id = ?")
        .bind(stored)
        .bind(song_id)
        .execute(db.pool())
        .await;
    match result {
        Ok(_) => {
            tracing::info!(
                song_id,
                bpm = stored,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "estimated tempo"
            );
            true
        }
        Err(error) => {
            tracing::warn!(%error, song_id, "failed to persist tempo");
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

    // Loop order: every enabled album in position order, then the loose
    // singles (songs in no album), then wrap back to the first album.
    if cursor.0.as_deref() == Some(SINGLES_ALBUM_ID) {
        if let Some(track) = next_single_track(db, Some(cursor.1)).await? {
            return Ok(Some(track));
        }
        // Singles exhausted: wrap to the first enabled album, or back to the
        // first single when no album is in rotation.
        if let Some(track) = first_album_track(db).await? {
            return Ok(Some(track));
        }
        return next_single_track(db, None).await;
    }

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

    if let Some(track) = next {
        return Ok(Some(track));
    }
    // End of the albums (or no cursor yet): play the singles next, falling
    // back to the first enabled album track when there are none.
    if cursor.0.is_some() {
        if let Some(track) = next_single_track(db, None).await? {
            return Ok(Some(track));
        }
    }
    if let Some(track) = first_album_track(db).await? {
        return Ok(Some(track));
    }
    next_single_track(db, None).await
}

/// First track of the first enabled album, for wrapping the loop.
async fn first_album_track(db: &Database) -> anyhow::Result<Option<LoopTrack>> {
    sqlx::query_as::<_, LoopTrack>(
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
    .context("loading first album loop track")
}

/// Next loose single after the given rowid cursor (or the first one). The
/// cursor reuses the loop table's track_position column to store the rowid.
async fn next_single_track(
    db: &Database,
    after_rowid: Option<i64>,
) -> anyhow::Result<Option<LoopTrack>> {
    sqlx::query_as::<_, LoopTrack>(&format!(
        r#"
        select '{SINGLES_ALBUM_ID}' as album_id,
            songs.rowid as track_position,
            songs.id as song_id
        from songs
        where songs.rowid > ?
          and not exists (
            select 1 from radio_album_tracks where radio_album_tracks.song_id = songs.id
          )
        order by songs.rowid asc
        limit 1
        "#,
    ))
    .bind(after_rowid.unwrap_or(-1))
    .fetch_optional(db.pool())
    .await
    .context("loading next single for rotation")
}

/// Re-queues a pinned playlist so a looping set survives the queue draining.
///
/// Clears the pin rather than erroring if the playlist has since been deleted
/// or emptied — a dangling pin would otherwise stall the station on every
/// advance.
async fn reload_loop_playlist(
    db: &Database,
    playlist_id: &str,
    admin_did: &str,
) -> anyhow::Result<()> {
    let mut song_ids = sqlx::query_scalar::<_, String>(
        "select song_id from playlist_tracks where playlist_id = ? order by position asc",
    )
    .bind(playlist_id)
    .fetch_all(db.pool())
    .await
    .context("loading looped playlist tracks")?;

    if song_ids.is_empty() {
        sqlx::query("update radio_state set loop_playlist_id = null where id = 1")
            .execute(db.pool())
            .await
            .context("clearing dangling loop playlist")?;
        tracing::warn!(playlist_id, "loop playlist is empty or gone; unpinned it");
        return Ok(());
    }

    let shuffle =
        sqlx::query_scalar::<_, bool>("select shuffle_on_load from playlists where id = ?")
            .bind(playlist_id)
            .fetch_optional(db.pool())
            .await
            .context("loading looped playlist shuffle_on_load")?
            .unwrap_or(false);
    if shuffle {
        song_ids = sequence_songs(db, &song_ids).await?;
    }

    let base = sqlx::query_scalar::<_, Option<i64>>("select max(position) from radio_queue")
        .fetch_one(db.pool())
        .await
        .context("loading queue tail for playlist loop")?
        .unwrap_or(0);

    for (index, song_id) in song_ids.iter().enumerate() {
        sqlx::query(
            "insert into radio_queue (id, song_id, position, queued_by_did) values (?, ?, ?, ?)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(song_id)
        .bind(base + index as i64 + 1)
        .bind(admin_did)
        .execute(db.pool())
        .await
        .context("re-queueing looped playlist track")?;
    }

    tracing::info!(
        playlist_id,
        count = song_ids.len(),
        "reloaded loop playlist"
    );
    Ok(())
}

async fn skip_to_next(db: &Database, admin_did: &str) -> anyhow::Result<()> {
    let mode = loop_mode(db).await?;

    // Repeat-one never consumes anything: restart whatever is playing and
    // leave the queue exactly as it is.
    if mode == LoopMode::One {
        let state = radio_state(db).await?;
        if let Some(song_id) = state.current_song_id.as_deref() {
            let timestamp = now();
            sqlx::query(
                r#"
                update radio_state
                set status = 'playing', started_at = ?, paused_at = null,
                    position_seconds = 0, updated_by_did = ?, updated_at = ?
                where id = 1
                "#,
            )
            .bind(timestamp)
            .bind(admin_did)
            .bind(timestamp)
            .execute(db.pool())
            .await
            .context("restarting looped track")?;
            record_play(db, song_id).await;
            return Ok(());
        }
    }

    // Loop-queue with nothing left to play still needs the pinned set (or the
    // rotation fallbacks) to refill it, so only reload when the queue is dry.
    if let Some(playlist_id) = loop_playlist_id(db).await? {
        let pending = sqlx::query_scalar::<_, i64>("select count(*) from radio_queue")
            .fetch_one(db.pool())
            .await
            .context("counting queue before playlist loop")?;
        if pending == 0 {
            reload_loop_playlist(db, &playlist_id, admin_did).await?;
        }
    }

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
            // In loop-queue mode a manually queued track is recycled to the
            // back rather than dropped, so the set keeps cycling. Auto-filled
            // shuffle rows are always consumed — they get replaced anyway.
            if mode == LoopMode::Queue && !item.is_shuffle {
                let tail =
                    sqlx::query_scalar::<_, Option<i64>>("select max(position) from radio_queue")
                        .fetch_one(db.pool())
                        .await
                        .context("loading queue tail for loop")?
                        .unwrap_or(0);
                sqlx::query("update radio_queue set position = ? where id = ?")
                    .bind(tail + 1)
                    .bind(&item.id)
                    .execute(db.pool())
                    .await
                    .context("recycling looped queue item")?;
            } else {
                sqlx::query("delete from radio_queue where id = ?")
                    .bind(&item.id)
                    .execute(db.pool())
                    .await
                    .context("removing skipped queue item")?;
            }
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
            record_play(db, &item.song_id).await;
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
                    record_play(db, &song_id).await;
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

/// How finished queue tracks are recycled.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LoopMode {
    /// Played tracks leave the queue — the original behaviour.
    Off,
    /// The current track restarts instead of advancing.
    One,
    /// Finished tracks go to the back of the queue, so it never drains.
    Queue,
}

impl LoopMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            LoopMode::Off => "off",
            LoopMode::One => "one",
            LoopMode::Queue => "queue",
        }
    }

    pub(crate) fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "off" => Ok(LoopMode::Off),
            "one" => Ok(LoopMode::One),
            "queue" => Ok(LoopMode::Queue),
            other => Err(anyhow!("unknown loop mode: {other}")),
        }
    }
}

/// Reads the persisted loop mode, tolerating an unrecognised stored value.
async fn loop_mode(db: &Database) -> anyhow::Result<LoopMode> {
    let raw = sqlx::query_scalar::<_, String>("select loop_mode from radio_state where id = 1")
        .fetch_one(db.pool())
        .await
        .context("loading loop mode")?;
    Ok(LoopMode::parse(&raw).unwrap_or(LoopMode::Off))
}

/// Reads the playlist that reloads whenever the queue drains, if any.
async fn loop_playlist_id(db: &Database) -> anyhow::Result<Option<String>> {
    sqlx::query_scalar::<_, Option<String>>("select loop_playlist_id from radio_state where id = 1")
        .fetch_one(db.pool())
        .await
        .context("loading loop playlist")
}

async fn set_loop_mode(db: &Database, mode: LoopMode, admin_did: &str) -> anyhow::Result<()> {
    let timestamp = now();
    sqlx::query(
        "update radio_state set loop_mode = ?, updated_by_did = ?, updated_at = ? where id = 1",
    )
    .bind(mode.as_str())
    .bind(admin_did)
    .bind(timestamp)
    .execute(db.pool())
    .await
    .context("setting loop mode")?;
    Ok(())
}

/// Pins the playlist that reloads when the queue drains, or clears the pin.
async fn set_loop_playlist(
    db: &Database,
    playlist_id: Option<&str>,
    admin_did: &str,
) -> anyhow::Result<()> {
    if let Some(id) = playlist_id {
        let exists = sqlx::query_scalar::<_, i64>("select count(*) from playlists where id = ?")
            .bind(id)
            .fetch_one(db.pool())
            .await
            .context("checking loop playlist")?;
        if exists == 0 {
            return Err(anyhow!("playlist not found"));
        }
    }

    let timestamp = now();
    sqlx::query(
        "update radio_state set loop_playlist_id = ?, updated_by_did = ?, updated_at = ? where id = 1",
    )
    .bind(playlist_id)
    .bind(admin_did)
    .bind(timestamp)
    .execute(db.pool())
    .await
    .context("setting loop playlist")?;
    Ok(())
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

/// Pseudo-album id the loop cursor uses while stepping through songs that
/// belong to no album. Singles are always part of rotation: they play after
/// the enabled albums in loop mode and are always eligible for shuffle.
const SINGLES_ALBUM_ID: &str = "__singles__";

/// Seconds shuffle avoids repeating a song for (classic radio separation).
const SHUFFLE_SEPARATION_SECS: i64 = 2 * 60 * 60;

/// Records a song starting on air, for the airlog and shuffle separation.
async fn record_play(db: &Database, song_id: &str) {
    let result = sqlx::query(
        r#"
        insert into play_history (song_id, title, artist, started_at)
        select id, title, artist, ? from songs where id = ?
        "#,
    )
    .bind(now())
    .bind(song_id)
    .execute(db.pool())
    .await;
    if let Err(error) = result {
        tracing::error!(?error, song_id, "failed to record play history");
    }
}

/// Cheap xorshift; shuffle picks don't need cryptographic randomness.
fn next_pseudo_random(seed: &mut u64) -> f64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    ((*seed >> 11) as f64) / ((1u64 << 53) as f64)
}

fn random_seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9e3779b97f4a7c15)
        | 1
}

/// Picks one candidate index at random, proportionally to `scores`.
fn weighted_pick(scores: &[f64], seed: &mut u64) -> Option<usize> {
    let total: f64 = scores.iter().map(|score| score.max(0.0)).sum();
    if !(total > 0.0) {
        return None;
    }
    let mut roll = next_pseudo_random(seed) * total;
    for (index, score) in scores.iter().enumerate() {
        roll -= score.max(0.0);
        if roll < 0.0 {
            return Some(index);
        }
    }
    Some(scores.len() - 1)
}

/// The song fields transition scoring compares between neighbors.
#[derive(Clone, FromRow)]
struct ShuffleCandidate {
    id: String,
    artist: String,
    album: Option<String>,
    genre: Option<String>,
    duration_seconds: Option<i64>,
    loudness_lufs: Option<f64>,
    bpm: Option<f64>,
    weight: i64,
}

/// How strongly recently-aired artists repel their own songs; index 0 is the
/// artist heard most recently. Beyond the table the artist is fair game.
const ARTIST_SEPARATION: [f64; 3] = [0.15, 0.4, 0.65];

fn same_text(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim(), b.trim());
    !a.is_empty() && a.eq_ignore_ascii_case(b)
}

/// Scores how well `candidate` would follow `previous` on air. The base is
/// the album rotation weight; every sequencing rule is a multiplier, so a
/// poor transition is discouraged, never forbidden — a small library must
/// still play something.
fn transition_score(
    candidate: &ShuffleCandidate,
    previous: &ShuffleCandidate,
    recent_artists: &[String],
) -> f64 {
    let mut score = candidate.weight.max(1) as f64;

    // Artist separation, strongest against the artist just heard.
    if let Some(position) = recent_artists
        .iter()
        .position(|artist| same_text(artist, &candidate.artist))
    {
        score *= ARTIST_SEPARATION.get(position).copied().unwrap_or(0.85);
    }

    // Album separation.
    if let (Some(prev), Some(cand)) = (&previous.album, &candidate.album) {
        if same_text(prev, cand) {
            score *= 0.3;
        }
    }

    // Energy smoothing: LUFS is a serviceable energy proxy, so prefer steps
    // over cliffs between neighboring songs.
    if let (Some(prev), Some(cand)) = (previous.loudness_lufs, candidate.loudness_lufs) {
        let delta = (prev - cand).abs();
        score *= if delta <= 3.0 {
            1.0
        } else if delta <= 6.0 {
            0.7
        } else {
            0.45
        };
    }

    // Genre adjacency: a mild pull toward same-genre sets.
    if let (Some(prev), Some(cand)) = (&previous.genre, &candidate.genre) {
        if same_text(prev, cand) {
            score *= 1.6;
        }
    }

    // Duration pacing: don't stack epics back to back.
    if previous.duration_seconds.unwrap_or(0) > 360 && candidate.duration_seconds.unwrap_or(0) > 360
    {
        score *= 0.6;
    }

    // Tempo pairing. 0 means "analyzed, no confident pulse". Half and double
    // time count as neighbors — 140 into 70 is a natural transition.
    let previous_bpm = previous.bpm.filter(|bpm| *bpm > 0.0);
    let candidate_bpm = candidate.bpm.filter(|bpm| *bpm > 0.0);
    if let (Some(prev), Some(cand)) = (previous_bpm, candidate_bpm) {
        let distance = [0.5, 1.0, 2.0]
            .iter()
            .map(|multiple| (cand - prev * multiple).abs())
            .fold(f64::INFINITY, f64::min);
        score *= if distance <= 6.0 {
            1.5
        } else if distance <= 16.0 {
            1.1
        } else {
            0.75
        };
    }

    score
}

/// Loads the transition fields for one song (the rotation weight is unused
/// on the `previous` side of a comparison and loads as a placeholder).
async fn shuffle_profile(db: &Database, song_id: &str) -> anyhow::Result<Option<ShuffleCandidate>> {
    sqlx::query_as::<_, ShuffleCandidate>(
        r#"
        select songs.id, songs.artist, songs.album, songs.genre,
            songs.duration_seconds, songs.loudness_lufs, songs.bpm,
            2 as weight
        from songs
        where songs.id = ?
        "#,
    )
    .bind(song_id)
    .fetch_optional(db.pool())
    .await
    .context("loading shuffle transition profile")
}

/// Artists aired most recently, newest first.
async fn recently_aired_artists(db: &Database, limit: i64) -> anyhow::Result<Vec<String>> {
    sqlx::query_scalar::<_, String>(
        "select artist from play_history order by started_at desc limit ?",
    )
    .bind(limit)
    .fetch_all(db.pool())
    .await
    .context("loading recently aired artists")
}

/// Shuffle candidates: eligible songs with their album rotation weight
/// (singles weigh normal), excluding the current song, anything queued, and —
/// unless relaxed — songs aired within the separation window.
async fn shuffle_candidates(
    db: &Database,
    respect_history: bool,
    exclude_queued: bool,
) -> anyhow::Result<Vec<ShuffleCandidate>> {
    let history_clause = if respect_history {
        "and songs.id not in (select song_id from play_history where started_at > ?)"
    } else {
        "and ? >= 0"
    };
    let queued_clause = if exclude_queued {
        "and songs.id not in (select song_id from radio_queue)"
    } else {
        ""
    };
    let query = format!(
        r#"
        select songs.id, songs.artist, songs.album, songs.genre,
            songs.duration_seconds, songs.loudness_lufs, songs.bpm,
            coalesce((
                select max(radio_albums.rotation_weight)
                from radio_album_tracks
                join radio_albums on radio_albums.id = radio_album_tracks.album_id
                where radio_album_tracks.song_id = songs.id
                  and radio_albums.is_enabled = 1
            ), 2) as weight
        from songs
        where songs.id != coalesce((select current_song_id from radio_state where id = 1), '')
          and {SHUFFLE_ELIGIBLE}
          {history_clause}
          {queued_clause}
        "#,
    );
    sqlx::query_as::<_, ShuffleCandidate>(&query)
        .bind(now() - SHUFFLE_SEPARATION_SECS)
        .fetch_all(db.pool())
        .await
        .context("loading shuffle candidates")
}

/// Scores every candidate against the previous song, or falls back to plain
/// rotation weights when there is nothing to transition from.
fn transition_scores(
    pool: &[ShuffleCandidate],
    previous: Option<&ShuffleCandidate>,
    recent_artists: &[String],
) -> Vec<f64> {
    pool.iter()
        .map(|candidate| match previous {
            Some(previous) => transition_score(candidate, previous, recent_artists),
            None => candidate.weight.max(1) as f64,
        })
        .collect()
}

/// Picks a random song for shuffle mode, avoiding an immediate repeat of the
/// current song when the library has more than one track. The pick is scored
/// against the song on air so the transition flows.
/// Orders an explicit set of songs into a listenable sequence.
///
/// This is the deliberate counterpart to `random_shuffle_song`: instead of
/// picking one track from the whole library, it walks a fixed set, each step
/// scoring the remainder against the track just placed. So the same rules the
/// station applies to itself — artist separation, album separation, LUFS
/// energy smoothing, genre adjacency, duration pacing, and BPM pairing with
/// half/double time — also shape anything a DJ shuffles in by hand.
///
/// `weighted_pick` keeps it probabilistic rather than deterministic: a good
/// transition is likelier, not certain, so shuffling the same set twice gives
/// two different orders that are both listenable.
///
/// Songs missing from the library are preserved in their original relative
/// order at the end rather than silently dropped.
async fn sequence_songs(db: &Database, song_ids: &[String]) -> anyhow::Result<Vec<String>> {
    if song_ids.len() < 2 {
        return Ok(song_ids.to_vec());
    }

    let mut pool: Vec<ShuffleCandidate> = Vec::with_capacity(song_ids.len());
    let mut unknown: Vec<String> = Vec::new();
    for song_id in song_ids {
        match shuffle_profile(db, song_id).await? {
            Some(mut candidate) => {
                candidate.weight = song_rotation_weight(db, song_id).await?;
                pool.push(candidate);
            }
            None => unknown.push(song_id.clone()),
        }
    }

    // Seed from what's playing so the first pick transitions out of it; with a
    // silent station the first pick is by weight alone.
    let current_id = sqlx::query_scalar::<_, Option<String>>(
        "select current_song_id from radio_state where id = 1",
    )
    .fetch_one(db.pool())
    .await
    .context("loading current song id")?;
    let mut previous = match &current_id {
        Some(id) => shuffle_profile(db, id).await?,
        None => None,
    };

    let mut recent = recently_aired_artists(db, ARTIST_SEPARATION.len() as i64).await?;
    let mut seed = random_seed();
    let mut ordered = Vec::with_capacity(pool.len());

    while !pool.is_empty() {
        let scores = transition_scores(&pool, previous.as_ref(), &recent);
        let index = weighted_pick(&scores, &mut seed).unwrap_or(0);
        let picked = pool.swap_remove(index);

        // Track artists as we place them, so separation applies within this
        // batch and not just against real airplay history.
        recent.insert(0, picked.artist.clone());
        recent.truncate(ARTIST_SEPARATION.len());

        ordered.push(picked.id.clone());
        previous = Some(picked);
    }

    ordered.extend(unknown);
    Ok(ordered)
}

/// The rotation weight shuffle scoring uses for a song, defaulting to normal.
async fn song_rotation_weight(db: &Database, song_id: &str) -> anyhow::Result<i64> {
    sqlx::query_scalar::<_, i64>(
        r#"
        select coalesce((
            select max(radio_albums.rotation_weight)
            from radio_album_tracks
            join radio_albums on radio_albums.id = radio_album_tracks.album_id
            where radio_album_tracks.song_id = ?
              and radio_albums.is_enabled = 1
        ), 2)
        "#,
    )
    .bind(song_id)
    .fetch_one(db.pool())
    .await
    .context("loading song rotation weight")
}

async fn random_shuffle_song(db: &Database) -> anyhow::Result<Option<String>> {
    let mut seed = random_seed();
    // Prefer songs outside the separation window; relax when the library is
    // too small to honor it, and finally allow replaying the current song.
    let mut candidates = shuffle_candidates(db, true, false).await?;
    if candidates.is_empty() {
        candidates = shuffle_candidates(db, false, false).await?;
    }

    let current_id = sqlx::query_scalar::<_, Option<String>>(
        "select current_song_id from radio_state where id = 1",
    )
    .fetch_one(db.pool())
    .await
    .context("loading current song id")?;
    let previous = match &current_id {
        Some(id) => shuffle_profile(db, id).await?,
        None => None,
    };
    let recent = recently_aired_artists(db, ARTIST_SEPARATION.len() as i64).await?;

    let scores = transition_scores(&candidates, previous.as_ref(), &recent);
    if let Some(index) = weighted_pick(&scores, &mut seed) {
        return Ok(Some(candidates.swap_remove(index).id));
    }
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
    let existing =
        sqlx::query_scalar::<_, i64>("select count(*) from radio_queue where is_shuffle = 1")
            .fetch_one(db.pool())
            .await
            .context("counting shuffle queue rows")?;
    let need = SHUFFLE_LOOKAHEAD - existing;
    if need <= 0 {
        return Ok(());
    }

    let mut seed = random_seed();
    let mut pool = shuffle_candidates(db, true, true).await?;
    if (pool.len() as i64) < need {
        pool = shuffle_candidates(db, false, true).await?;
    }

    // Seed the transition chain from the newest queued row, else the song on
    // air, so the first refill continues whatever the listener hears last;
    // after that each pick chains off the one before it.
    let chain_seed_id = sqlx::query_scalar::<_, String>(
        "select song_id from radio_queue order by position desc, created_at desc limit 1",
    )
    .fetch_optional(db.pool())
    .await
    .context("loading last queued song id")?;
    let chain_seed_id = match chain_seed_id {
        Some(id) => Some(id),
        None => sqlx::query_scalar::<_, Option<String>>(
            "select current_song_id from radio_state where id = 1",
        )
        .fetch_one(db.pool())
        .await
        .context("loading current song id")?,
    };
    let mut previous = match &chain_seed_id {
        Some(id) => shuffle_profile(db, id).await?,
        None => None,
    };
    let mut recent: Vec<String> = Vec::new();
    if let Some(previous) = &previous {
        recent.push(previous.artist.clone());
    }
    recent.extend(recently_aired_artists(db, ARTIST_SEPARATION.len() as i64).await?);
    recent.truncate(ARTIST_SEPARATION.len());

    let mut candidates = Vec::new();
    while (candidates.len() as i64) < need && !pool.is_empty() {
        let scores = transition_scores(&pool, previous.as_ref(), &recent);
        let Some(index) = weighted_pick(&scores, &mut seed) else {
            break;
        };
        let picked = pool.swap_remove(index);
        recent.insert(0, picked.artist.clone());
        recent.truncate(ARTIST_SEPARATION.len());
        previous = Some(picked.clone());
        candidates.push(picked.id);
    }

    let mut position =
        sqlx::query_scalar::<_, Option<i64>>("select max(position) from radio_queue")
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
    use crate::chat::ChatService;
    use crate::db::Database;
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

        // Shuffle off + empty queue + no albums => the singles loop keeps the
        // station playing (loose songs are always part of rotation).
        skip_to_next(&db, "did").await.unwrap();
        let state = radio_state(&db).await.unwrap();
        assert_eq!(state.status, "playing");
        assert!(state.current_song_id.is_some());
        assert!(!state.shuffle);

        // Toggling shuffle on should fill the queue with the upcoming shuffle
        // lookahead (all songs except the one now playing, since the library
        // only has three).
        service
            .control(RadioControlAction::Shuffle, "did")
            .await
            .unwrap();
        let state = radio_state(&db).await.unwrap();
        assert!(state.shuffle);
        assert_eq!(state.status, "playing");
        let queue = queue_items(&db).await.unwrap();
        assert!(
            !queue.is_empty(),
            "shuffle should show upcoming songs in the queue"
        );
        assert!(
            queue.iter().all(|item| item.is_shuffle),
            "auto-filled rows are marked shuffle"
        );
        let mut prev = state
            .current_song_id
            .clone()
            .expect("shuffle should start playback");

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
        service
            .control(RadioControlAction::Shuffle, "did")
            .await
            .unwrap();
        let state = radio_state(&db).await.unwrap();
        assert!(!state.shuffle);
        let queue = queue_items(&db).await.unwrap();
        assert!(
            queue.is_empty(),
            "disabling shuffle clears auto-filled rows"
        );
    }

    #[tokio::test]
    async fn sequence_songs_separates_artists_and_keeps_everything() {
        let (_service, db) = setup_test_service().await;

        // Six tracks, two artists, deliberately interleaved badly on input:
        // all of A's, then all of B's.
        for (i, artist) in ["A", "A", "A", "B", "B", "B"].iter().enumerate() {
            sqlx::query(
                "insert into songs (id, title, artist, file_path, added_by_did, created_at, bpm) values (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(format!("s{i}"))
            .bind(format!("Title {i}"))
            .bind(*artist)
            .bind(format!("p{i}.mp3"))
            .bind("did")
            .bind(100 + i as i64)
            .bind(120.0)
            .execute(db.pool())
            .await
            .unwrap();
        }

        let input: Vec<String> = (0..6).map(|i| format!("s{i}")).collect();
        let ordered = sequence_songs(&db, &input).await.unwrap();

        assert_eq!(ordered.len(), input.len(), "sequencing must not drop tracks");
        let mut sorted = ordered.clone();
        sorted.sort();
        let mut expected = input.clone();
        expected.sort();
        assert_eq!(sorted, expected, "sequencing must not invent or lose tracks");

        // Artist separation should break up the three-in-a-row runs the input had.
        let artists: Vec<&str> = ordered
            .iter()
            .map(|id| if id < &"s3".to_string() { "A" } else { "B" })
            .collect();
        let longest_run = artists
            .windows(2)
            .fold((1, 1), |(best, run), pair| {
                let run = if pair[0] == pair[1] { run + 1 } else { 1 };
                (best.max(run), run)
            })
            .0;
        assert!(
            longest_run < 3,
            "expected artists to be broken up, got {artists:?}"
        );
    }

    #[tokio::test]
    async fn sequence_songs_preserves_unknown_ids() {
        let (_service, db) = setup_test_service().await;
        let ordered = sequence_songs(&db, &["ghost-a".into(), "ghost-b".into()])
            .await
            .unwrap();
        assert_eq!(ordered, vec!["ghost-a".to_string(), "ghost-b".to_string()]);
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
        let resolved_title: String =
            sqlx::query_scalar("select album from songs where id = 'song-1'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        let resolved_title_2: String =
            sqlx::query_scalar("select album from songs where id = 'song-2'")
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

        let album_a = initial_albums
            .iter()
            .find(|a| a.title == "Album A")
            .unwrap();
        let album_b = initial_albums
            .iter()
            .find(|a| a.title == "Album B")
            .unwrap();

        // Point loop state cursor to Album B
        sqlx::query(
            "update radio_loop_state set last_album_id = ?, last_track_position = 1 where id = 1",
        )
        .bind(&album_b.id)
        .execute(db.pool())
        .await
        .unwrap();

        // Merge Album B into Album A
        let remaining_albums = service
            .merge_albums(&album_b.id, &album_a.id)
            .await
            .unwrap();
        assert_eq!(remaining_albums.len(), 1);
        assert_eq!(remaining_albums[0].title, "Album A");
        assert_eq!(remaining_albums[0].tracks.len(), 2);

        // Verify loop state was updated to target album (Album A)
        let loop_cursor: (Option<String>, i64) = sqlx::query_as(
            "select last_album_id, last_track_position from radio_loop_state where id = 1",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(loop_cursor.0.unwrap(), album_a.id);
        assert_eq!(loop_cursor.1, 1);
    }

    fn candidate(artist: &str, weight: i64) -> ShuffleCandidate {
        ShuffleCandidate {
            id: artist.to_lowercase(),
            artist: artist.into(),
            album: None,
            genre: None,
            duration_seconds: None,
            loudness_lufs: None,
            bpm: None,
            weight,
        }
    }

    #[test]
    fn transition_score_separates_recent_artists() {
        let previous = candidate("The Smiths", 2);
        let repeat = candidate("The Smiths", 2);
        let fresh = candidate("Cocteau Twins", 2);
        let recent = vec!["The Smiths".to_string()];
        let repeat_score = transition_score(&repeat, &previous, &recent);
        let fresh_score = transition_score(&fresh, &previous, &recent);
        assert!(repeat_score < fresh_score * 0.2);
    }

    #[test]
    fn transition_score_prefers_close_tempo_and_energy() {
        let mut previous = candidate("A", 2);
        previous.bpm = Some(120.0);
        previous.loudness_lufs = Some(-10.0);

        let mut near = candidate("B", 2);
        near.bpm = Some(122.0);
        near.loudness_lufs = Some(-11.0);

        let mut far = candidate("C", 2);
        far.bpm = Some(158.0);
        far.loudness_lufs = Some(-20.0);

        let near_score = transition_score(&near, &previous, &[]);
        let far_score = transition_score(&far, &previous, &[]);
        assert!(near_score > far_score * 2.0);
    }

    #[test]
    fn transition_score_treats_half_time_as_neighbor() {
        let mut previous = candidate("A", 2);
        previous.bpm = Some(140.0);
        let mut half_time = candidate("B", 2);
        half_time.bpm = Some(70.0);
        let boosted = transition_score(&half_time, &previous, &[]);
        assert!(
            boosted > 2.0,
            "half time should score the close-tempo boost, got {boosted}"
        );
    }

    #[test]
    fn transition_score_ignores_unanalyzed_tempo() {
        let mut previous = candidate("A", 2);
        previous.bpm = Some(120.0);
        let mut unknown = candidate("B", 2);
        unknown.bpm = Some(0.0);
        assert_eq!(transition_score(&unknown, &previous, &[]), 2.0);
    }
}
