pub(crate) struct OnlineLookup {
    pub(crate) cover: Option<(Vec<u8>, String)>,
    pub(crate) genre: Option<String>,
}

pub(crate) async fn fetch_online_genre(
    artist: &str,
    album: Option<&str>,
    title: &str,
) -> Option<String> {
    let Ok(client) = reqwest::Client::builder().user_agent("radio/0.1").build() else {
        return None;
    };

    let (_, genre) = lookup_mbids_and_genre(&client, artist, album, title).await;
    if let Some(g) = genre {
        return Some(g);
    }
    fetch_itunes_genre(&client, artist, title).await
}

pub(crate) async fn fetch_online_metadata(
    artist: &str,
    album: Option<&str>,
    title: &str,
) -> OnlineLookup {
    let Ok(client) = reqwest::Client::builder().user_agent("radio/0.1").build() else {
        return OnlineLookup {
            cover: None,
            genre: None,
        };
    };

    let (mbids, genre) = lookup_mbids_and_genre(&client, artist, album, title).await;

    let genre = match genre {
        Some(g) => Some(g),
        None => fetch_itunes_genre(&client, artist, title).await,
    };

    let mut cover = None;
    for mbid in &mbids {
        if let Some(c) = caa_front_cover(&client, mbid).await {
            cover = Some(c);
            break;
        }
    }

    if cover.is_none() {
        cover = fetch_ytdlp_cover(&client, artist, title).await;
    }

    OnlineLookup { cover, genre }
}

pub(crate) async fn fetch_ytdlp_cover(
    client: &reqwest::Client,
    artist: &str,
    title: &str,
) -> Option<(Vec<u8>, String)> {
    use std::time::Duration;
    use tokio::process::Command;

    let query = format!("ytsearch1:{artist} {title}");
    let output = match tokio::time::timeout(
        Duration::from_secs(45),
        Command::new("yt-dlp")
            .args([
                "--no-update",
                "--no-warnings",
                "--flat-playlist",
                "--playlist-end",
                "1",
                "--socket-timeout",
                "15",
                "--retries",
                "1",
                "--print",
                "%(id)s",
            ])
            .arg(&query)
            .output(),
    )
    .await
    {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            tracing::warn!(%error, "yt-dlp unavailable for cover lookup");
            return None;
        }
        Err(_) => {
            tracing::warn!(%artist, %title, "yt-dlp cover lookup timed out");
            return None;
        }
    };

    if !output.status.success() {
        tracing::debug!(
            %artist,
            %title,
            stderr = %String::from_utf8_lossy(&output.stderr),
            "yt-dlp cover lookup failed"
        );
        return None;
    }

    let video_id = first_nonempty_line(&output.stdout)?;
    if !is_youtube_video_id(video_id) {
        return None;
    }
    let thumbnail_url = format!("https://i.ytimg.com/vi/{video_id}/hqdefault.jpg");
    let response = client.get(thumbnail_url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let mime = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())?
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    if !mime.starts_with("image/") {
        return None;
    }
    if response
        .content_length()
        .is_some_and(|length| length > 20 * 1024 * 1024)
    {
        return None;
    }
    let bytes = response.bytes().await.ok()?;
    if bytes.is_empty() {
        return None;
    }
    Some((bytes.to_vec(), mime))
}

fn first_nonempty_line(stdout: &[u8]) -> Option<&str> {
    std::str::from_utf8(stdout)
        .ok()?
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
}

