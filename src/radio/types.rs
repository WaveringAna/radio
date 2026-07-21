use serde::Serialize;
use sqlx::FromRow;

pub(crate) const UNKNOWN_DURATION_ADVANCE_AFTER_SECONDS: i64 = 30 * 60;

/// Radio playback status persisted by the backend.
#[derive(Clone, Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RadioState {
    pub(crate) current_song_id: Option<String>,
    pub(crate) status: String,
    pub(crate) started_at: Option<i64>,
    pub(crate) paused_at: Option<i64>,
    pub(crate) position_seconds: i64,
    pub(crate) updated_by_did: Option<String>,
    /// When true, the empty-queue fallback plays a random song from the whole
    /// library instead of stepping through album loops.
    #[serde(default)]
    pub(crate) shuffle: bool,
    /// How finished queue tracks are recycled: "off", "one", or "queue".
    #[serde(default)]
    pub(crate) loop_mode: String,
    /// Playlist re-queued automatically whenever the queue drains.
    #[serde(default)]
    pub(crate) loop_playlist_id: Option<String>,
}

/// Song metadata stored by the backend.
#[derive(Clone, Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Song {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) album: Option<String>,
    pub(crate) genre: Option<String>,
    pub(crate) duration_seconds: Option<i64>,
    pub(crate) mime_type: Option<String>,
    pub(crate) has_cover: bool,
    pub(crate) added_by_did: String,
    pub(crate) created_at: i64,
    pub(crate) loudness_lufs: Option<f64>,
    pub(crate) loudness_peak: Option<f64>,
}

#[derive(Clone, Debug, FromRow)]
pub(crate) struct SongFile {
    pub(crate) file_path: String,
    pub(crate) mime_type: Option<String>,
}

#[derive(Clone, Debug, FromRow)]
pub(crate) struct StoredAudioFile {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) file_path: String,
    pub(crate) mime_type: Option<String>,
}

#[derive(Clone, Debug, FromRow)]
pub(crate) struct MissingMetadataSong {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) album: Option<String>,
}

/// Queue item joined with song metadata.
#[derive(Clone, Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QueueItem {
    pub(crate) id: String,
    pub(crate) position: i64,
    pub(crate) queued_by_did: String,
    /// True when this row was auto-filled by shuffle mode rather than queued by
    /// an admin. Manual items sort ahead of shuffle items.
    #[serde(default)]
    pub(crate) is_shuffle: bool,
    pub(crate) song_id: String,
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) album: Option<String>,
    pub(crate) genre: Option<String>,
    pub(crate) duration_seconds: Option<i64>,
    pub(crate) mime_type: Option<String>,
    pub(crate) has_cover: bool,
    pub(crate) added_by_did: String,
    pub(crate) created_at: i64,
    pub(crate) loudness_lufs: Option<f64>,
    pub(crate) loudness_peak: Option<f64>,
}

/// Admin-defined album loop metadata.
#[derive(Clone, Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RadioAlbum {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) position: i64,
    pub(crate) is_enabled: bool,
    #[sqlx(skip)]
    pub(crate) tracks: Vec<Song>,
}

/// Combined radio view returned to clients.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RadioSnapshot {
    pub(crate) state: RadioState,
    pub(crate) current_song: Option<Song>,
    pub(crate) queue: Vec<QueueItem>,
}

/// One aired song in the station's play history.
#[derive(Clone, Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PlayHistoryItem {
    pub(crate) song_id: String,
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) started_at: i64,
}

/// The song rotation will play next when the queue drains (loop mode only).
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RotationUpNext {
    pub(crate) song_id: String,
    pub(crate) title: String,
    pub(crate) artist: String,
    /// Where it comes from: the album title, or "singles".
    pub(crate) source: String,
}

/// Rotation metadata for the admin UI: album weights plus the recent airlog.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RotationInfo {
    pub(crate) weights: std::collections::BTreeMap<String, i64>,
    pub(crate) recently_played: Vec<PlayHistoryItem>,
    /// Deterministic next rotation track; None while shuffle mode is on.
    pub(crate) up_next: Option<RotationUpNext>,
}

/// Live seek position returned by the backend.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RadioSeek {
    pub(crate) position_seconds: i64,
}

/// Realtime events broadcast to websocket clients.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub(crate) enum RadioEvent {
    SnapshotChanged {
        snapshot: RadioSnapshot,
    },
    ViewerCountChanged {
        viewer_count: usize,
        #[serde(default)]
        listener_dids: Vec<String>,
    },
    ViewerKeepalive,
}

/// Uploaded song input after multipart parsing.
pub(crate) struct NewSongUpload {
    pub(crate) filename: Option<String>,
    pub(crate) mime_type: Option<String>,
    pub(crate) bytes: Vec<u8>,
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) album: Option<String>,
    pub(crate) genre: Option<String>,
    pub(crate) duration_seconds: Option<i64>,
    pub(crate) add_to_queue: bool,
}

/// Editable song metadata.
pub(crate) struct SongMetadataUpdate {
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) album: Option<String>,
    pub(crate) genre: Option<String>,
    pub(crate) duration_seconds: Option<i64>,
}

/// Admin radio control actions.
pub(crate) enum RadioControlAction {
    Play,
    Pause,
    Stop,
    Skip,
    Previous,
    /// Toggles station-wide shuffle mode.
    Shuffle,
    /// Sets how finished queue tracks are recycled.
    SetLoopMode(String),
    /// Pins (or, with `None`, unpins) the playlist that reloads when the queue drains.
    SetLoopPlaylist(Option<String>),
}

/// A saved playlist/set of songs.
#[derive(Clone, Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Playlist {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) created_at: i64,
    /// Randomize the track order every time this set is loaded into the queue.
    pub(crate) shuffle_on_load: bool,
    #[sqlx(skip)]
    pub(crate) tracks: Vec<Song>,
}
