use axum::{Json, extract::{Multipart, State}, http::StatusCode};
use jacquard::{
    CowStr,
    xrpc::XrpcError,
};
use jacquard_axum::{
    ExtractXrpc, XrpcErrorResponse,
    service_auth::{ExtractServiceAuth, VerifiedServiceAuth},
};
use radio_lexicons::pet_nkp::radio::{
    QueueItem as XrpcQueueItem, RadioSnapshot as XrpcRadioSnapshot, RadioState as XrpcRadioState,
    RadioStateStatus as XrpcRadioStateStatus, Song as XrpcSong,
    queue::{
        list::{
            ListError as QueueListError, ListOutput as QueueListOutput,
        },
        modify::{
            ModifyAction as QueueModifyAction, ModifyError as QueueModifyError,
            ModifyOutput as QueueModifyOutput, ModifyRequest as QueueModifyRequest,
        },
    },
    songs::{
        add::{
            AddError as SongsAddError, AddOutput as SongsAddOutput, AddRequest as SongsAddRequest,
        },
        list::{
            ListError as SongsListError, ListOutput as SongsListOutput,
        },
        upload::{
            UploadError as SongsUploadError, UploadOutput as SongsUploadOutput,
        },
    },
};
use serde::Serialize;

use super::AppState;
use super::songs::add_song_from_multipart_upload;
use super::upload::{UrlSongRequest, add_song_from_url_source};

// ── Conversion helpers ──

pub(crate) fn xrpc_typed_error<E>(status: StatusCode, error: E) -> XrpcErrorResponse<E>
where
    E: std::error::Error + jacquard::IntoStatic + Serialize,
{
    XrpcErrorResponse::new(status, XrpcError::Xrpc(error))
}

pub(crate) fn xrpc_message(message: &'static str) -> Option<CowStr<'static>> {
    Some(CowStr::from(message))
}

pub(crate) fn xrpc_cow(value: String) -> CowStr<'static> {
    CowStr::from(value)
}

pub(crate) fn optional_xrpc_cow(value: Option<String>) -> Option<CowStr<'static>> {
    value.map(xrpc_cow)
}

pub(crate) fn optional_decimal(value: Option<f64>) -> Option<CowStr<'static>> {
    value.map(|number| CowStr::from(number.to_string()))
}

pub(crate) fn xrpc_song(song: crate::radio::Song) -> XrpcSong<'static> {
    XrpcSong::new()
        .id(song.id)
        .title(song.title)
        .artist(song.artist)
        .maybe_album(optional_xrpc_cow(song.album))
        .maybe_genre(optional_xrpc_cow(song.genre))
        .maybe_duration_seconds(song.duration_seconds)
        .maybe_mime_type(optional_xrpc_cow(song.mime_type))
        .has_cover(song.has_cover)
        .added_by_did(song.added_by_did)
        .created_at(song.created_at)
        .maybe_loudness_lufs(optional_decimal(song.loudness_lufs))
        .maybe_loudness_peak(optional_decimal(song.loudness_peak))
        .build()
}

pub(crate) fn xrpc_queue_item(item: crate::radio::QueueItem) -> XrpcQueueItem<'static> {
    let song = xrpc_song(crate::radio::Song {
        id: item.song_id.clone(),
        title: item.title.clone(),
        artist: item.artist.clone(),
        album: item.album.clone(),
        genre: item.genre.clone(),
        duration_seconds: item.duration_seconds,
        mime_type: item.mime_type.clone(),
        has_cover: item.has_cover,
        added_by_did: item.added_by_did.clone(),
        created_at: item.created_at,
        loudness_lufs: item.loudness_lufs,
        loudness_peak: item.loudness_peak,
    });

    XrpcQueueItem::new()
        .id(item.id)
        .position(item.position)
        .queued_by_did(item.queued_by_did)
        .song_id(item.song_id)
        .song(song)
        .title(item.title)
        .artist(item.artist)
        .maybe_album(optional_xrpc_cow(item.album))
        .maybe_duration_seconds(item.duration_seconds)
        .added_by_did(item.added_by_did)
        .build()
}

pub(crate) fn xrpc_radio_status(status: String) -> XrpcRadioStateStatus<'static> {
    match status.as_str() {
        "playing" => XrpcRadioStateStatus::Playing,
        "paused" => XrpcRadioStateStatus::Paused,
        "stopped" => XrpcRadioStateStatus::Stopped,
        _ => XrpcRadioStateStatus::Other(CowStr::from(status)),
    }
}

