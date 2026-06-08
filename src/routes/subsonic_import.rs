use axum::{Json, extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};

use super::{AppState, ErrorResponse, SessionToken, admin_session, api_error, internal_api_error};
use super::helpers::{extract_share_info, parse_share_url};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SubsonicCreds {
    pub(crate) server_url: String,
    pub(crate) username: String,
    pub(crate) password: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SubsonicSearchRequest {
    #[serde(flatten)]
    pub(crate) creds: SubsonicCreds,
    pub(crate) query: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SubsonicSongResult {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) album: Option<String>,
    pub(crate) duration_seconds: Option<u64>,
    pub(crate) cover_art_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SubsonicImportRequest {
    #[serde(flatten)]
    pub(crate) creds: SubsonicCreds,
    pub(crate) song_id: String,
    pub(crate) cover_art_id: Option<String>,
    pub(crate) add_to_queue: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SubsonicShareImportRequest {
    pub(crate) share_url: String,
    pub(crate) add_to_queue: Option<bool>,
}

pub(crate) fn subsonic_auth_params(creds: &SubsonicCreds) -> [(&'static str, String); 5] {
    let hex_pass = creds
        .password
        .bytes()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    [
        ("u", creds.username.clone()),
        ("p", format!("enc:{hex_pass}")),
        ("v", "1.16.1".into()),
        ("c", "radio".into()),
        ("f", "json".into()),
    ]
}

pub(crate) async fn subsonic_search(
    State(state): State<AppState>,
    session_token: SessionToken,
    Json(payload): Json<SubsonicSearchRequest>,
) -> Result<Json<Vec<SubsonicSongResult>>, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, session_token.0.as_deref()).await?;

    let base = payload.creds.server_url.trim_end_matches('/').to_owned();
    let auth = subsonic_auth_params(&payload.creds);

    let response = reqwest::Client::new()
        .get(format!("{base}/rest/search3.view"))
        .query(&auth)
        .query(&[
            ("query", payload.query.as_str()),
            ("songCount", "50"),
            ("artistCount", "0"),
            ("albumCount", "0"),
        ])
        .send()
        .await
        .map_err(|error| {
            tracing::warn!(%error, "subsonic search request failed");
            api_error(StatusCode::BAD_GATEWAY, "subsonic_unreachable")
        })?;

    let json: serde_json::Value = response
        .json()
        .await
        .map_err(|_| api_error(StatusCode::BAD_GATEWAY, "subsonic_parse_error"))?;

    let empty = vec![];
    let songs = json["subsonic-response"]["searchResult3"]["song"]
        .as_array()
        .unwrap_or(&empty);

    let results = songs
        .iter()
        .map(|s| SubsonicSongResult {
            id: s["id"].as_str().unwrap_or_default().to_owned(),
            title: s["title"].as_str().unwrap_or("Unknown").to_owned(),
            artist: s["artist"].as_str().unwrap_or("Unknown").to_owned(),
            album: s["album"].as_str().map(ToOwned::to_owned),
            duration_seconds: s["duration"].as_u64(),
            cover_art_id: s["coverArt"].as_str().map(ToOwned::to_owned),
        })
        .collect();

    Ok(Json(results))
}

pub(crate) async fn import_from_subsonic(
    State(state): State<AppState>,
    session_token: SessionToken,
    Json(payload): Json<SubsonicImportRequest>,
) -> Result<Json<crate::radio::Song>, (StatusCode, Json<ErrorResponse>)> {
    let session = admin_session(&state, session_token.0.as_deref()).await?;

    let base = payload.creds.server_url.trim_end_matches('/').to_owned();
    let auth = subsonic_auth_params(&payload.creds);
    let client = reqwest::Client::new();

    // Fetch song metadata
    let meta: serde_json::Value = client
        .get(format!("{base}/rest/getSong.view"))
        .query(&auth)
        .query(&[("id", payload.song_id.as_str())])
        .send()
        .await
        .map_err(|_| api_error(StatusCode::BAD_GATEWAY, "subsonic_unreachable"))?
        .json()
        .await
        .map_err(|_| api_error(StatusCode::BAD_GATEWAY, "subsonic_parse_error"))?;

    let song_meta = &meta["subsonic-response"]["song"];
    let title = song_meta["title"].as_str().unwrap_or("Unknown").to_owned();
    let artist = song_meta["artist"].as_str().unwrap_or("Unknown").to_owned();
    let album = song_meta["album"].as_str().map(ToOwned::to_owned);
    let duration_seconds = song_meta["duration"].as_i64();
    let cover_art_id = payload
        .cover_art_id
        .as_deref()
        .or_else(|| song_meta["coverArt"].as_str())
        .map(ToOwned::to_owned);

    // Stream audio
    let stream_resp = client
        .get(format!("{base}/rest/stream.view"))
        .query(&auth)
        .query(&[("id", payload.song_id.as_str())])
        .send()
        .await
        .map_err(|_| api_error(StatusCode::BAD_GATEWAY, "subsonic_unreachable"))?;

    if !stream_resp.status().is_success() {
        return Err(api_error(StatusCode::BAD_GATEWAY, "subsonic_stream_failed"));
    }

    let mime_type = stream_resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(';').next().unwrap_or(s).trim().to_owned());

    let audio_bytes = stream_resp
        .bytes()
        .await
        .map_err(|_| api_error(StatusCode::BAD_GATEWAY, "subsonic_stream_failed"))?
        .to_vec();

    let mut song = state
        .radio
        .add_song(
            crate::radio::NewSongUpload {
                filename: None,
                mime_type,
                bytes: audio_bytes,
                title,
                artist,
                album,
                genre: None,
                duration_seconds,
                add_to_queue: payload.add_to_queue.unwrap_or(false),
            },
            &session.account_did,
        )
        .await
        .map_err(internal_api_error)?;

    // Fetch and attach cover art
    if let Some(cover_id) = cover_art_id {
        if let Ok(cover_resp) = client
            .get(format!("{base}/rest/getCoverArt.view"))
            .query(&auth)
            .query(&[("id", cover_id.as_str())])
            .send()
            .await
        {
            if cover_resp.status().is_success() {
                let cover_mime = cover_resp
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .map(ToOwned::to_owned);
                if let Ok(cover_bytes) = cover_resp.bytes().await {
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

    Ok(Json(song))
}

pub(crate) async fn import_from_subsonic_share(
    State(state): State<AppState>,
    session_token: SessionToken,
    Json(payload): Json<SubsonicShareImportRequest>,
) -> Result<Json<crate::radio::Song>, (StatusCode, Json<ErrorResponse>)> {
    let session = admin_session(&state, session_token.0.as_deref()).await?;

    let (base, share_id) = parse_share_url(&payload.share_url)
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "share_url_invalid"))?;

    let client = reqwest::Client::new();

    let html = client
        .get(format!("{base}/share/{share_id}"))
        .send()
        .await
        .map_err(|_| api_error(StatusCode::BAD_GATEWAY, "subsonic_unreachable"))?
        .text()
        .await
        .map_err(|_| api_error(StatusCode::BAD_GATEWAY, "subsonic_parse_error"))?;

    let share_info_raw = extract_share_info(&html)
        .ok_or_else(|| api_error(StatusCode::BAD_GATEWAY, "share_info_missing"))?;
    let share_info: serde_json::Value = serde_json::from_str(&share_info_raw)
        .map_err(|_| api_error(StatusCode::BAD_GATEWAY, "share_info_invalid"))?;

    let track = share_info["tracks"]
        .as_array()
        .and_then(|a| a.first())
        .ok_or_else(|| api_error(StatusCode::BAD_GATEWAY, "share_has_no_tracks"))?;

    let track_id = track["id"]
        .as_str()
        .ok_or_else(|| api_error(StatusCode::BAD_GATEWAY, "share_track_id_missing"))?
        .to_owned();
    let title = track["title"].as_str().unwrap_or("Unknown").to_owned();
    let artist = track["artist"].as_str().unwrap_or("Unknown").to_owned();
    let album = track["album"].as_str().map(ToOwned::to_owned);
    let duration_seconds = track["duration"]
        .as_f64()
        .map(|d| d.round() as i64)
        .or_else(|| track["duration"].as_i64());

    let stream_resp = client
        .get(format!("{base}/share/s/{track_id}"))
        .send()
        .await
        .map_err(|_| api_error(StatusCode::BAD_GATEWAY, "subsonic_unreachable"))?;

    if !stream_resp.status().is_success() {
        return Err(api_error(StatusCode::BAD_GATEWAY, "subsonic_stream_failed"));
    }

    let mime_type = stream_resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(';').next().unwrap_or(s).trim().to_owned());

    let audio_bytes = stream_resp
        .bytes()
        .await
        .map_err(|_| api_error(StatusCode::BAD_GATEWAY, "subsonic_stream_failed"))?
        .to_vec();

    let mut song = state
        .radio
        .add_song(
            crate::radio::NewSongUpload {
                filename: None,
                mime_type,
                bytes: audio_bytes,
                title,
                artist,
                album,
                genre: None,
                duration_seconds,
                add_to_queue: payload.add_to_queue.unwrap_or(false),
            },
            &session.account_did,
        )
        .await
        .map_err(internal_api_error)?;

    if let Ok(cover_resp) = client
        .get(format!("{base}/share/img/{track_id}"))
        .query(&[("size", "600")])
        .send()
        .await
    {
        if cover_resp.status().is_success() {
            let cover_mime = cover_resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(ToOwned::to_owned);
            if let Ok(cover_bytes) = cover_resp.bytes().await {
                if !cover_bytes.is_empty() {
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

    Ok(Json(song))
}
