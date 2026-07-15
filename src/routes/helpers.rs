use std::io::{BufReader, Cursor};

use axum::{Json, http::StatusCode};

use super::{ErrorResponse, api_error};
use crate::radio::NewSongUpload;

#[derive(Clone, Debug)]
pub(crate) struct PlaylistEntry {
    pub(crate) location: String,
    pub(crate) title: Option<String>,
    pub(crate) artist: Option<String>,
}

pub(crate) struct EmbeddedMetadata {
    pub(crate) cover: Option<(Vec<u8>, String)>,
    pub(crate) genre: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) artist: Option<String>,
    pub(crate) album: Option<String>,
    pub(crate) duration_seconds: Option<i64>,
}

impl EmbeddedMetadata {
    pub(crate) fn empty() -> Self {
        Self {
            cover: None,
            genre: None,
            title: None,
            artist: None,
            album: None,
            duration_seconds: None,
        }
    }
}

pub(crate) fn valid_viewer_id(viewer_id: &str) -> bool {
    !viewer_id.is_empty()
        && viewer_id.len() <= super::MAX_VIEWER_ID_LEN
        && viewer_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

pub(crate) fn valid_listener_did(did: &str) -> bool {
    !did.is_empty()
        && did.len() <= super::MAX_VIEWER_ID_LEN
        && did.starts_with("did:")
        && did
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'-' | b'_' | b'.'))
}

pub(crate) fn parse_byte_range(range_header: &str, total_len: u64) -> Option<(u64, u64)> {
    if total_len == 0 {
        return None;
    }

    let bytes_spec = range_header.trim().strip_prefix("bytes=")?;
    if bytes_spec.contains(',') {
        return None;
    }

    let (start_text, end_text) = bytes_spec.split_once('-')?;
    if start_text.is_empty() {
        let suffix_len = end_text.parse::<u64>().ok()?;
        if suffix_len == 0 {
            return None;
        }
        let clamped = suffix_len.min(total_len);
        let start = total_len.saturating_sub(clamped);
        return Some((start, total_len - 1));
    }

    let start = start_text.parse::<u64>().ok()?;
    if start >= total_len {
        return None;
    }

    let end = if end_text.is_empty() {
        total_len - 1
    } else {
        let parsed_end = end_text.parse::<u64>().ok()?;
        if parsed_end < start {
            return None;
        }
        parsed_end.min(total_len - 1)
    };

    Some((start, end))
}

pub(crate) fn reject_unsupported_audio_upload(
    filename: Option<&str>,
    mime_type: Option<&str>,
    bytes: &[u8],
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    if is_playlist_upload(filename, mime_type, bytes) {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "playlist_requires_batch_import",
        ));
    }
    Ok(())
}

pub(crate) fn is_playlist_upload(
    filename: Option<&str>,
    mime_type: Option<&str>,
    bytes: &[u8],
) -> bool {
    has_playlist_extension(filename) || has_playlist_mime(mime_type) || has_m3u_header(bytes)
}

pub(crate) fn has_playlist_extension(filename: Option<&str>) -> bool {
    filename
        .and_then(|name| name.rsplit_once('.').map(|(_, extension)| extension))
        .map(|extension| extension.to_ascii_lowercase())
        .is_some_and(|extension| matches!(extension.as_str(), "m3u" | "m3u8" | "pls" | "xspf"))
}

pub(crate) fn has_playlist_mime(mime_type: Option<&str>) -> bool {
    mime_type
        .and_then(|mime| mime.split(';').next())
        .map(|mime| mime.trim().to_ascii_lowercase())
        .is_some_and(|mime| {
            matches!(
                mime.as_str(),
                "application/vnd.apple.mpegurl"
                    | "application/x-mpegurl"
                    | "audio/mpegurl"
                    | "audio/x-mpegurl"
                    | "audio/m3u"
                    | "audio/x-m3u"
                    | "application/pls+xml"
            )
        })
}

pub(crate) fn has_m3u_header(bytes: &[u8]) -> bool {
    let head_len = bytes.len().min(256);
    String::from_utf8_lossy(&bytes[..head_len])
        .trim_start_matches('\u{feff}')
        .trim_start()
        .starts_with("#EXTM3U")
}

