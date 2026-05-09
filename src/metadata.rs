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
    genre
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

    let mut cover = None;
    for mbid in &mbids {
        if let Some(c) = caa_front_cover(&client, mbid).await {
            cover = Some(c);
            break;
        }
    }

    OnlineLookup { cover, genre }
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
            ("inc", "genres"),
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
    let genre = releases.first().and_then(|r| {
        r["genres"]
            .as_array()?
            .first()?
            .get("name")?
            .as_str()
            .map(|s| s.to_owned())
    });
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
            ("inc", "releases genres"),
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
    let genre = recordings.first().and_then(|r| {
        r["genres"]
            .as_array()?
            .first()?
            .get("name")?
            .as_str()
            .map(|s| s.to_owned())
    });
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
