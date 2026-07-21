//! Local bulk importer for an existing on-disk music library.
//!
//! Run with `cargo run --release -- import <dir> [--did <did>]`. It walks
//! `<dir>` for audio files, reads their embedded tags (falling back to the
//! `Artist/Album/Track` folder layout), and inserts a `songs` row for each that
//! points at the file *where it already lives* — nothing is copied. Loudness and
//! any missing cover art are filled in by the normal boot-time backfills the
//! next time the server starts, so restart the server after importing.

use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow};

use crate::chat::ChatService;
use crate::db::Database;
use crate::radio::{NewSongUpload, RadioService};
use crate::routes::helpers::{extract_embedded_metadata, parse_filename_metadata};

/// Audio extensions the importer will consider.
const AUDIO_EXTS: &[&str] = &[
    "mp3", "flac", "m4a", "aac", "ogg", "oga", "opus", "wav", "aiff", "aif", "alac", "wma",
];

/// Tallies the outcome of an import run for the closing summary.
#[derive(Default)]
struct Stats {
    imported: usize,
    skipped_duplicate: usize,
    skipped_no_metadata: usize,
    failed: usize,
    covers_set: usize,
}

/// Entry point for the `import` subcommand.
pub(crate) async fn run(
    database_url: &str,
    audio_dir: PathBuf,
    admin_did: &str,
    root: &Path,
) -> anyhow::Result<()> {
    if !root.is_dir() {
        return Err(anyhow!(
            "import path is not a directory: {}",
            root.display()
        ));
    }

    let db = Database::connect(database_url).await?;
    db.prepare().await?;
    let chat = ChatService::new(db.clone());
    // Offline service: no advance loop, so importing never fights the running
    // server for control of the shared station state.
    let radio = RadioService::new_offline(db, audio_dir, chat);

    tracing::info!(root = %root.display(), "scanning library");
    let mut files = Vec::new();
    collect_audio_files(root, &mut files)?;
    files.sort();
    let total = files.len();
    tracing::info!(total, "found audio files; importing (referenced in place)");

    let mut stats = Stats::default();
    for (index, path) in files.iter().enumerate() {
        match import_one(&radio, admin_did, path).await {
            Ok(Outcome::Imported { cover }) => {
                stats.imported += 1;
                if cover {
                    stats.covers_set += 1;
                }
            }
            Ok(Outcome::Duplicate) => stats.skipped_duplicate += 1,
            Ok(Outcome::NoMetadata) => {
                stats.skipped_no_metadata += 1;
                tracing::warn!(path = %path.display(), "skipped: no usable title/artist");
            }
            Err(error) => {
                stats.failed += 1;
                tracing::warn!(path = %path.display(), %error, "failed to import file");
            }
        }

        let done = index + 1;
        if done % 50 == 0 || done == total {
            tracing::info!(
                "progress {done}/{total} — imported {}, dup {}, no-meta {}, failed {}",
                stats.imported,
                stats.skipped_duplicate,
                stats.skipped_no_metadata,
                stats.failed
            );
        }
    }

    // Rebuild album loops once for everything we just added.
    if let Err(error) = radio.sync_albums_after_import().await {
        tracing::warn!(%error, "album sync after import failed (songs still imported)");
    }

    tracing::info!(
        "import complete: {} imported ({} with embedded covers), {} duplicates skipped, {} without metadata, {} failed",
        stats.imported,
        stats.covers_set,
        stats.skipped_duplicate,
        stats.skipped_no_metadata,
        stats.failed
    );
    Ok(())
}

enum Outcome {
    Imported { cover: bool },
    Duplicate,
    NoMetadata,
}

/// Imports a single file, referencing it in place.
async fn import_one(radio: &RadioService, admin_did: &str, path: &Path) -> anyhow::Result<Outcome> {
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("reading {}", path.display()))?;
    let embedded = extract_embedded_metadata(&bytes).await;

    let filename = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_owned();
    let (filename_artist, filename_title) = parse_filename_metadata(&filename);

    // Title/artist prefer embedded tags, then the filename, then the folder
    // layout (`.../<Artist>/<Album>/<file>`).
    let title = embedded
        .title
        .or(filename_title)
        .or_else(|| file_stem(path))
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty());
    let artist = embedded
        .artist
        .or(filename_artist)
        .or_else(|| ancestor_name(path, 2))
        .map(|a| a.trim().to_owned())
        .filter(|a| !a.is_empty());
    let album = embedded
        .album
        .or_else(|| ancestor_name(path, 1))
        .map(|a| a.trim().to_owned())
        .filter(|a| !a.is_empty());

    let (Some(title), Some(artist)) = (title, artist) else {
        return Ok(Outcome::NoMetadata);
    };

    let upload = NewSongUpload {
        filename: Some(filename),
        mime_type: Some(mime_for(path).to_owned()),
        bytes: Vec::new(), // referenced in place — bytes are never stored
        title,
        artist,
        album,
        genre: embedded.genre,
        duration_seconds: embedded.duration_seconds,
        add_to_queue: false,
    };

    let Some(song) = radio
        .add_referenced_song(path.to_path_buf(), upload, admin_did)
        .await?
    else {
        return Ok(Outcome::Duplicate);
    };

    let mut cover = false;
    if let Some((cover_bytes, cover_mime)) = embedded.cover {
        match radio
            .set_song_cover(&song.id, None, Some(cover_mime), cover_bytes)
            .await
        {
            Ok(_) => cover = true,
            Err(error) => {
                tracing::warn!(song_id = %song.id, %error, "failed to store embedded cover")
            }
        }
    }

    Ok(Outcome::Imported { cover })
}

/// Recursively collects audio files under `dir`, skipping hidden entries and
/// the macOS AppleDouble sidecars (`._name`) that litter exfat drives.
fn collect_audio_files(dir: &Path, out: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    let entries =
        std::fs::read_dir(dir).with_context(|| format!("reading dir {}", dir.display()))?;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!(%error, dir = %dir.display(), "skipping unreadable entry");
                continue;
            }
        };
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if file_type.is_dir() {
            collect_audio_files(&path, out)?;
        } else if file_type.is_file() && has_audio_ext(&path) {
            out.push(path);
        }
    }
    Ok(())
}

fn has_audio_ext(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| AUDIO_EXTS.contains(&e.as_str()))
}

fn mime_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("flac") => "audio/flac",
        Some("m4a" | "aac" | "alac") => "audio/mp4",
        Some("ogg" | "oga") => "audio/ogg",
        Some("opus") => "audio/opus",
        Some("wav") => "audio/wav",
        Some("aiff" | "aif") => "audio/aiff",
        Some("wma") => "audio/x-ms-wma",
        _ => "audio/mpeg",
    }
}

fn file_stem(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(ToOwned::to_owned)
}

/// Returns the name of the ancestor directory `up` levels above the file
/// (`up = 1` is the containing folder, `up = 2` its parent).
fn ancestor_name(path: &Path, up: usize) -> Option<String> {
    let mut cur = path.parent()?;
    for _ in 1..up {
        cur = cur.parent()?;
    }
    cur.file_name()
        .and_then(|s| s.to_str())
        .map(ToOwned::to_owned)
}
