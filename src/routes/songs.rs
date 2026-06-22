use axum::{
    Json,
    body::Body,
    extract::{Multipart, Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::Response,
};
use serde::Deserialize;

use super::{
    AppState, ErrorResponse, SessionToken, admin_session, api_error, internal_api_error,
};
use super::helpers::{
    extract_embedded_metadata, parse_byte_range,
    parse_filename_metadata, reject_unsupported_audio_upload,
};
use crate::metadata::fetch_online_metadata;
use crate::radio::{NewSongUpload, SongFile, SongMetadataUpdate};

#[derive(Deserialize)]
pub(crate) struct AlbumEnabledRequest {
    pub(crate) enabled: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SongMetadataRequest {
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) album: Option<String>,
    pub(crate) genre: Option<String>,
    pub(crate) duration_seconds: Option<i64>,
}

// ── Albums ──

pub(crate) async fn get_albums(
    State(state): State<AppState>,
    session_token: SessionToken,
) -> Result<Json<Vec<crate::radio::RadioAlbum>>, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, session_token.0.as_deref()).await?;
    state
        .radio
        .albums()
        .await
        .map(Json)
        .map_err(internal_api_error)
}

pub(crate) async fn delete_album(
    State(state): State<AppState>,
    session_token: SessionToken,
    Path(album_id): Path<String>,
) -> Result<Json<Vec<crate::radio::RadioAlbum>>, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, session_token.0.as_deref()).await?;
    state
        .radio
        .delete_album(&album_id)
        .await
        .map(Json)
        .map_err(internal_api_error)
}

pub(crate) async fn set_album_enabled(
    State(state): State<AppState>,
    session_token: SessionToken,
    Path(album_id): Path<String>,
    Json(payload): Json<AlbumEnabledRequest>,
) -> Result<Json<Vec<crate::radio::RadioAlbum>>, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, session_token.0.as_deref()).await?;
    state
        .radio
        .set_album_enabled(&album_id, payload.enabled)
        .await
        .map(Json)
        .map_err(internal_api_error)
}

// ── Songs ──

pub(crate) async fn get_songs(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::radio::Song>>, (StatusCode, Json<ErrorResponse>)> {
    state
        .radio
        .songs()
        .await
        .map(Json)
        .map_err(internal_api_error)
}

pub(crate) async fn update_song(
    State(state): State<AppState>,
    session_token: SessionToken,
    Path(song_id): Path<String>,
    Json(input): Json<SongMetadataRequest>,
) -> Result<Json<crate::radio::Song>, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, session_token.0.as_deref()).await?;
    state
        .radio
        .update_song_metadata(
            &song_id,
            SongMetadataUpdate {
                title: input.title,
                artist: input.artist,
                album: input.album,
                genre: input.genre,
                duration_seconds: input.duration_seconds,
            },
        )
        .await
        .map(Json)
        .map_err(internal_api_error)
}

pub(crate) async fn delete_song(
    State(state): State<AppState>,
    session_token: SessionToken,
    Path(song_id): Path<String>,
) -> Result<Json<crate::radio::RadioSnapshot>, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, session_token.0.as_deref()).await?;
    state
        .radio
        .delete_song(&song_id)
        .await
        .map(Json)
        .map_err(internal_api_error)
}

// ── Upload ──

pub(crate) async fn upload_song(
    State(state): State<AppState>,
    session_token: SessionToken,
    multipart: Multipart,
) -> Result<Json<crate::radio::Song>, (StatusCode, Json<ErrorResponse>)> {
    let session = admin_session(&state, session_token.0.as_deref()).await?;
    add_song_from_multipart_upload(&state, &session.account_did, multipart)
        .await
        .map(Json)
}

pub(crate) async fn add_song_from_multipart_upload(
    state: &AppState,
    uploader_did: &str,
    multipart: Multipart,
) -> Result<crate::radio::Song, (StatusCode, Json<ErrorResponse>)> {
    let upload = super::helpers::parse_song_upload(multipart).await?;
    add_song_from_upload(state, uploader_did, upload).await
}

