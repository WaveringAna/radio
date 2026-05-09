use std::{collections::HashMap, fmt::Write as _};

use axum::{
    Router,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Redirect, Response},
    routing::get,
};
use serde::Deserialize;

use crate::{radio::Song, routes::AppState};

const API_VERSION: &str = "1.16.1";

pub(crate) fn router() -> Router<AppState> {
    Router::new().route("/{action}", get(dispatch))
}

#[derive(Deserialize, Default)]
struct SubsonicQuery {
    id: Option<String>,
    query: Option<String>,
    #[serde(rename = "artistCount")]
    artist_count: Option<usize>,
    #[serde(rename = "albumCount")]
    album_count: Option<usize>,
    #[serde(rename = "songCount")]
    song_count: Option<usize>,
}

async fn dispatch(
    State(state): State<AppState>,
    Path(action): Path<String>,
    Query(q): Query<SubsonicQuery>,
) -> Response {
    match action.trim_end_matches(".view") {
        "ping" => ok_xml(""),
        "getLicense" => ok_xml(
            r#"<license valid="true" email="radio@nekomimi.pet" licenseExpires="2099-12-31T23:59:59"/>"#,
        ),
        "getMusicFolders" => ok_xml(
            r#"<musicFolders><musicFolder id="1" name="radio"/></musicFolders>"#,
        ),
        "getIndexes" | "getArtists" => get_artists(&state).await,
        "getArtist" => get_artist(&state, q.id.as_deref()).await,
        "getAlbumList" | "getAlbumList2" => get_album_list(&state).await,
        "getAlbum" => get_album(&state, q.id.as_deref()).await,
        "getSong" => get_song(&state, q.id.as_deref()).await,
        "stream" | "download" => stream_song(q.id.as_deref()),
        "getCoverArt" => cover_art(q.id.as_deref()),
        "getNowPlaying" => now_playing(&state).await,
        "search2" | "search3" => search(
            &state,
            q.query.as_deref(),
            q.artist_count.unwrap_or(20),
            q.album_count.unwrap_or(20),
            q.song_count.unwrap_or(20),
        )
        .await,
        _ => err_xml(0, "unknown method"),
    }
}