pub(crate) fn parse_m3u(bytes: &[u8]) -> Vec<PlaylistEntry> {
    let text = String::from_utf8_lossy(bytes);
    let mut entries = Vec::new();
    let mut label: Option<String> = None;

    for raw_line in text.lines() {
        let line = raw_line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() {
            continue;
        }
        if let Some(extinf) = line.strip_prefix("#EXTINF:") {
            label = extinf
                .split_once(',')
                .map(|(_, value)| value.trim().to_owned())
                .filter(|value| !value.is_empty());
            continue;
        }
        if line.starts_with('#') {
            continue;
        }

        let (artist, title) = playlist_label_metadata(label.take().as_deref());
        entries.push(PlaylistEntry {
            location: line.to_owned(),
            title,
            artist,
        });
    }

    entries
}

pub(crate) fn playlist_label_metadata(label: Option<&str>) -> (Option<String>, Option<String>) {
    let Some(label) = label.map(str::trim).filter(|label| !label.is_empty()) else {
        return (None, None);
    };
    if let Some((artist, title)) = label.split_once(" - ") {
        let artist = artist.trim();
        let title = title.trim();
        if !artist.is_empty() && !title.is_empty() {
            return (Some(artist.to_owned()), Some(title.to_owned()));
        }
    }
    (None, Some(label.to_owned()))
}

pub(crate) fn playlist_entry_url(
    base_url: &reqwest::Url,
    location: &str,
) -> Result<reqwest::Url, (StatusCode, Json<ErrorResponse>)> {
    let entry_url = base_url
        .join(location)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "invalid_playlist_entry"))?;
    if !matches!(entry_url.scheme(), "http" | "https") {
        return Err(api_error(StatusCode::BAD_REQUEST, "invalid_playlist_entry"));
    }
    if !entry_url.username().is_empty() || entry_url.password().is_some() {
        return Err(api_error(StatusCode::BAD_REQUEST, "invalid_playlist_entry"));
    }
    if entry_url.origin().ascii_serialization() != base_url.origin().ascii_serialization() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "cross_origin_playlist_entry",
        ));
    }
    Ok(entry_url)
}

pub(crate) fn parse_share_url(input: &str) -> Option<(String, String)> {
    let trimmed = input.trim();
    let without_fragment = trimmed.split('#').next().unwrap_or(trimmed);
    let without_query = without_fragment
        .split('?')
        .next()
        .unwrap_or(without_fragment);
    let url = reqwest::Url::parse(without_query).ok()?;
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let host = url.host_str()?;
    let port = url.port().map(|p| format!(":{p}")).unwrap_or_default();
    let base = format!("{scheme}://{host}{port}");

    let segments: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    let share_idx = segments.iter().rposition(|s| *s == "share")?;
    let id = segments.get(share_idx + 1)?.to_string();
    if id.is_empty() {
        return None;
    }
    Some((base, id))
}

pub(crate) fn extract_share_info(html: &str) -> Option<String> {
    let needle = "window.SHARE_INFO";
    let start = html.find(needle)? + needle.len();
    let after = &html[start..];
    let eq = after.find('=')?;
    let after_eq = after[eq + 1..].trim_start();
    if !after_eq.starts_with('"') {
        return None;
    }
    let body = &after_eq[1..];
    let line_end = body.find('\n').unwrap_or(body.len());
    let line = body[..line_end].trim_end();
    let line = line.trim_end_matches(';');
    let line = line.trim_end();
    let inner = line.strip_suffix('"')?;
    Some(html_decode(inner))
}

pub(crate) fn html_decode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        if let Some(semi) = tail.find(';') {
            let entity = &tail[1..semi];
            let replacement = match entity {
                "quot" => Some('"'),
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "apos" => Some('\''),
                _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                    u32::from_str_radix(&entity[2..], 16)
                        .ok()
                        .and_then(char::from_u32)
                }
                _ if entity.starts_with('#') => {
                    entity[1..].parse::<u32>().ok().and_then(char::from_u32)
                }
                _ => None,
            };
            if let Some(c) = replacement {
                out.push(c);
                rest = &tail[semi + 1..];
                continue;
            }
        }
        out.push('&');
        rest = &tail[1..];
    }
    out.push_str(rest);
    out
}