fn is_youtube_video_id(value: &str) -> bool {
    value.len() == 11
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

async fn fetch_itunes_genre(client: &reqwest::Client, artist: &str, title: &str) -> Option<String> {
    let term = format!("{artist} {title}");
    let resp = client
        .get("https://itunes.apple.com/search")
        .query(&[("term", term.as_str()), ("entity", "song"), ("limit", "10")])
        .send()
        .await
        .ok()?;
    let json: serde_json::Value = resp.json().await.ok()?;
    let results = json["results"].as_array()?;
    if results.is_empty() {
        return None;
    }

    // Prefer a result whose artist and title both match (case-insensitive
    // substring); otherwise fall back to the first result, since iTunes's
    // own ranking is usually sensible.
    let artist_lower = artist.to_lowercase();
    let title_lower = title.to_lowercase();
    let best = results
        .iter()
        .find(|r| {
            let a = r["artistName"].as_str().unwrap_or("").to_lowercase();
            let t = r["trackName"].as_str().unwrap_or("").to_lowercase();
            a.contains(&artist_lower) && t.contains(&title_lower)
        })
        .or_else(|| results.first())?;

    best["primaryGenreName"]
        .as_str()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

async fn lookup_mbids_and_genre(
    client: &reqwest::Client,
    artist: &str,
    album: Option<&str>,
    title: &str,
) -> (Vec<String>, Option<String>) {
    match album {
        Some(album) => musicbrainz_release_lookup(client, artist, album).await,
        None => musicbrainz_recording_lookup(client, artist, title).await,
    }
}

async fn musicbrainz_release_lookup(
    client: &reqwest::Client,
    artist: &str,
    album: &str,
) -> (Vec<String>, Option<String>) {
    let query = format!("artist:\"{artist}\" AND release:\"{album}\"");
    let Ok(resp) = client
        .get("https://musicbrainz.org/ws/2/release")
        .query(&[
            ("query", query.as_str()),
            ("fmt", "json"),
            ("limit", "5"),
            ("inc", "genres tags"),
        ])
        .send()
        .await
    else {
        return (vec![], None);
    };
    let Ok(json) = resp.json::<serde_json::Value>().await else {
        return (vec![], None);
    };
    let releases = json["releases"]
        .as_array()
        .map(|v| v.as_slice())
        .unwrap_or(&[]);
    let genre = releases.iter().find_map(extract_genre_or_tag);
    let ids = releases
        .iter()
        .filter_map(|r| r["id"].as_str().map(|s| s.to_owned()))
        .collect();
    (ids, genre)
}

async fn musicbrainz_recording_lookup(
    client: &reqwest::Client,
    artist: &str,
    title: &str,
) -> (Vec<String>, Option<String>) {
    let query = format!("artist:\"{artist}\" AND recording:\"{title}\"");
    let Ok(resp) = client
        .get("https://musicbrainz.org/ws/2/recording")
        .query(&[
            ("query", query.as_str()),
            ("fmt", "json"),
            ("limit", "5"),
            ("inc", "releases genres tags"),
        ])
        .send()
        .await
    else {
        return (vec![], None);
    };
    let Ok(json) = resp.json::<serde_json::Value>().await else {
        return (vec![], None);
    };
    let recordings = json["recordings"]
        .as_array()
        .map(|v| v.as_slice())
        .unwrap_or(&[]);
    let genre = recordings.iter().find_map(extract_genre_or_tag);
    let ids: Vec<String> = recordings
        .iter()
        .flat_map(|rec| {
            rec["releases"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| r["id"].as_str().map(|s| s.to_owned()))
                .collect::<Vec<_>>()
        })
        .take(5)
        .collect();
    (ids, genre)
}

fn extract_genre_or_tag(entry: &serde_json::Value) -> Option<String> {
    // Prefer curated genres if present.
    if let Some(name) = entry["genres"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|g| g.get("name"))
        .and_then(|n| n.as_str())
    {
        return Some(name.to_owned());
    }
    // Fall back to community tags, picking the one with the highest count.
    let tags = entry["tags"].as_array()?;
    let mut best: Option<(i64, &str)> = None;
    for tag in tags {
        let count = tag.get("count").and_then(|c| c.as_i64()).unwrap_or(0);
        let Some(name) = tag.get("name").and_then(|n| n.as_str()) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        if best.map(|(c, _)| count > c).unwrap_or(true) {
            best = Some((count, name));
        }
    }
    best.map(|(_, n)| n.to_owned())
}

async fn caa_front_cover(client: &reqwest::Client, mbid: &str) -> Option<(Vec<u8>, String)> {
    let resp = client
        .get(format!("https://coverartarchive.org/release/{mbid}/front"))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let mime = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("image/jpeg")
        .to_owned();
    let bytes = resp.bytes().await.ok()?;
    if bytes.is_empty() {
        return None;
    }
    Some((bytes.to_vec(), mime))
}

#[cfg(test)]
mod tests {
    use super::{first_nonempty_line, is_youtube_video_id};

    #[test]
    fn extracts_first_ytdlp_video_id() {
        let output = b"\n8oBV3jPTW4s\nignored\n";
        assert_eq!(first_nonempty_line(output), Some("8oBV3jPTW4s"));
    }

    #[test]
    fn rejects_non_utf8_or_empty_ytdlp_output() {
        assert_eq!(first_nonempty_line(b" \n\t\n"), None);
        assert_eq!(first_nonempty_line(&[0xff]), None);
    }

    #[test]
    fn validates_youtube_video_ids_before_building_thumbnail_urls() {
        assert!(is_youtube_video_id("8oBV3jPTW4s"));
        assert!(is_youtube_video_id("abc_DEF-123"));
        assert!(!is_youtube_video_id("too-short"));
        assert!(!is_youtube_video_id("../not-safe"));
    }
}
