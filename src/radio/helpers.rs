use tokio::io::AsyncReadExt;
use super::types::NewSongUpload;

pub(crate) fn has_playlist_extension(file_path: &str) -> bool {
    file_path
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
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

pub(crate) async fn file_starts_with_extm3u(file_path: &str) -> bool {
    let mut file = match tokio::fs::File::open(file_path).await {
        Ok(file) => file,
        Err(_) => return false,
    };
    let mut buffer = [0_u8; 256];
    let read = match file.read(&mut buffer).await {
        Ok(read) => read,
        Err(_) => return false,
    };

    String::from_utf8_lossy(&buffer[..read])
        .trim_start_matches('\u{feff}')
        .trim_start()
        .starts_with("#EXTM3U")
}

pub(crate) fn is_unsupported_audio_file(file_path: &str, mime_type: Option<&str>) -> bool {
    has_playlist_extension(file_path) || has_playlist_mime(mime_type)
    // Note: file_starts_with_extm3u is async, called separately
}

pub(crate) fn extension(upload: &NewSongUpload) -> String {
    file_extension(upload.filename.as_deref(), ".audio")
}

pub(crate) fn file_extension(filename: Option<&str>, fallback: &str) -> String {
    filename
        .and_then(|filename| filename.rsplit_once('.').map(|(_, extension)| extension))
        .filter(|extension| !extension.contains('/'))
        .map(|extension| format!(".{extension}"))
        .unwrap_or_else(|| fallback.into())
}

pub(crate) fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}