pub(crate) async fn add_song_from_upload(
    state: &AppState,
    uploader_did: &str,
    mut upload: NewSongUpload,
) -> Result<crate::radio::Song, (StatusCode, Json<ErrorResponse>)> {
    reject_unsupported_audio_upload(
        upload.filename.as_deref(),
        upload.mime_type.as_deref(),
        &upload.bytes,
    )?;

    let embedded = extract_embedded_metadata(&upload.bytes).await;
    if upload.genre.is_none() {
        upload.genre = embedded.genre;
    }
    if upload.title.is_empty() {
        if let Some(title) = embedded.title.clone() {
            upload.title = title;
        }
    }
    if upload.artist.is_empty() {
        if let Some(artist) = embedded.artist.clone() {
            upload.artist = artist;
        }
    }
    if upload.album.is_none() {
        upload.album = embedded.album.clone();
    }
    if upload.duration_seconds.is_none() {
        upload.duration_seconds = embedded.duration_seconds;
    }
    if (upload.title.is_empty() || upload.artist.is_empty())
        && let Some(filename) = upload.filename.as_deref()
    {
        let (parsed_artist, parsed_title) = parse_filename_metadata(filename);
        if upload.title.is_empty()
            && let Some(title) = parsed_title
        {
            upload.title = title;
        }
        if upload.artist.is_empty()
            && let Some(artist) = parsed_artist
        {
            upload.artist = artist;
        }
    }
    if upload.title.is_empty() {
        return Err(api_error(StatusCode::BAD_REQUEST, "missing_title"));
    }
    if upload.artist.is_empty() {
        return Err(api_error(StatusCode::BAD_REQUEST, "missing_artist"));
    }

    let artist = upload.artist.clone();
    let album = upload.album.clone();
    let title = upload.title.clone();
    let embedded_cover = embedded.cover;

    // Fetch from MusicBrainz before saving so genre can be included in the INSERT
    let online = if upload.genre.is_none() || embedded_cover.is_none() {
        Some(fetch_online_metadata(&artist, album.as_deref(), &title).await)
    } else {
        None
    };

    if upload.genre.is_none() {
        upload.genre = online.as_ref().and_then(|ol| ol.genre.clone());
    }

    let mut song = state
        .radio
        .add_song(upload, uploader_did)
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

// ── Audio / Cover serving ──

pub(crate) async fn read_song_file_response(
    song_file: SongFile,
    not_found_error: &str,
    range_header: Option<&str>,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    let bytes = tokio::fs::read(&song_file.file_path)
        .await
        .map_err(|error| {
            tracing::error!(?error, path = %song_file.file_path, "failed to read song file");
            api_error(StatusCode::NOT_FOUND, not_found_error)
        })?;

    let content_type = song_file
        .mime_type
        .unwrap_or_else(|| "application/octet-stream".into());
    let total_len = bytes.len() as u64;

    if let Some(range_header) = range_header {
        if let Some((start, end)) = parse_byte_range(range_header, total_len) {
            let chunk = bytes[start as usize..=end as usize].to_vec();
            return Ok(Response::builder()
                .status(StatusCode::PARTIAL_CONTENT)
                .header(header::CONTENT_TYPE, &content_type)
                .header(header::ACCEPT_RANGES, "bytes")
                .header(
                    header::CONTENT_RANGE,
                    format!("bytes {start}-{end}/{total_len}"),
                )
                .header(header::CONTENT_LENGTH, chunk.len().to_string())
                .body(Body::from(chunk))
                .expect("partial file response should be valid"));
        }

        return Ok(Response::builder()
            .status(StatusCode::RANGE_NOT_SATISFIABLE)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CONTENT_RANGE, format!("bytes */{total_len}"))
            .body(Body::empty())
            .expect("range error response should be valid"));
    }

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, total_len.to_string())
        .body(Body::from(bytes))
        .expect("file response should be valid"))
}

pub(crate) async fn song_audio(
    State(state): State<AppState>,
    Path(song_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    let Some(song_file) = state
        .radio
        .song_file(&song_id)
        .await
        .map_err(internal_api_error)?
    else {
        return Err(api_error(StatusCode::NOT_FOUND, "song_not_found"));
    };

    read_song_file_response(
        song_file,
        "audio_not_found",
        headers
            .get(header::RANGE)
            .and_then(|value| value.to_str().ok()),
    )
    .await
}

pub(crate) async fn song_cover(
    State(state): State<AppState>,
    Path(song_id): Path<String>,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    let Some(song_file) = state
        .radio
        .cover_file(&song_id)
        .await
        .map_err(internal_api_error)?
    else {
        return Err(api_error(StatusCode::NOT_FOUND, "cover_not_found"));
    };

    let mut response = read_song_file_response(song_file, "cover_not_found", None).await?;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    Ok(response)
}

pub(crate) async fn song_cover_thumbnail(
    State(state): State<AppState>,
    Path(song_id): Path<String>,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    let Some(thumb_path) = state
        .radio
        .cover_thumbnail(&song_id)
        .await
        .map_err(internal_api_error)?
    else {
        return Err(api_error(StatusCode::NOT_FOUND, "cover_not_found"));
    };

    let bytes = tokio::fs::read(&thumb_path).await.map_err(|error| {
        tracing::error!(?error, "failed to read thumbnail");
        api_error(StatusCode::INTERNAL_SERVER_ERROR, "thumbnail_read_error")
    })?;

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "image/jpeg")
        .header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")
        .header(header::CONTENT_LENGTH, bytes.len().to_string())
        .body(Body::from(bytes))
        .expect("thumbnail response should be valid"))
}

pub(crate) async fn upload_song_cover(
    State(state): State<AppState>,
    session_token: SessionToken,
    Path(song_id): Path<String>,
    mut multipart: Multipart,
) -> Result<Json<crate::radio::Song>, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, session_token.0.as_deref()).await?;
    let mut filename = None;
    let mut mime_type = None;
    let mut bytes = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "invalid_multipart"))?
    {
        if field.name() == Some("cover") {
            filename = field.file_name().map(ToOwned::to_owned);
            mime_type = field.content_type().map(ToString::to_string);
            bytes = Some(
                field
                    .bytes()
                    .await
                    .map_err(|_| api_error(StatusCode::BAD_REQUEST, "invalid_cover_file"))?
                    .to_vec(),
            );
        }
    }

    state
        .radio
        .set_song_cover(
            &song_id,
            filename,
            mime_type,
            bytes.ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "missing_cover_file"))?,
        )
        .await
        .map(Json)
        .map_err(internal_api_error)
}