pub(crate) fn parse_filename_metadata(filename: &str) -> (Option<String>, Option<String>) {
    let stem = std::path::Path::new(filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(filename);
    let stripped = {
        let after_digits = stem.trim_start_matches(|c: char| c.is_ascii_digit());
        if after_digits.len() < stem.len() {
            after_digits
                .trim_start_matches(|c: char| c == '.' || c == '-' || c == '_' || c.is_whitespace())
                .trim()
        } else {
            stem.trim()
        }
    };
    if let Some((artist, title)) = stripped.split_once(" - ") {
        let artist = artist.trim();
        let title = title.trim();
        if !artist.is_empty() && !title.is_empty() {
            return (Some(artist.to_owned()), Some(title.to_owned()));
        }
    }
    if stripped.is_empty() {
        (None, None)
    } else {
        (None, Some(stripped.to_owned()))
    }
}

pub(crate) async fn parse_song_upload(
    mut multipart: axum::extract::Multipart,
) -> Result<NewSongUpload, (StatusCode, Json<ErrorResponse>)> {
    let mut filename = None;
    let mut mime_type = None;
    let mut bytes = None;
    let mut title = None;
    let mut artist = None;
    let mut album = None;
    let mut genre = None;
    let mut duration_seconds = None;
    let mut add_to_queue = false;

    while let Some(field) = multipart.next_field().await.map_err(|error| {
        tracing::error!(?error, "failed to read multipart field");
        api_error(StatusCode::BAD_REQUEST, "invalid_multipart")
    })? {
        let name = field.name().unwrap_or_default().to_owned();
        match name.as_str() {
            "file" => {
                filename = field.file_name().map(ToOwned::to_owned);
                mime_type = field.content_type().map(ToString::to_string);
                bytes = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "invalid_audio_file"))?
                        .to_vec(),
                );
            }
            "title" => title = Some(field.text().await.unwrap_or_default()),
            "artist" => artist = Some(field.text().await.unwrap_or_default()),
            "album" => album = Some(field.text().await.unwrap_or_default()),
            "genre" => genre = Some(field.text().await.unwrap_or_default()),
            "durationSeconds" => {
                duration_seconds = field
                    .text()
                    .await
                    .unwrap_or_default()
                    .parse::<i64>()
                    .ok()
                    .filter(|duration| *duration > 0)
            }
            "addToQueue" => add_to_queue = field.text().await.unwrap_or_default() == "true",
            _ => {}
        }
    }

    let title = title
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_default();
    let artist = artist
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_default();
    let bytes = bytes.ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "missing_audio_file"))?;
    let genre = genre.map(|g| g.trim().to_owned()).filter(|g| !g.is_empty());

    Ok(NewSongUpload {
        filename,
        mime_type,
        bytes,
        title,
        artist,
        album,
        genre,
        duration_seconds,
        add_to_queue,
    })
}

pub(crate) async fn extract_embedded_metadata(bytes: &[u8]) -> EmbeddedMetadata {
    let bytes = bytes.to_vec();
    tokio::task::spawn_blocking(move || -> EmbeddedMetadata {
        use lofty::file::AudioFile;
        use lofty::prelude::*;
        use lofty::probe::Probe;
        let cursor = BufReader::new(Cursor::new(bytes));
        let Ok(probe) = Probe::new(cursor).guess_file_type() else {
            return EmbeddedMetadata::empty();
        };
        let Ok(tagged) = probe.read() else {
            return EmbeddedMetadata::empty();
        };
        let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
        let cover = tag.and_then(|t| t.pictures().first()).map(|pic| {
            let mime = pic
                .mime_type()
                .map(|m| m.to_string())
                .unwrap_or_else(|| "image/jpeg".to_owned());
            (pic.data().to_vec(), mime)
        });
        let trim_owned = |value: std::borrow::Cow<'_, str>| {
            let trimmed = value.trim().to_owned();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        };
        let genre = tag.and_then(|t| t.genre()).and_then(trim_owned);
        let title = tag.and_then(|t| t.title()).and_then(trim_owned);
        let artist = tag.and_then(|t| t.artist()).and_then(trim_owned);
        let album = tag.and_then(|t| t.album()).and_then(trim_owned);
        let duration_seconds = tagged
            .properties()
            .duration()
            .as_secs()
            .try_into()
            .ok()
            .filter(|duration: &i64| *duration > 0);
        EmbeddedMetadata {
            cover,
            genre,
            title,
            artist,
            album,
            duration_seconds,
        }
    })
    .await
    .unwrap_or_else(|_| EmbeddedMetadata::empty())
}