pub(crate) fn xrpc_radio_state(state: crate::radio::RadioState) -> XrpcRadioState<'static> {
    XrpcRadioState::new()
        .maybe_current_song_id(optional_xrpc_cow(state.current_song_id))
        .status(xrpc_radio_status(state.status))
        .maybe_started_at(state.started_at)
        .maybe_paused_at(state.paused_at)
        .position_seconds(state.position_seconds)
        .maybe_updated_by_did(optional_xrpc_cow(state.updated_by_did))
        .build()
}

pub(crate) fn xrpc_radio_snapshot(
    snapshot: crate::radio::RadioSnapshot,
) -> XrpcRadioSnapshot<'static> {
    let current_song = snapshot.current_song.map(xrpc_song);
    XrpcRadioSnapshot::new()
        .state(xrpc_radio_state(snapshot.state))
        .maybe_current_song(current_song.clone())
        .maybe_now_playing(current_song)
        .queue(
            snapshot
                .queue
                .into_iter()
                .map(xrpc_queue_item)
                .collect::<Vec<_>>(),
        )
        .build()
}

// ── Service auth ──

pub(crate) fn service_auth_has_lxm(auth: &VerifiedServiceAuth<'_>, nsid: &str) -> bool {
    auth.lxm().map(|lxm| lxm.as_str() == nsid).unwrap_or(false)
}

/// Why an XRPC caller failed the admin whitelist check.
pub(crate) enum AdminDenied {
    /// The caller authenticated but their DID is not on the admin whitelist.
    NotAdmin,
    /// The whitelist lookup itself failed.
    Internal,
}

/// Verifies the service-auth caller's DID against the admin whitelist, returning
/// the DID on success. Every XRPC endpoint is whitelist-gated; callers map the
/// `AdminDenied` outcome onto their own typed error enum.
pub(crate) async fn xrpc_admin_did(
    state: &AppState,
    auth: &VerifiedServiceAuth<'_>,
    nsid: &str,
) -> Result<String, AdminDenied> {
    let did = auth.did().as_str();
    match state.auth.is_admin_did(did).await {
        Ok(true) => Ok(did.to_owned()),
        Ok(false) => Err(AdminDenied::NotAdmin),
        Err(error) => {
            tracing::error!(?error, nsid, "xrpc admin check failed");
            Err(AdminDenied::Internal)
        }
    }
}

// ── XRPC handlers ──

pub(crate) fn xrpc_songs_upload_api_error(
    error: (StatusCode, Json<super::ErrorResponse>),
) -> XrpcErrorResponse<SongsUploadError<'static>> {
    let (status, Json(error)) = error;
    let typed = match error.error.as_str() {
        "unsupported_audio" | "missing_audio_file" | "invalid_audio_file" => {
            SongsUploadError::UnsupportedAudio(Some(CowStr::from(error.error)))
        }
        _ => SongsUploadError::InvalidRequest(Some(CowStr::from(error.error))),
    };
    xrpc_typed_error(status, typed)
}

// Parameterless query; see original comment for why there is no `ExtractXrpc`.
pub(crate) async fn xrpc_queue_list(
    State(state): State<AppState>,
    ExtractServiceAuth(auth): ExtractServiceAuth,
) -> Result<Json<QueueListOutput<'static>>, XrpcErrorResponse<QueueListError<'static>>> {
    if !service_auth_has_lxm(&auth, "pet.nkp.radio.queue.list") {
        return Err(xrpc_typed_error(
            StatusCode::UNAUTHORIZED,
            QueueListError::AuthenticationRequired(xrpc_message(
                "invalid service auth method binding",
            )),
        ));
    }
    xrpc_admin_did(&state, &auth, "pet.nkp.radio.queue.list")
        .await
        .map_err(|denied| match denied {
            AdminDenied::NotAdmin => xrpc_typed_error(
                StatusCode::FORBIDDEN,
                QueueListError::AdminRequired(xrpc_message("admin privileges required")),
            ),
            AdminDenied::Internal => xrpc_typed_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                QueueListError::InvalidRequest(xrpc_message("internal server error")),
            ),
        })?;

    let snapshot = state.radio.external_snapshot().await.map_err(|error| {
        tracing::error!(?error, "xrpc queue.list failed");
        xrpc_typed_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            QueueListError::InvalidRequest(xrpc_message("internal server error")),
        )
    })?;

    Ok(Json(QueueListOutput {
        snapshot: xrpc_radio_snapshot(snapshot),
        extra_data: None,
    }))
}

