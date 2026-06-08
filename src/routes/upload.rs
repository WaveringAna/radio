use axum::{Json, extract::State, http::StatusCode};
use serde::Deserialize;

use super::{AppState, ErrorResponse, SessionToken, admin_session, api_error, internal_api_error};
use super::helpers::{
    extract_embedded_metadata as extract_embedded_metadata_inner,
    is_playlist_upload, parse_filename_metadata, parse_m3u, playlist_entry_url,
    reject_unsupported_audio_upload,
};
use crate::metadata::fetch_online_metadata;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UrlSongRequest {
    pub(crate) url: String,
    pub(crate) title: Option<String>,
    pub(crate) artist: Option<String>,
    pub(crate) album: Option<String>,
    pub(crate) add_to_queue: Option<bool>,
}

pub(crate) fn is_ytdlp_url(url: &str) -> bool {
    url.contains("youtube.com/")
        || url.contains("youtu.be/")
        || url.contains("soundcloud.com/")
        || url.contains("bandcamp.com/")
        || url.contains("vimeo.com/")
}

/// Sentinel carried on the error so the caller can map a permanently-gone
/// source to a distinct, user-facing code instead of a generic gateway error.
pub(crate) const SOURCE_UNAVAILABLE_MARKER: &str = "source_unavailable";

/// True when yt-dlp's stderr means the source itself is gone.
pub(crate) fn is_source_unavailable(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    s.contains("video unavailable")
        || s.contains("this video is not available")
        || s.contains("video is not available")
        || s.contains("private video")
        || s.contains("members-only")
        || s.contains("who has blocked it in your country")
        || s.contains("video has been removed")
        || s.contains("account associated with this video has been terminated")
        || s.contains("no longer available")
}

pub(crate) struct YtdlpResult {
    pub(crate) bytes: Vec<u8>,
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) album: Option<String>,
    pub(crate) duration_seconds: Option<i64>,
    pub(crate) thumbnail_url: Option<String>,
}

pub(crate) async fn download_with_ytdlp(url: &str) -> anyhow::Result<YtdlpResult> {
    use tokio::process::Command;

    let meta_out = Command::new("yt-dlp")
        .args([
            "--no-update",
            "--extractor-args",
            "youtube:player_client=android_vr",
            "--dump-json",
            "--no-playlist",
            url,
        ])
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("yt-dlp not found (is it installed?): {e}"))?;

    if !meta_out.status.success() {
        let stderr = String::from_utf8_lossy(&meta_out.stderr);
        if is_source_unavailable(&stderr) {
            return Err(anyhow::anyhow!(SOURCE_UNAVAILABLE_MARKER));
        }
        return Err(anyhow::anyhow!("yt-dlp metadata failed: {stderr}"));
    }

    let meta: serde_json::Value = serde_json::from_slice(&meta_out.stdout)
        .map_err(|_| anyhow::anyhow!("failed to parse yt-dlp output"))?;

    let title = meta["title"].as_str().unwrap_or("Unknown").to_owned();
    let artist = meta["artist"]
        .as_str()
        .or_else(|| meta["uploader"].as_str())
        .or_else(|| meta["channel"].as_str())
        .unwrap_or("Unknown")
        .to_owned();
    let album = meta["album"].as_str().map(ToOwned::to_owned);
    let duration_seconds = meta["duration"].as_f64().map(|d| d as i64);
    let thumbnail_url = meta["thumbnail"].as_str().map(ToOwned::to_owned);

    let tmp_path = format!("/tmp/radio-{}.mp3", uuid::Uuid::new_v4());

    let dl = Command::new("yt-dlp")
        .args([
            "--no-update",
            "--extractor-args",
            "youtube:player_client=android_vr",
            "-x",
            "--audio-format",
            "mp3",
            "--audio-quality",
            "0",
            "--no-playlist",
            "-o",
            &tmp_path,
            url,
        ])
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("yt-dlp download failed: {e}"))?;

    if !dl.status.success() {
        let stderr = String::from_utf8_lossy(&dl.stderr);
        if is_source_unavailable(&stderr) {
            return Err(anyhow::anyhow!(SOURCE_UNAVAILABLE_MARKER));
        }
        return Err(anyhow::anyhow!("yt-dlp download failed: {stderr}"));
    }

    let bytes = tokio::fs::read(&tmp_path)
        .await
        .map_err(|_| anyhow::anyhow!("failed to read downloaded audio"))?;

    let _ = tokio::fs::remove_file(&tmp_path).await;

    Ok(YtdlpResult {
        bytes,
        title,
        artist,
        album,
        duration_seconds,
        thumbnail_url,
    })
}