#[cfg(test)]
mod tests {
    use super::{
        has_m3u_header, is_playlist_upload, parse_byte_range, parse_m3u, playlist_entry_url,
        reject_unsupported_audio_upload,
    };

    #[test]
    fn parses_open_ended_byte_range() {
        assert_eq!(parse_byte_range("bytes=10-", 100), Some((10, 99)));
    }

    #[test]
    fn parses_suffix_byte_range() {
        assert_eq!(parse_byte_range("bytes=-25", 100), Some((75, 99)));
    }

    #[test]
    fn rejects_invalid_ranges() {
        assert_eq!(parse_byte_range("bytes=101-120", 100), None);
        assert_eq!(parse_byte_range("bytes=30-20", 100), None);
        assert_eq!(parse_byte_range("bytes=0-1,5-6", 100), None);
    }

    #[test]
    fn detects_playlist_uploads_by_extension_mime_or_body() {
        assert!(is_playlist_upload(
            Some("mix.M3U8"),
            Some("audio/flac"),
            b"not a playlist"
        ));
        assert!(is_playlist_upload(
            None,
            Some("application/vnd.apple.mpegurl; charset=utf-8"),
            b"anything"
        ));
        assert!(is_playlist_upload(
            None,
            Some("audio/flac"),
            b"\xef\xbb\xbf#EXTM3U\ntrack.flac"
        ));
        assert!(has_m3u_header(b"   #EXTM3U\ntrack.flac"));
        assert!(!is_playlist_upload(
            Some("track.flac"),
            Some("audio/flac"),
            b"fLaC data"
        ));
    }

    #[test]
    fn rejects_playlist_when_single_audio_upload_path_is_used() {
        let result = reject_unsupported_audio_upload(
            Some("poison.m3u8"),
            Some("audio/x-mpegurl"),
            b"#EXTM3U\ntrack.flac",
        );
        assert!(result.is_err());
    }

    #[test]
    fn parses_m3u_entries_with_metadata_comments_and_bom() {
        let entries = parse_m3u(
            b"\xef\xbb\xbf#EXTM3U\n# comment\n#EXTINF:238,artist - first\n01. first.flac\n\n#EXTINF:42,second\nhttps://example.test/second.mp3\n",
        );
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].location, "01. first.flac");
        assert_eq!(entries[0].artist.as_deref(), Some("artist"));
        assert_eq!(entries[0].title.as_deref(), Some("first"));
        assert_eq!(entries[1].location, "https://example.test/second.mp3");
        assert_eq!(entries[1].title.as_deref(), Some("second"));
    }

    #[test]
    fn resolves_playlist_entries_only_within_same_origin() {
        let base = reqwest::Url::parse("https://music.example.test/albums/list.m3u8").unwrap();

        let relative = playlist_entry_url(&base, "../tracks/01.flac").ok().unwrap();
        assert_eq!(
            relative.as_str(),
            "https://music.example.test/tracks/01.flac"
        );

        let same_origin_absolute =
            playlist_entry_url(&base, "https://music.example.test/cdn/02.mp3")
                .ok()
                .unwrap();
        assert_eq!(
            same_origin_absolute.as_str(),
            "https://music.example.test/cdn/02.mp3"
        );

        assert!(playlist_entry_url(&base, "https://evil.example.test/steal.mp3").is_err());
        assert!(playlist_entry_url(&base, "file:///etc/passwd").is_err());
        assert!(
            playlist_entry_url(&base, "https://user:pass@music.example.test/private.mp3").is_err()
        );
    }
}