// Parameterless query; see `xrpc_queue_list` for why there is no `ExtractXrpc`.
pub(crate) async fn xrpc_songs_list(
    State(state): State<AppState>,
    ExtractServiceAuth(auth): ExtractServiceAuth,
) -> Result<Json<SongsListOutput<'static>>, XrpcErrorResponse<SongsListError<'static>>> {
    if !service_auth_has_lxm(&auth, "pet.nkp.radio.songs.list") {
        return Err(xrpc_typed_error(
            StatusCode::UNAUTHORIZED,
            SongsListError::AuthenticationRequired(xrpc_message(
                "invalid service auth method binding",
            )),
        ));
    }
    xrpc_admin_did(&state, &auth, "pet.nkp.radio.songs.list")
        .await
        .map_err(|denied| match denied {
            AdminDenied::NotAdmin => xrpc_typed_error(
                StatusCode::FORBIDDEN,
                SongsListError::AdminRequired(xrpc_message("admin privileges required")),
            ),
            AdminDenied::Internal => xrpc_typed_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                SongsListError::InvalidRequest(xrpc_message("internal server error")),
            ),
        })?;

    let songs = state.radio.songs().await.map_err(|error| {
        tracing::error!(?error, "xrpc songs.list failed");
        xrpc_typed_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            SongsListError::InvalidRequest(xrpc_message("internal server error")),
        )
    })?;

    Ok(Json(SongsListOutput {
        songs: songs.into_iter().map(xrpc_song).collect(),
        extra_data: None,
    }))
}

pub(crate) async fn xrpc_queue_modify(
    State(state): State<AppState>,
    ExtractServiceAuth(auth): ExtractServiceAuth,
    ExtractXrpc(request): ExtractXrpc<QueueModifyRequest>,
) -> Result<Json<QueueModifyOutput<'static>>, XrpcErrorResponse<QueueModifyError<'static>>> {
    if !service_auth_has_lxm(&auth, "pet.nkp.radio.queue.modify") {
        return Err(xrpc_typed_error(
            StatusCode::UNAUTHORIZED,
            QueueModifyError::AuthenticationRequired(xrpc_message(
                "invalid service auth method binding",
            )),
        ));
    }
    let admin_did = xrpc_admin_did(&state, &auth, "pet.nkp.radio.queue.modify")
        .await
        .map_err(|denied| match denied {
            AdminDenied::NotAdmin => xrpc_typed_error(
                StatusCode::FORBIDDEN,
                QueueModifyError::AdminRequired(xrpc_message("admin privileges required")),
            ),
            AdminDenied::Internal => xrpc_typed_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                QueueModifyError::InvalidRequest(xrpc_message("internal server error")),
            ),
        })?;

    let snapshot = match request.action {
        QueueModifyAction::Enqueue => {
            let song_ids = request.song_ids.ok_or_else(|| {
                xrpc_typed_error(
                    StatusCode::BAD_REQUEST,
                    QueueModifyError::InvalidRequest(xrpc_message(
                        "songIds is required for enqueue",
                    )),
                )
            })?;
            if song_ids.is_empty() {
                return Err(xrpc_typed_error(
                    StatusCode::BAD_REQUEST,
                    QueueModifyError::InvalidRequest(xrpc_message("songIds cannot be empty")),
                ));
            }
            let song_ids: Vec<String> = song_ids
                .iter()
                .map(|song_id| song_id.as_ref().to_owned())
                .collect();
            state
                .radio
                .enqueue_songs(&song_ids, &admin_did)
                .await
                .map_err(|error| {
                    tracing::warn!(?error, "xrpc queue.modify enqueue failed");
                    let message = error.to_string();
                    let typed = if message.contains("song not found") {
                        QueueModifyError::SongNotFound(Some(CowStr::from(message)))
                    } else {
                        QueueModifyError::InvalidRequest(Some(CowStr::from(message)))
                    };
                    xrpc_typed_error(StatusCode::BAD_REQUEST, typed)
                })?
        }
        QueueModifyAction::Remove => {
            let queue_id = request.queue_id.ok_or_else(|| {
                xrpc_typed_error(
                    StatusCode::BAD_REQUEST,
                    QueueModifyError::InvalidRequest(xrpc_message(
                        "queueId is required for remove",
                    )),
                )
            })?;
            state
                .radio
                .remove_queue_item(queue_id.as_ref())
                .await
                .map_err(|error| {
                    tracing::warn!(?error, "xrpc queue.modify remove failed");
                    xrpc_typed_error(
                        StatusCode::BAD_REQUEST,
                        QueueModifyError::QueueItemNotFound(Some(CowStr::from(error.to_string()))),
                    )
                })?
        }
        QueueModifyAction::Clear => state.radio.clear_queue().await.map_err(|error| {
            tracing::warn!(?error, "xrpc queue.modify clear failed");
            xrpc_typed_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                QueueModifyError::InvalidRequest(xrpc_message("internal server error")),
            )
        })?,
        QueueModifyAction::Reorder => {
            let queue_ids = request.queue_ids.ok_or_else(|| {
                xrpc_typed_error(
                    StatusCode::BAD_REQUEST,
                    QueueModifyError::InvalidRequest(xrpc_message(
                        "queueIds is required for reorder",
                    )),
                )
            })?;
            let queue_ids: Vec<String> = queue_ids
                .iter()
                .map(|queue_id| queue_id.as_ref().to_owned())
                .collect();
            state
                .radio
                .reorder_queue(&queue_ids)
                .await
                .map_err(|error| {
                    tracing::warn!(?error, "xrpc queue.modify reorder failed");
                    xrpc_typed_error(
                        StatusCode::BAD_REQUEST,
                        QueueModifyError::InvalidRequest(Some(CowStr::from(error.to_string()))),
                    )
                })?
        }
        QueueModifyAction::Other(action) => {
            return Err(xrpc_typed_error(
                StatusCode::BAD_REQUEST,
                QueueModifyError::InvalidRequest(Some(CowStr::from(format!(
                    "unknown queue action: {action}"
                )))),
            ));
        }
    };

    Ok(Json(QueueModifyOutput {
        snapshot: xrpc_radio_snapshot(snapshot),
        extra_data: None,
    }))
}