async fn get_artists(state: &AppState) -> Response {
    let songs = match state.radio.songs().await {
        Ok(s) => s,
        Err(error) => {
            tracing::error!(?error, "subsonic: failed to load songs");
            return err_xml(0, "internal error");
        }
    };

    let mut artist_song_count: HashMap<String, usize> = HashMap::new();
    for song in &songs {
        *artist_song_count.entry(song.artist.clone()).or_insert(0) += 1;
    }

    let mut sorted: Vec<_> = artist_song_count.into_iter().collect();
    sorted.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));

    let mut by_letter: HashMap<String, Vec<(String, usize)>> = HashMap::new();
    for (name, count) in sorted {
        let letter = name
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .filter(|s| s.chars().next().map(|c| c.is_alphabetic()).unwrap_or(false))
            .unwrap_or_else(|| "#".into());
        by_letter.entry(letter).or_default().push((name, count));
    }

    let mut letters: Vec<_> = by_letter.keys().cloned().collect();
    letters.sort();

    let mut xml = String::from(r#"<artists ignoredArticles="">"#);
    for letter in letters {
        write!(xml, r#"<index name="{letter}">"#).ok();
        for (name, count) in &by_letter[&letter] {
            write!(
                xml,
                r#"<artist id="ar:{}" name="{}" albumCount="{}"/>"#,
                esc(name),
                esc(name),
                count
            )
            .ok();
        }
        xml.push_str("</index>");
    }
    xml.push_str("</artists>");
    ok_xml(&xml)
}

async fn get_artist(state: &AppState, id: Option<&str>) -> Response {
    let Some(artist_name) = id.and_then(|s| s.strip_prefix("ar:")) else {
        return err_xml(10, "required parameter id is missing");
    };

    let songs = match state.radio.songs().await {
        Ok(s) => s,
        Err(error) => {
            tracing::error!(?error, "subsonic: failed to load songs");
            return err_xml(0, "internal error");
        }
    };

    let artist_songs: Vec<_> = songs.into_iter().filter(|s| s.artist == artist_name).collect();
    if artist_songs.is_empty() {
        return err_xml(70, "artist not found");
    }

    let cover_id = artist_songs.iter().find(|s| s.has_cover).map(|s| s.id.clone());
    let total_duration: i64 = artist_songs.iter().filter_map(|s| s.duration_seconds).sum();

    let mut xml = format!(
        r#"<artist id="ar:{}" name="{}" albumCount="1">"#,
        esc(artist_name),
        esc(artist_name)
    );
    write!(
        xml,
        r#"<album id="ara:{}" name="Songs" artist="{}" songCount="{}" duration="{}""#,
        esc(artist_name),
        esc(artist_name),
        artist_songs.len(),
        total_duration
    )
    .ok();
    if let Some(cid) = cover_id {
        write!(xml, r#" coverArt="{}""#, cid).ok();
    }
    xml.push_str("/></artist>");
    ok_xml(&xml)
}

async fn get_album_list(state: &AppState) -> Response {
    let albums = match state.radio.albums().await {
        Ok(a) => a,
        Err(error) => {
            tracing::error!(?error, "subsonic: failed to load albums");
            return err_xml(0, "internal error");
        }
    };

    let mut xml = String::from("<albumList2>");
    for album in &albums {
        let cover_id = album.tracks.iter().find(|s| s.has_cover).map(|s| s.id.clone());
        let duration: i64 = album.tracks.iter().filter_map(|s| s.duration_seconds).sum();
        let artist = album.tracks.first().map(|s| s.artist.as_str()).unwrap_or("Various");
        write!(
            xml,
            r#"<album id="{}" name="{}" artist="{}" songCount="{}" duration="{}""#,
            album.id,
            esc(&album.title),
            esc(artist),
            album.tracks.len(),
            duration
        )
        .ok();
        if let Some(cid) = cover_id {
            write!(xml, r#" coverArt="{}""#, cid).ok();
        }
        xml.push_str("/>");
    }
    xml.push_str("</albumList2>");
    ok_xml(&xml)
}

async fn get_album(state: &AppState, id: Option<&str>) -> Response {
    let Some(id) = id else {
        return err_xml(10, "required parameter id is missing");
    };

    if let Some(artist_name) = id.strip_prefix("ara:") {
        let songs = match state.radio.songs().await {
            Ok(s) => s,
            Err(error) => {
                tracing::error!(?error, "subsonic: failed to load songs");
                return err_xml(0, "internal error");
            }
        };
        let artist_songs: Vec<_> = songs.into_iter().filter(|s| s.artist == artist_name).collect();
        if artist_songs.is_empty() {
            return err_xml(70, "album not found");
        }
        let duration: i64 = artist_songs.iter().filter_map(|s| s.duration_seconds).sum();
        let cover_id = artist_songs.iter().find(|s| s.has_cover).map(|s| s.id.clone());
        let mut xml = format!(
            r#"<album id="{}" name="Songs" artist="{}" songCount="{}" duration="{}""#,
            esc(id),
            esc(artist_name),
            artist_songs.len(),
            duration
        );
        if let Some(cid) = &cover_id {
            write!(xml, r#" coverArt="{}""#, cid).ok();
        }
        xml.push('>');
        for song in &artist_songs {
            xml.push_str(&song_element(song, id));
        }
        xml.push_str("</album>");
        return ok_xml(&xml);
    }

    let albums = match state.radio.albums().await {
        Ok(a) => a,
        Err(error) => {
            tracing::error!(?error, "subsonic: failed to load albums");
            return err_xml(0, "internal error");
        }
    };

    let Some(album) = albums.iter().find(|a| a.id == id) else {
        return err_xml(70, "album not found");
    };

    let duration: i64 = album.tracks.iter().filter_map(|s| s.duration_seconds).sum();
    let artist = album.tracks.first().map(|s| s.artist.as_str()).unwrap_or("Various");
    let cover_id = album.tracks.iter().find(|s| s.has_cover).map(|s| s.id.clone());

    let mut xml = format!(
        r#"<album id="{}" name="{}" artist="{}" songCount="{}" duration="{}""#,
        album.id,
        esc(&album.title),
        esc(artist),
        album.tracks.len(),
        duration
    );
    if let Some(cid) = &cover_id {
        write!(xml, r#" coverArt="{}""#, cid).ok();
    }
    xml.push('>');
    for song in &album.tracks {
        xml.push_str(&song_element(song, &album.id));
    }
    xml.push_str("</album>");
    ok_xml(&xml)
}

async fn get_song(state: &AppState, id: Option<&str>) -> Response {
    let Some(id) = id else {
        return err_xml(10, "required parameter id is missing");
    };

    let songs = match state.radio.songs().await {
        Ok(s) => s,
        Err(error) => {
            tracing::error!(?error, "subsonic: failed to load songs");
            return err_xml(0, "internal error");
        }
    };

    let Some(song) = songs.iter().find(|s| s.id == id) else {
        return err_xml(70, "song not found");
    };

    ok_xml(&format!("<song {}/>", song_attrs(song, "")))
}

fn stream_song(id: Option<&str>) -> Response {
    match id {
        Some(id) => Redirect::temporary(&format!("/api/songs/{id}/audio")).into_response(),
        None => err_xml(10, "required parameter id is missing"),
    }
}

fn cover_art(id: Option<&str>) -> Response {
    match id {
        Some(id) => Redirect::temporary(&format!("/api/songs/{id}/cover")).into_response(),
        None => err_xml(10, "required parameter id is missing"),
    }
}

async fn now_playing(state: &AppState) -> Response {
    let snapshot = match state.radio.snapshot().await {
        Ok(s) => s,
        Err(error) => {
            tracing::error!(?error, "subsonic: failed to load snapshot");
            return err_xml(0, "internal error");
        }
    };

    let mut xml = String::from("<nowPlaying>");
    if let Some(song) = &snapshot.current_song {
        if snapshot.state.status == "playing" {
            write!(
                xml,
                r#"<entry username="radio" minutesAgo="0" playerId="0" {}/>"#,
                song_attrs(song, "")
            )
            .ok();
        }
    }
    xml.push_str("</nowPlaying>");
    ok_xml(&xml)
}

async fn search(
    state: &AppState,
    query: Option<&str>,
    artist_count: usize,
    album_count: usize,
    song_count: usize,
) -> Response {
    let q = query.unwrap_or("").to_lowercase();

    let songs = match state.radio.songs().await {
        Ok(s) => s,
        Err(error) => {
            tracing::error!(?error, "subsonic: failed to load songs");
            return err_xml(0, "internal error");
        }
    };

    let mut xml = String::from("<searchResult3>");

    let mut seen_artists = std::collections::HashSet::new();
    let mut artist_hits = 0;
    for song in &songs {
        if artist_hits >= artist_count {
            break;
        }
        if song.artist.to_lowercase().contains(&q) && seen_artists.insert(song.artist.clone()) {
            write!(
                xml,
                r#"<artist id="ar:{}" name="{}"/>"#,
                esc(&song.artist),
                esc(&song.artist)
            )
            .ok();
            artist_hits += 1;
        }
    }

    let albums = match state.radio.albums().await {
        Ok(a) => a,
        Err(_) => vec![],
    };
    for album in albums.iter().filter(|a| a.title.to_lowercase().contains(&q)).take(album_count) {
        let artist = album.tracks.first().map(|s| s.artist.as_str()).unwrap_or("Various");
        write!(
            xml,
            r#"<album id="{}" name="{}" artist="{}"/>"#,
            album.id,
            esc(&album.title),
            esc(artist)
        )
        .ok();
    }

    for song in songs
        .iter()
        .filter(|s| {
            s.title.to_lowercase().contains(&q)
                || s.artist.to_lowercase().contains(&q)
                || s.album.as_ref().map(|a| a.to_lowercase().contains(&q)).unwrap_or(false)
        })
        .take(song_count)
    {
        xml.push_str(&format!("<song {}/>", song_attrs(song, "")));
    }

    xml.push_str("</searchResult3>");
    ok_xml(&xml)
}

fn song_element(song: &Song, parent_id: &str) -> String {
    format!("<song {}/>", song_attrs(song, parent_id))
}

fn song_attrs(song: &Song, parent_id: &str) -> String {
    let suffix = mime_suffix(song.mime_type.as_deref());
    let content_type = song.mime_type.as_deref().unwrap_or("audio/mpeg");
    let duration = song.duration_seconds.unwrap_or(0);

    let mut s = format!(
        r#"id="{}" parent="{}" title="{}" artist="{}" duration="{}" suffix="{}" contentType="{}" isVideo="false" type="music""#,
        song.id, parent_id, esc(&song.title), esc(&song.artist), duration, suffix, content_type
    );

    if let Some(album) = &song.album {
        write!(s, r#" album="{}""#, esc(album)).ok();
    }
    if song.has_cover {
        write!(s, r#" coverArt="{}""#, song.id).ok();
    }
    s
}

fn mime_suffix(mime: Option<&str>) -> &'static str {
    match mime {
        Some("audio/mpeg") => "mp3",
        Some("audio/ogg") | Some("audio/vorbis") => "ogg",
        Some("audio/flac") => "flac",
        Some("audio/aac") => "aac",
        Some("audio/wav") => "wav",
        Some("audio/x-m4a") | Some("audio/mp4") => "m4a",
        Some("audio/opus") => "opus",
        _ => "mp3",
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn ok_xml(inner: &str) -> Response {
    let body = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><subsonic-response xmlns="http://subsonic.org/restapi" status="ok" version="{API_VERSION}">{inner}</subsonic-response>"#
    );
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
        body,
    )
        .into_response()
}

fn err_xml(code: u32, msg: &str) -> Response {
    let body = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><subsonic-response xmlns="http://subsonic.org/restapi" status="failed" version="{API_VERSION}"><error code="{code}" message="{}"/></subsonic-response>"#,
        esc(msg)
    );
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
        body,
    )
        .into_response()
}