pub(crate) async fn upload_song_from_url(
    State(state): State<AppState>,
    session_token: SessionToken,
    Json(payload): Json<UrlSongRequest>,
) -> Result<Json<crate::radio::Song>, (StatusCode, Json<ErrorResponse>)> {
    let session = admin_session(&state, session_token.0.as_deref()).await?;
    add_song_from_url_source(&state, &session.account_did, payload)
        .await
        .map(Json)
}

pub(crate) async fn add_song_from_url_source(
    state: &AppState,
    admin_did: &str,
    payload: UrlSongRequest,
) -> Result<crate::radio::Song, (StatusCode, Json<ErrorResponse>)> {
    let url = payload.url.trim().to_owned();
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(api_error(StatusCode::BAD_REQUEST, "invalid_url"));
    }

    let title_override = payload
        .title
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    let artist_override = payload
        .artist
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());

    if is_ytdlp_url(&url) {
        let dl = download_with_ytdlp(&url).await.map_err(|error| {
            tracing::warn!(%error, %url, "yt-dlp download failed");
            if error.to_string().contains(SOURCE_UNAVAILABLE_MARKER) {
                api_error(StatusCode::UNPROCESSABLE_ENTITY, "source_unavailable")
            } else {
                api_error(StatusCode::BAD_GATEWAY, "ytdlp_failed")
            }
        })?;

        let title = title_override.unwrap_or(dl.title);
        let artist = artist_override.unwrap_or(dl.artist);
        let album = payload.album.or(dl.album);

        let mut song = state
            .radio
            .add_song(
                crate::radio::NewSongUpload {
                    filename: None,
                    mime_type: Some("audio/mpeg".into()),
                    bytes: dl.bytes,
                    title,
                    artist,
                    album,
                    genre: None,
                    duration_seconds: dl.duration_seconds,
                    add_to_queue: payload.add_to_queue.unwrap_or(false),
                },
                admin_did,
            )
            .await
            .map_err(internal_api_error)?;

        // Attach thumbnail as cover art
        if let Some(thumb_url) = dl.thumbnail_url {
            if let Ok(thumb_resp) = reqwest::get(&thumb_url).await {
                if thumb_resp.status().is_success() {
                    let cover_mime = thumb_resp
                        .headers()
                        .get(reqwest::header::CONTENT_TYPE)
                        .and_then(|v| v.to_str().ok())
                        .map(ToOwned::to_owned);
                    if let Ok(cover_bytes) = thumb_resp.bytes().await {
                        if let Ok(updated) = state
                            .radio
                            .set_song_cover(&song.id, None, cover_mime, cover_bytes.to_vec())
                            .await
                        {
                            song = updated;
                        }
                    }
                }
            }
        }

        return Ok(song);
    }

    // Plain URL download
    let title =
        title_override.ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "missing_title"))?;
    let artist =
        artist_override.ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "missing_artist"))?;

    let response = reqwest::get(&url).await.map_err(|error| {
        tracing::warn!(%error, %url, "failed to fetch url");
        api_error(StatusCode::BAD_REQUEST, "url_fetch_failed")
    })?;

    if !response.status().is_success() {
        return Err(api_error(StatusCode::BAD_REQUEST, "url_fetch_failed"));
    }

    let mime_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(';').next().unwrap_or(s).trim().to_owned());

    let filename = url
        .split('/')
        .last()
        .and_then(|s| s.split('?').next())
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);

    let bytes = response
        .bytes()
        .await
        .map_err(|error| {
            tracing::warn!(%error, %url, "failed to read url response body");
            api_error(StatusCode::BAD_REQUEST, "url_fetch_failed")
        })?
        .to_vec();

    if is_playlist_upload(filename.as_deref(), mime_type.as_deref(), &bytes) {
        let songs = import_m3u_url_playlist(
            state,
            admin_did,
            &url,
            &bytes,
            payload.album.clone(),
            payload.add_to_queue.unwrap_or(false),
        )
        .await?;
        let first_song = songs
            .into_iter()
            .next()
            .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "empty_playlist"))?;
        return Ok(first_song);
    }

    let embedded = extract_embedded_metadata_inner(&bytes).await;
    let embedded_cover = embedded.cover;
    let mut genre = embedded.genre;

    let online = if genre.is_none() || embedded_cover.is_none() {
        Some(fetch_online_metadata(&artist, payload.album.as_deref(), &title).await)
    } else {
        None
    };

    if genre.is_none() {
        genre = online.as_ref().and_then(|ol| ol.genre.clone());
    }

    let mut song = state
        .radio
        .add_song(
            crate::radio::NewSongUpload {
                filename,
                mime_type,
                bytes,
                title: title.clone(),
                artist: artist.clone(),
                album: payload.album.clone(),
                genre,
                duration_seconds: embedded.duration_seconds,
                add_to_queue: payload.add_to_queue.unwrap_or(false),
            },
            admin_did,
        )
        .await
        .map_err(internal_api_error)?;

    let cover = embedded_cover.or_else(|| online.and_then(|ol| ol.cover));

    if let Some((cover_bytes, cover_mime)) = cover {
        match state
            .radio
            .set_song_cover(&song.id, None, Some(cover_mime), cover_bytes)
            .await
        {
            Ok(updated) => song = updated,
            Err(error) => {
                tracing::warn!(%error, song_id = %song.id, "failed to set auto-fetched cover")
            }
        }
    }

    Ok(song)
}