pub(crate) async fn xrpc_songs_add(
    State(state): State<AppState>,
    ExtractServiceAuth(auth): ExtractServiceAuth,
    ExtractXrpc(request): ExtractXrpc<SongsAddRequest>,
) -> Result<Json<SongsAddOutput<'static>>, XrpcErrorResponse<SongsAddError<'static>>> {
    if !service_auth_has_lxm(&auth, "pet.nkp.radio.songs.add") {
        return Err(xrpc_typed_error(
            StatusCode::UNAUTHORIZED,
            SongsAddError::AuthenticationRequired(xrpc_message(
                "invalid service auth method binding",
            )),
        ));
    }
    let admin_did = xrpc_admin_did(&state, &auth, "pet.nkp.radio.songs.add")
        .await
        .map_err(|denied| match denied {
            AdminDenied::NotAdmin => xrpc_typed_error(
                StatusCode::FORBIDDEN,
                SongsAddError::AdminRequired(xrpc_message("admin privileges required")),
            ),
            AdminDenied::Internal => xrpc_typed_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                SongsAddError::InvalidRequest(xrpc_message("internal server error")),
            ),
        })?;

    if request.sources.is_empty() {
        return Err(xrpc_typed_error(
            StatusCode::BAD_REQUEST,
            SongsAddError::InvalidRequest(xrpc_message("sources cannot be empty")),
        ));
    }
    if request.sources.len() > 100 {
        return Err(xrpc_typed_error(
            StatusCode::BAD_REQUEST,
            SongsAddError::InvalidRequest(xrpc_message(
                "sources cannot contain more than 100 items",
            )),
        ));
    }

    // Build owned import payloads up front so the spawned task is `'static`, and
    // reject obviously-malformed URLs synchronously so callers still get fast
    // feedback. Anything network-bound (yt-dlp, fetch) is deferred below.
    let mut payloads = Vec::with_capacity(request.sources.len());
    for source in request.sources {
        let url = source.url.as_str().trim().to_owned();
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(xrpc_typed_error(
                StatusCode::BAD_REQUEST,
                SongsAddError::InvalidUrl(xrpc_message("url must be http(s)")),
            ));
        }
        payloads.push(UrlSongRequest {
            url,
            title: source.title.map(|value| value.as_ref().to_owned()),
            artist: source.artist.map(|value| value.as_ref().to_owned()),
            album: source.album.map(|value| value.as_ref().to_owned()),
            add_to_queue: source.add_to_queue,
        });
    }

    // Importing a yt-dlp source (download + transcode) routinely takes longer
    // than the upstream proxy's ~10s headers timeout. Run the import detached.
    let accepted = payloads.len() as i64;
    let import_state = state.clone();
    let importer_did = admin_did.clone();
    tokio::spawn(async move {
        for payload in payloads {
            let url = payload.url.clone();
            if let Err((status, Json(body))) =
                add_song_from_url_source(&import_state, &importer_did, payload).await
            {
                tracing::warn!(%url, ?status, error = %body.error, "background songs.add import failed");
            }
        }
    });

    let snapshot = state.radio.snapshot().await.map_err(|error| {
        tracing::error!(?error, "xrpc songs.add snapshot failed");
        xrpc_typed_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            SongsAddError::InvalidRequest(xrpc_message("internal server error")),
        )
    })?;

    Ok(Json(SongsAddOutput {
        accepted,
        songs: Vec::new(),
        snapshot: xrpc_radio_snapshot(snapshot),
        extra_data: None,
    }))
}