pub(crate) async fn import_m3u_url_playlist(
    state: &AppState,
    admin_did: &str,
    playlist_url: &str,
    playlist_bytes: &[u8],
    album: Option<String>,
    add_to_queue: bool,
) -> Result<Vec<crate::radio::Song>, (StatusCode, Json<ErrorResponse>)> {
    const MAX_PLAYLIST_ENTRIES: usize = 100;

    let base_url = reqwest::Url::parse(playlist_url)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "invalid_url"))?;
    let entries = parse_m3u(playlist_bytes);
    if entries.is_empty() {
        return Err(api_error(StatusCode::BAD_REQUEST, "empty_playlist"));
    }

    let client = reqwest::Client::builder()
        .user_agent("radio/0.1")
        .build()
        .map_err(|error| internal_api_error(error.into()))?;
    let mut songs = Vec::new();

    for entry in entries.into_iter().take(MAX_PLAYLIST_ENTRIES) {
        let entry_url = playlist_entry_url(&base_url, &entry.location)?;

        let response = client
            .get(entry_url.clone())
            .send()
            .await
            .map_err(|error| {
                tracing::warn!(%error, url = %entry_url, "failed to fetch playlist entry");
                api_error(StatusCode::BAD_REQUEST, "playlist_entry_fetch_failed")
            })?;
        if !response.status().is_success() {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                "playlist_entry_fetch_failed",
            ));
        }

        let mime_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.split(';').next().unwrap_or(value).trim().to_owned());
        let filename = entry_url
            .path_segments()
            .and_then(Iterator::last)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        let bytes = response
            .bytes()
            .await
            .map_err(|_| api_error(StatusCode::BAD_REQUEST, "playlist_entry_fetch_failed"))?
            .to_vec();
        reject_unsupported_audio_upload(filename.as_deref(), mime_type.as_deref(), &bytes)?;

        let embedded = extract_embedded_metadata_inner(&bytes).await;
        let (parsed_artist, parsed_title) = filename
            .as_deref()
            .map(parse_filename_metadata)
            .unwrap_or((None, None));
        let title = embedded
            .title
            .clone()
            .or(entry.title.clone())
            .or(parsed_title)
            .unwrap_or_else(|| "Unknown".to_owned());
        let artist = embedded
            .artist
            .clone()
            .or(entry.artist.clone())
            .or(parsed_artist)
            .unwrap_or_else(|| "Unknown".to_owned());
        let genre = embedded.genre.clone();
        let cover = embedded.cover;
        let mut song = state
            .radio
            .add_song(
                crate::radio::NewSongUpload {
                    filename,
                    mime_type,
                    bytes,
                    title,
                    artist,
                    album: embedded.album.clone().or_else(|| album.clone()),
                    genre,
                    duration_seconds: embedded.duration_seconds,
                    add_to_queue,
                },
                admin_did,
            )
            .await
            .map_err(internal_api_error)?;

        if let Some((cover_bytes, cover_mime)) = cover {
            match state
                .radio
                .set_song_cover(&song.id, None, Some(cover_mime), cover_bytes)
                .await
            {
                Ok(updated) => song = updated,
                Err(error) => {
                    tracing::warn!(%error, song_id = %song.id, "failed to set playlist entry cover")
                }
            }
        }

        songs.push(song);
    }

    Ok(songs)
}