pub(crate) async fn xrpc_songs_upload(
    State(state): State<AppState>,
    ExtractServiceAuth(auth): ExtractServiceAuth,
    multipart: Multipart,
) -> Result<Json<SongsUploadOutput<'static>>, XrpcErrorResponse<SongsUploadError<'static>>> {
    if !service_auth_has_lxm(&auth, "pet.nkp.radio.songs.upload") {
        return Err(xrpc_typed_error(
            StatusCode::UNAUTHORIZED,
            SongsUploadError::AuthenticationRequired(xrpc_message(
                "invalid service auth method binding",
            )),
        ));
    }
    let admin_did = xrpc_admin_did(&state, &auth, "pet.nkp.radio.songs.upload")
        .await
        .map_err(|denied| match denied {
            AdminDenied::NotAdmin => xrpc_typed_error(
                StatusCode::FORBIDDEN,
                SongsUploadError::AdminRequired(xrpc_message("admin privileges required")),
            ),
            AdminDenied::Internal => xrpc_typed_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                SongsUploadError::InvalidRequest(xrpc_message("internal server error")),
            ),
        })?;

    let song = add_song_from_multipart_upload(&state, &admin_did, multipart)
        .await
        .map_err(xrpc_songs_upload_api_error)?;

    let snapshot = state.radio.snapshot().await.map_err(|error| {
        tracing::error!(?error, "xrpc songs.upload snapshot failed");
        xrpc_typed_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            SongsUploadError::InvalidRequest(xrpc_message("internal server error")),
        )
    })?;

    Ok(Json(SongsUploadOutput {
        songs: vec![xrpc_song(song)],
        snapshot: xrpc_radio_snapshot(snapshot),
        extra_data: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::{xrpc_radio_snapshot};
    use crate::radio::{QueueItem, RadioSnapshot, RadioState, Song};

    #[test]
    fn xrpc_snapshot_includes_queue_song_duration_and_now_playing_alias() {
        let song = Song {
            id: "song-1".into(),
            title: "title".into(),
            artist: "artist".into(),
            album: Some("album".into()),
            genre: Some("genre".into()),
            duration_seconds: Some(123),
            mime_type: Some("audio/mpeg".into()),
            has_cover: true,
            added_by_did: "did:plc:uploader".into(),
            created_at: 42,
            loudness_lufs: Some(-14.0),
            loudness_peak: Some(-1.5),
        };
        let snapshot = RadioSnapshot {
            state: RadioState {
                current_song_id: Some(song.id.clone()),
                status: "playing".into(),
                started_at: Some(100),
                paused_at: None,
                position_seconds: 0,
                updated_by_did: Some("did:plc:admin".into()),
            },
            current_song: Some(song.clone()),
            queue: vec![QueueItem {
                id: "queue-1".into(),
                position: 1,
                queued_by_did: "did:plc:admin".into(),
                song_id: song.id.clone(),
                title: song.title.clone(),
                artist: song.artist.clone(),
                album: song.album.clone(),
                genre: song.genre.clone(),
                duration_seconds: song.duration_seconds,
                mime_type: song.mime_type.clone(),
                has_cover: song.has_cover,
                added_by_did: song.added_by_did.clone(),
                created_at: song.created_at,
                loudness_lufs: song.loudness_lufs,
                loudness_peak: song.loudness_peak,
            }],
        };

        let value = serde_json::to_value(xrpc_radio_snapshot(snapshot)).unwrap();

        assert_eq!(value["nowPlaying"]["title"], "title");
        assert_eq!(value["queue"][0]["title"], "title");
        assert_eq!(value["queue"][0]["durationSeconds"], 123);
        assert_eq!(value["queue"][0]["song"]["durationSeconds"], 123);
    }
}
