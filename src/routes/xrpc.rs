use axum::{
    Json,
    extract::{Multipart, State},
    http::StatusCode,
};
use jacquard::{CowStr, xrpc::XrpcError};
use jacquard_axum::{
    ExtractXrpc, XrpcErrorResponse,
    service_auth::{ExtractServiceAuth, VerifiedServiceAuth},
};
use radio_lexicons::pet_nkp::radio::{
    AdminPermission as XrpcAdminPermission, AdminPermissions as XrpcAdminPermissions,
    ChatBan as XrpcChatBan, ChatMessage as XrpcChatMessage, ChatMessageKind as XrpcChatMessageKind,
    Playlist as XrpcPlaylist, QueueItem as XrpcQueueItem, RadioAlbum as XrpcRadioAlbum,
    RadioSnapshot as XrpcRadioSnapshot, RadioState as XrpcRadioState,
    RadioStateStatus as XrpcRadioStateStatus, Song as XrpcSong,
    SubsonicSongResult as XrpcSubsonicSongResult,
    admin::{
        modify::{
            ModifyAction as AdminModifyAction, ModifyError as AdminModifyError,
            ModifyOutput as AdminModifyOutput, ModifyRequest as AdminModifyRequest,
        },
        permissions::{
            PermissionsError as AdminPermissionsError, PermissionsOutput as AdminPermissionsOutput,
        },
    },
    albums::{
        list::{ListError as AlbumsListError, ListOutput as AlbumsListOutput},
        modify::{
            ModifyAction as AlbumsModifyAction, ModifyError as AlbumsModifyError,
            ModifyOutput as AlbumsModifyOutput, ModifyRequest as AlbumsModifyRequest,
        },
    },
    chat::{
        bans::{
            list::{ListError as ChatBansListError, ListOutput as ChatBansListOutput},
            modify::{
                ModifyAction as ChatBansModifyAction, ModifyError as ChatBansModifyError,
                ModifyOutput as ChatBansModifyOutput, ModifyRequest as ChatBansModifyRequest,
            },
        },
        messages::modify::{
            ModifyAction as ChatMessagesModifyAction, ModifyError as ChatMessagesModifyError,
            ModifyOutput as ChatMessagesModifyOutput, ModifyRequest as ChatMessagesModifyRequest,
        },
        send::{
            SendError as ChatSendError, SendOutput as ChatSendOutput,
            SendRequest as ChatSendRequest,
        },
    },
    control::{
        ControlAction as XrpcControlAction, ControlError, ControlIntent, ControlOutput,
        ControlRequest,
    },
    playlists::{
        list::{ListError as PlaylistsListError, ListOutput as PlaylistsListOutput},
        modify::{
            ModifyAction as PlaylistsModifyAction, ModifyError as PlaylistsModifyError,
            ModifyOutput as PlaylistsModifyOutput, ModifyRequest as PlaylistsModifyRequest,
        },
    },
    queue::{
        list::{ListError as QueueListError, ListOutput as QueueListOutput},
        modify::{
            ModifyAction as QueueModifyAction, ModifyError as QueueModifyError,
            ModifyOutput as QueueModifyOutput, ModifyRequest as QueueModifyRequest,
        },
    },
    songs::{
        add::{
            AddError as SongsAddError, AddOutput as SongsAddOutput, AddRequest as SongsAddRequest,
        },
        cover::{CoverError as SongsCoverError, CoverOutput as SongsCoverOutput},
        list::{ListError as SongsListError, ListOutput as SongsListOutput},
        modify::{
            ModifyAction as SongsModifyAction, ModifyError as SongsModifyError,
            ModifyOutput as SongsModifyOutput, ModifyRequest as SongsModifyRequest,
        },
        upload::{UploadError as SongsUploadError, UploadOutput as SongsUploadOutput},
    },
    subsonic::{
        import::{
            ImportError as SubsonicImportError, ImportOutput as SubsonicImportOutput,
            ImportRequest as SubsonicImportRequest, ImportSource,
        },
        search::{
            SearchError as SubsonicSearchError, SearchOutput as SubsonicSearchOutput,
            SearchRequest as SubsonicSearchRequest,
        },
    },
};
use serde::Serialize;

use super::AppState;
use super::admin::admin_permissions;
use super::helpers::valid_listener_did;
use super::songs::add_song_from_multipart_upload;
use super::upload::{UrlSongRequest, add_song_from_url_source};
use crate::chat::MAX_CHAT_BODY_LEN;
use crate::radio::{RadioControlAction, SongMetadataUpdate};

pub(crate) const XRPC_QUEUE_LIST_NSID: &str = "pet.nkp.radio.queue.list";
pub(crate) const XRPC_QUEUE_MODIFY_NSID: &str = "pet.nkp.radio.queue.modify";
pub(crate) const XRPC_SONGS_LIST_NSID: &str = "pet.nkp.radio.songs.list";
pub(crate) const XRPC_SONGS_ADD_NSID: &str = "pet.nkp.radio.songs.add";
pub(crate) const XRPC_SONGS_UPLOAD_NSID: &str = "pet.nkp.radio.songs.upload";
pub(crate) const XRPC_SONGS_COVER_NSID: &str = "pet.nkp.radio.songs.cover";
pub(crate) const XRPC_SONGS_MODIFY_NSID: &str = "pet.nkp.radio.songs.modify";
pub(crate) const XRPC_ADMIN_PERMISSIONS_NSID: &str = "pet.nkp.radio.admin.permissions";
pub(crate) const XRPC_ADMIN_MODIFY_NSID: &str = "pet.nkp.radio.admin.modify";
pub(crate) const XRPC_CONTROL_NSID: &str = "pet.nkp.radio.control";
pub(crate) const XRPC_ALBUMS_LIST_NSID: &str = "pet.nkp.radio.albums.list";
pub(crate) const XRPC_ALBUMS_MODIFY_NSID: &str = "pet.nkp.radio.albums.modify";
pub(crate) const XRPC_PLAYLISTS_LIST_NSID: &str = "pet.nkp.radio.playlists.list";
pub(crate) const XRPC_PLAYLISTS_MODIFY_NSID: &str = "pet.nkp.radio.playlists.modify";
pub(crate) const XRPC_CHAT_SEND_NSID: &str = "pet.nkp.radio.chat.send";
pub(crate) const XRPC_CHAT_BANS_LIST_NSID: &str = "pet.nkp.radio.chat.bans.list";
pub(crate) const XRPC_CHAT_BANS_MODIFY_NSID: &str = "pet.nkp.radio.chat.bans.modify";
pub(crate) const XRPC_CHAT_MESSAGES_MODIFY_NSID: &str = "pet.nkp.radio.chat.messages.modify";
pub(crate) const XRPC_SUBSONIC_SEARCH_NSID: &str = "pet.nkp.radio.subsonic.search";
pub(crate) const XRPC_SUBSONIC_IMPORT_NSID: &str = "pet.nkp.radio.subsonic.import";

#[cfg(test)]
pub(crate) const ADMIN_XRPC_METHODS: &[&str] = &[
    XRPC_QUEUE_MODIFY_NSID,
    XRPC_SONGS_ADD_NSID,
    XRPC_SONGS_UPLOAD_NSID,
    XRPC_SONGS_COVER_NSID,
    XRPC_SONGS_MODIFY_NSID,
    XRPC_ADMIN_PERMISSIONS_NSID,
    XRPC_ADMIN_MODIFY_NSID,
    XRPC_CONTROL_NSID,
    XRPC_ALBUMS_LIST_NSID,
    XRPC_ALBUMS_MODIFY_NSID,
    XRPC_PLAYLISTS_LIST_NSID,
    XRPC_PLAYLISTS_MODIFY_NSID,
    XRPC_CHAT_BANS_LIST_NSID,
    XRPC_CHAT_BANS_MODIFY_NSID,
    XRPC_CHAT_MESSAGES_MODIFY_NSID,
    XRPC_SUBSONIC_SEARCH_NSID,
    XRPC_SUBSONIC_IMPORT_NSID,
];

#[cfg(test)]
pub(crate) const LISTENER_XRPC_METHODS: &[&str] = &[
    XRPC_QUEUE_LIST_NSID,
    XRPC_SONGS_LIST_NSID,
    XRPC_CHAT_SEND_NSID,
];

#[cfg(test)]
pub(crate) fn is_admin_xrpc_method(nsid: &str) -> bool {
    ADMIN_XRPC_METHODS.contains(&nsid)
}

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

pub(crate) fn xrpc_radio_album(album: crate::radio::RadioAlbum) -> XrpcRadioAlbum<'static> {
    XrpcRadioAlbum {
        id: xrpc_cow(album.id),
        title: xrpc_cow(album.title),
        tracks: album.tracks.into_iter().map(xrpc_song).collect(),
        extra_data: None,
    }
}

pub(crate) fn xrpc_playlist(playlist: crate::radio::Playlist) -> XrpcPlaylist<'static> {
    XrpcPlaylist {
        id: xrpc_cow(playlist.id),
        name: xrpc_cow(playlist.name),
        created_at: playlist.created_at,
        tracks: playlist.tracks.into_iter().map(xrpc_song).collect(),
        extra_data: None,
    }
}

pub(crate) fn xrpc_chat_message(message: crate::chat::ChatMessage) -> XrpcChatMessage<'static> {
    XrpcChatMessage {
        id: xrpc_cow(message.id),
        sender_did: xrpc_cow(message.sender_did),
        body: xrpc_cow(message.body),
        created_at: message.created_at,
        kind: match message.kind.as_str() {
            "user" => XrpcChatMessageKind::User,
            "now_playing" => XrpcChatMessageKind::NowPlaying,
            _ => XrpcChatMessageKind::Other(xrpc_cow(message.kind)),
        },
        extra_data: None,
    }
}

pub(crate) fn xrpc_chat_ban(ban: crate::chat::ChatBan) -> XrpcChatBan<'static> {
    XrpcChatBan {
        did: xrpc_cow(ban.did),
        banned_by_did: xrpc_cow(ban.banned_by_did),
        reason: optional_xrpc_cow(ban.reason),
        created_at: ban.created_at,
        extra_data: None,
    }
}

pub(crate) fn xrpc_admin_permissions(
    whitelisted_dids: Vec<String>,
) -> XrpcAdminPermissions<'static> {
    XrpcAdminPermissions {
        whitelisted_dids: whitelisted_dids.into_iter().map(xrpc_cow).collect(),
        permissions: admin_permissions()
            .into_iter()
            .map(|permission| XrpcAdminPermission {
                key: CowStr::from(permission.key),
                description: CowStr::from(permission.description),
                extra_data: None,
            })
            .collect(),
        extra_data: None,
    }
}

pub(crate) fn xrpc_subsonic_result(
    result: super::subsonic_import::SubsonicSongResult,
) -> XrpcSubsonicSongResult<'static> {
    XrpcSubsonicSongResult {
        id: xrpc_cow(result.id),
        title: xrpc_cow(result.title),
        artist: xrpc_cow(result.artist),
        album: optional_xrpc_cow(result.album),
        duration_seconds: result
            .duration_seconds
            .and_then(|value| i64::try_from(value).ok()),
        cover_art_id: optional_xrpc_cow(result.cover_art_id),
        extra_data: None,
    }
}

// ── Service auth ──

pub(crate) fn service_auth_has_lxm(auth: &VerifiedServiceAuth<'_>, nsid: &str) -> bool {
    auth.lxm().map(|lxm| lxm.as_str() == nsid).unwrap_or(false)
}

pub(crate) fn require_xrpc_lxm<E, F>(
    auth: &VerifiedServiceAuth<'_>,
    nsid: &'static str,
    auth_error: F,
) -> Result<(), XrpcErrorResponse<E>>
where
    E: std::error::Error + jacquard::IntoStatic + Serialize,
    F: FnOnce(Option<CowStr<'static>>) -> E,
{
    if service_auth_has_lxm(auth, nsid) {
        return Ok(());
    }

    Err(xrpc_typed_error(
        StatusCode::UNAUTHORIZED,
        auth_error(xrpc_message("invalid service auth method binding")),
    ))
}

pub(crate) async fn require_xrpc_admin<E, FAuth, FAdmin, FInvalid>(
    state: &AppState,
    auth: &VerifiedServiceAuth<'_>,
    nsid: &'static str,
    auth_error: FAuth,
    admin_error: FAdmin,
    invalid_error: FInvalid,
) -> Result<String, XrpcErrorResponse<E>>
where
    E: std::error::Error + jacquard::IntoStatic + Serialize,
    FAuth: FnOnce(Option<CowStr<'static>>) -> E,
    FAdmin: FnOnce(Option<CowStr<'static>>) -> E,
    FInvalid: FnOnce(Option<CowStr<'static>>) -> E,
{
    require_xrpc_lxm(auth, nsid, auth_error)?;
    xrpc_admin_did(state, auth, nsid)
        .await
        .map_err(|denied| match denied {
            AdminDenied::NotAdmin => xrpc_typed_error(
                StatusCode::FORBIDDEN,
                admin_error(xrpc_message("admin privileges required")),
            ),
            AdminDenied::Internal => xrpc_typed_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                invalid_error(xrpc_message("internal server error")),
            ),
        })
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
    let result = state.auth.is_admin_did(did).await;
    tracing::info!(did = %did, nsid = %nsid, is_admin = ?result, "Checking admin permissions");
    match result {
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
    require_xrpc_lxm(
        &auth,
        XRPC_QUEUE_LIST_NSID,
        QueueListError::AuthenticationRequired,
    )?;

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
    require_xrpc_lxm(
        &auth,
        XRPC_SONGS_LIST_NSID,
        SongsListError::AuthenticationRequired,
    )?;

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
    if !service_auth_has_lxm(&auth, XRPC_QUEUE_MODIFY_NSID) {
        return Err(xrpc_typed_error(
            StatusCode::UNAUTHORIZED,
            QueueModifyError::AuthenticationRequired(xrpc_message(
                "invalid service auth method binding",
            )),
        ));
    }
    let admin_did = xrpc_admin_did(&state, &auth, XRPC_QUEUE_MODIFY_NSID)
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
    if !service_auth_has_lxm(&auth, XRPC_SONGS_ADD_NSID) {
        return Err(xrpc_typed_error(
            StatusCode::UNAUTHORIZED,
            SongsAddError::AuthenticationRequired(xrpc_message(
                "invalid service auth method binding",
            )),
        ));
    }
    let admin_did = xrpc_admin_did(&state, &auth, XRPC_SONGS_ADD_NSID)
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
    if !service_auth_has_lxm(&auth, XRPC_SONGS_UPLOAD_NSID) {
        return Err(xrpc_typed_error(
            StatusCode::UNAUTHORIZED,
            SongsUploadError::AuthenticationRequired(xrpc_message(
                "invalid service auth method binding",
            )),
        ));
    }
    let admin_did = xrpc_admin_did(&state, &auth, XRPC_SONGS_UPLOAD_NSID)
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

pub(crate) async fn xrpc_admin_permissions_query(
    State(state): State<AppState>,
    ExtractServiceAuth(auth): ExtractServiceAuth,
) -> Result<Json<AdminPermissionsOutput<'static>>, XrpcErrorResponse<AdminPermissionsError<'static>>>
{
    require_xrpc_admin(
        &state,
        &auth,
        XRPC_ADMIN_PERMISSIONS_NSID,
        AdminPermissionsError::AuthenticationRequired,
        AdminPermissionsError::AdminRequired,
        AdminPermissionsError::InvalidRequest,
    )
    .await?;

    let whitelisted_dids = state.auth.admin_dids().await.map_err(|error| {
        tracing::error!(?error, "xrpc admin.permissions failed");
        xrpc_typed_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminPermissionsError::InvalidRequest(xrpc_message("internal server error")),
        )
    })?;

    Ok(Json(AdminPermissionsOutput {
        permissions: xrpc_admin_permissions(whitelisted_dids),
        extra_data: None,
    }))
}

pub(crate) async fn xrpc_admin_modify(
    State(state): State<AppState>,
    ExtractServiceAuth(auth): ExtractServiceAuth,
    ExtractXrpc(request): ExtractXrpc<AdminModifyRequest>,
) -> Result<Json<AdminModifyOutput<'static>>, XrpcErrorResponse<AdminModifyError<'static>>> {
    require_xrpc_admin(
        &state,
        &auth,
        XRPC_ADMIN_MODIFY_NSID,
        AdminModifyError::AuthenticationRequired,
        AdminModifyError::AdminRequired,
        AdminModifyError::InvalidRequest,
    )
    .await?;

    let did = request.did.as_ref().trim();
    if did.is_empty() {
        return Err(xrpc_typed_error(
            StatusCode::BAD_REQUEST,
            AdminModifyError::InvalidRequest(xrpc_message("did is required")),
        ));
    }

    match request.action {
        AdminModifyAction::Add => state.auth.add_admin_did(did).await,
        AdminModifyAction::Remove => state.auth.remove_admin_did(did).await,
        AdminModifyAction::Other(action) => {
            return Err(xrpc_typed_error(
                StatusCode::BAD_REQUEST,
                AdminModifyError::InvalidRequest(Some(CowStr::from(format!(
                    "unknown admin action: {action}"
                )))),
            ));
        }
    }
    .map_err(|error| {
        tracing::error!(?error, "xrpc admin.modify failed");
        xrpc_typed_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminModifyError::InvalidRequest(xrpc_message("internal server error")),
        )
    })?;

    let whitelisted_dids = state.auth.admin_dids().await.map_err(|error| {
        tracing::error!(?error, "xrpc admin.modify list failed");
        xrpc_typed_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminModifyError::InvalidRequest(xrpc_message("internal server error")),
        )
    })?;

    Ok(Json(AdminModifyOutput {
        permissions: xrpc_admin_permissions(whitelisted_dids),
        extra_data: None,
    }))
}

pub(crate) async fn xrpc_control(
    State(state): State<AppState>,
    ExtractServiceAuth(auth): ExtractServiceAuth,
    ExtractXrpc(request): ExtractXrpc<ControlRequest>,
) -> Result<Json<ControlOutput<'static>>, XrpcErrorResponse<ControlError<'static>>> {
    let admin_did = require_xrpc_admin(
        &state,
        &auth,
        XRPC_CONTROL_NSID,
        ControlError::AuthenticationRequired,
        ControlError::AdminRequired,
        ControlError::InvalidRequest,
    )
    .await?;

    if request.intent != ControlIntent::ExplicitAdminAction {
        return Err(xrpc_typed_error(
            StatusCode::BAD_REQUEST,
            ControlError::InvalidRequest(xrpc_message("invalid control intent")),
        ));
    }

    let action = match request.action {
        XrpcControlAction::Play => RadioControlAction::Play,
        XrpcControlAction::Pause => RadioControlAction::Pause,
        XrpcControlAction::Stop => RadioControlAction::Stop,
        XrpcControlAction::Skip => RadioControlAction::Skip,
        XrpcControlAction::Previous => RadioControlAction::Previous,
        XrpcControlAction::Other(action) => {
            return Err(xrpc_typed_error(
                StatusCode::BAD_REQUEST,
                ControlError::InvalidRequest(Some(CowStr::from(format!(
                    "unknown radio action: {action}"
                )))),
            ));
        }
    };

    let snapshot = state
        .radio
        .control(action, &admin_did)
        .await
        .map_err(|error| {
            tracing::error!(?error, "xrpc radio.control failed");
            xrpc_typed_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                ControlError::InvalidRequest(xrpc_message("internal server error")),
            )
        })?;

    Ok(Json(ControlOutput {
        snapshot: xrpc_radio_snapshot(snapshot),
        extra_data: None,
    }))
}

pub(crate) async fn xrpc_albums_list(
    State(state): State<AppState>,
    ExtractServiceAuth(auth): ExtractServiceAuth,
) -> Result<Json<AlbumsListOutput<'static>>, XrpcErrorResponse<AlbumsListError<'static>>> {
    require_xrpc_admin(
        &state,
        &auth,
        XRPC_ALBUMS_LIST_NSID,
        AlbumsListError::AuthenticationRequired,
        AlbumsListError::AdminRequired,
        AlbumsListError::InvalidRequest,
    )
    .await?;

    let albums = state.radio.albums().await.map_err(|error| {
        tracing::error!(?error, "xrpc albums.list failed");
        xrpc_typed_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            AlbumsListError::InvalidRequest(xrpc_message("internal server error")),
        )
    })?;

    Ok(Json(AlbumsListOutput {
        albums: albums.into_iter().map(xrpc_radio_album).collect(),
        extra_data: None,
    }))
}

pub(crate) async fn xrpc_albums_modify(
    State(state): State<AppState>,
    ExtractServiceAuth(auth): ExtractServiceAuth,
    ExtractXrpc(request): ExtractXrpc<AlbumsModifyRequest>,
) -> Result<Json<AlbumsModifyOutput<'static>>, XrpcErrorResponse<AlbumsModifyError<'static>>> {
    require_xrpc_admin(
        &state,
        &auth,
        XRPC_ALBUMS_MODIFY_NSID,
        AlbumsModifyError::AuthenticationRequired,
        AlbumsModifyError::AdminRequired,
        AlbumsModifyError::InvalidRequest,
    )
    .await?;

    let albums = match request.action {
        AlbumsModifyAction::Delete => state.radio.delete_album(request.album_id.as_ref()).await,
        AlbumsModifyAction::Merge => {
            let target_album_id = request.target_album_id.ok_or_else(|| {
                xrpc_typed_error(
                    StatusCode::BAD_REQUEST,
                    AlbumsModifyError::InvalidRequest(xrpc_message(
                        "targetAlbumId is required for merge",
                    )),
                )
            })?;
            state
                .radio
                .merge_albums(request.album_id.as_ref(), target_album_id.as_ref())
                .await
        }
        AlbumsModifyAction::Other(action) => {
            return Err(xrpc_typed_error(
                StatusCode::BAD_REQUEST,
                AlbumsModifyError::InvalidRequest(Some(CowStr::from(format!(
                    "unknown album action: {action}"
                )))),
            ));
        }
    }
    .map_err(|error| {
        tracing::error!(?error, "xrpc albums.modify failed");
        xrpc_typed_error(
            StatusCode::BAD_REQUEST,
            AlbumsModifyError::InvalidRequest(Some(CowStr::from(error.to_string()))),
        )
    })?;

    Ok(Json(AlbumsModifyOutput {
        albums: albums.into_iter().map(xrpc_radio_album).collect(),
        extra_data: None,
    }))
}

pub(crate) async fn xrpc_playlists_list(
    State(state): State<AppState>,
    ExtractServiceAuth(auth): ExtractServiceAuth,
) -> Result<Json<PlaylistsListOutput<'static>>, XrpcErrorResponse<PlaylistsListError<'static>>> {
    require_xrpc_admin(
        &state,
        &auth,
        XRPC_PLAYLISTS_LIST_NSID,
        PlaylistsListError::AuthenticationRequired,
        PlaylistsListError::AdminRequired,
        PlaylistsListError::InvalidRequest,
    )
    .await?;

    let playlists = state.radio.playlists().await.map_err(|error| {
        tracing::error!(?error, "xrpc playlists.list failed");
        xrpc_typed_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            PlaylistsListError::InvalidRequest(xrpc_message("internal server error")),
        )
    })?;

    Ok(Json(PlaylistsListOutput {
        playlists: playlists.into_iter().map(xrpc_playlist).collect(),
        extra_data: None,
    }))
}

pub(crate) async fn xrpc_playlists_modify(
    State(state): State<AppState>,
    ExtractServiceAuth(auth): ExtractServiceAuth,
    ExtractXrpc(request): ExtractXrpc<PlaylistsModifyRequest>,
) -> Result<Json<PlaylistsModifyOutput<'static>>, XrpcErrorResponse<PlaylistsModifyError<'static>>>
{
    let admin_did = require_xrpc_admin(
        &state,
        &auth,
        XRPC_PLAYLISTS_MODIFY_NSID,
        PlaylistsModifyError::AuthenticationRequired,
        PlaylistsModifyError::AdminRequired,
        PlaylistsModifyError::InvalidRequest,
    )
    .await?;

    match request.action {
        PlaylistsModifyAction::Create => {
            let name = request.name.ok_or_else(|| {
                xrpc_typed_error(
                    StatusCode::BAD_REQUEST,
                    PlaylistsModifyError::InvalidRequest(xrpc_message(
                        "name is required for create",
                    )),
                )
            })?;
            let song_ids = request.song_ids.ok_or_else(|| {
                xrpc_typed_error(
                    StatusCode::BAD_REQUEST,
                    PlaylistsModifyError::InvalidRequest(xrpc_message(
                        "songIds is required for create",
                    )),
                )
            })?;
            let song_ids: Vec<String> = song_ids
                .iter()
                .map(|song_id| song_id.as_ref().to_owned())
                .collect();
            let playlist = state
                .radio
                .create_playlist(name.as_ref(), &song_ids)
                .await
                .map_err(|error| {
                    xrpc_typed_error(
                        StatusCode::BAD_REQUEST,
                        PlaylistsModifyError::InvalidRequest(Some(CowStr::from(error.to_string()))),
                    )
                })?;
            Ok(Json(PlaylistsModifyOutput {
                playlist: Some(xrpc_playlist(playlist)),
                snapshot: None,
                extra_data: None,
            }))
        }
        PlaylistsModifyAction::Delete => {
            let playlist_id = request.playlist_id.ok_or_else(|| {
                xrpc_typed_error(
                    StatusCode::BAD_REQUEST,
                    PlaylistsModifyError::InvalidRequest(xrpc_message(
                        "playlistId is required for delete",
                    )),
                )
            })?;
            state
                .radio
                .delete_playlist(playlist_id.as_ref())
                .await
                .map_err(|error| {
                    xrpc_typed_error(
                        StatusCode::BAD_REQUEST,
                        PlaylistsModifyError::PlaylistNotFound(Some(CowStr::from(
                            error.to_string(),
                        ))),
                    )
                })?;
            Ok(Json(PlaylistsModifyOutput {
                playlist: None,
                snapshot: None,
                extra_data: None,
            }))
        }
        PlaylistsModifyAction::Load => {
            let playlist_id = request.playlist_id.ok_or_else(|| {
                xrpc_typed_error(
                    StatusCode::BAD_REQUEST,
                    PlaylistsModifyError::InvalidRequest(xrpc_message(
                        "playlistId is required for load",
                    )),
                )
            })?;
            let snapshot = state
                .radio
                .load_playlist(
                    playlist_id.as_ref(),
                    request.replace.unwrap_or(false),
                    &admin_did,
                )
                .await
                .map_err(|error| {
                    xrpc_typed_error(
                        StatusCode::BAD_REQUEST,
                        PlaylistsModifyError::PlaylistNotFound(Some(CowStr::from(
                            error.to_string(),
                        ))),
                    )
                })?;
            Ok(Json(PlaylistsModifyOutput {
                playlist: None,
                snapshot: Some(xrpc_radio_snapshot(snapshot)),
                extra_data: None,
            }))
        }
        PlaylistsModifyAction::Other(action) => Err(xrpc_typed_error(
            StatusCode::BAD_REQUEST,
            PlaylistsModifyError::InvalidRequest(Some(CowStr::from(format!(
                "unknown playlist action: {action}"
            )))),
        )),
    }
}

pub(crate) async fn xrpc_songs_modify(
    State(state): State<AppState>,
    ExtractServiceAuth(auth): ExtractServiceAuth,
    ExtractXrpc(request): ExtractXrpc<SongsModifyRequest>,
) -> Result<Json<SongsModifyOutput<'static>>, XrpcErrorResponse<SongsModifyError<'static>>> {
    require_xrpc_admin(
        &state,
        &auth,
        XRPC_SONGS_MODIFY_NSID,
        SongsModifyError::AuthenticationRequired,
        SongsModifyError::AdminRequired,
        SongsModifyError::InvalidRequest,
    )
    .await?;

    match request.action {
        SongsModifyAction::Update => {
            let title = request.title.ok_or_else(|| {
                xrpc_typed_error(
                    StatusCode::BAD_REQUEST,
                    SongsModifyError::InvalidRequest(xrpc_message("title is required for update")),
                )
            })?;
            let artist = request.artist.ok_or_else(|| {
                xrpc_typed_error(
                    StatusCode::BAD_REQUEST,
                    SongsModifyError::InvalidRequest(xrpc_message("artist is required for update")),
                )
            })?;
            let song = state
                .radio
                .update_song_metadata(
                    request.song_id.as_ref(),
                    SongMetadataUpdate {
                        title: title.as_ref().to_owned(),
                        artist: artist.as_ref().to_owned(),
                        album: request.album.map(|value| value.as_ref().to_owned()),
                        genre: request.genre.map(|value| value.as_ref().to_owned()),
                        duration_seconds: request.duration_seconds,
                    },
                )
                .await
                .map_err(|error| {
                    let message = error.to_string();
                    let typed = if message.contains("song not found") {
                        SongsModifyError::SongNotFound(Some(CowStr::from(message)))
                    } else {
                        SongsModifyError::InvalidRequest(Some(CowStr::from(message)))
                    };
                    xrpc_typed_error(StatusCode::BAD_REQUEST, typed)
                })?;

            Ok(Json(SongsModifyOutput {
                song: Some(xrpc_song(song)),
                snapshot: None,
                extra_data: None,
            }))
        }
        SongsModifyAction::Delete => {
            let snapshot = state
                .radio
                .delete_song(request.song_id.as_ref())
                .await
                .map_err(|error| {
                    xrpc_typed_error(
                        StatusCode::BAD_REQUEST,
                        SongsModifyError::SongNotFound(Some(CowStr::from(error.to_string()))),
                    )
                })?;
            Ok(Json(SongsModifyOutput {
                song: None,
                snapshot: Some(xrpc_radio_snapshot(snapshot)),
                extra_data: None,
            }))
        }
        SongsModifyAction::Other(action) => Err(xrpc_typed_error(
            StatusCode::BAD_REQUEST,
            SongsModifyError::InvalidRequest(Some(CowStr::from(format!(
                "unknown song action: {action}"
            )))),
        )),
    }
}

pub(crate) async fn xrpc_songs_cover(
    State(state): State<AppState>,
    ExtractServiceAuth(auth): ExtractServiceAuth,
    mut multipart: Multipart,
) -> Result<Json<SongsCoverOutput<'static>>, XrpcErrorResponse<SongsCoverError<'static>>> {
    require_xrpc_admin(
        &state,
        &auth,
        XRPC_SONGS_COVER_NSID,
        SongsCoverError::AuthenticationRequired,
        SongsCoverError::AdminRequired,
        SongsCoverError::InvalidRequest,
    )
    .await?;

    let mut song_id = None;
    let mut filename = None;
    let mut mime_type = None;
    let mut bytes = None;

    while let Some(field) = multipart.next_field().await.map_err(|_| {
        xrpc_typed_error(
            StatusCode::BAD_REQUEST,
            SongsCoverError::InvalidRequest(xrpc_message("invalid multipart body")),
        )
    })? {
        match field.name() {
            Some("songId") => {
                song_id = Some(field.text().await.map_err(|_| {
                    xrpc_typed_error(
                        StatusCode::BAD_REQUEST,
                        SongsCoverError::InvalidRequest(xrpc_message("invalid songId field")),
                    )
                })?);
            }
            Some("cover") => {
                filename = field.file_name().map(ToOwned::to_owned);
                mime_type = field.content_type().map(ToString::to_string);
                bytes = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|_| {
                            xrpc_typed_error(
                                StatusCode::BAD_REQUEST,
                                SongsCoverError::InvalidRequest(xrpc_message("invalid cover file")),
                            )
                        })?
                        .to_vec(),
                );
            }
            _ => {}
        }
    }

    let song_id = song_id
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            xrpc_typed_error(
                StatusCode::BAD_REQUEST,
                SongsCoverError::InvalidRequest(xrpc_message("songId is required")),
            )
        })?;
    let bytes = bytes.ok_or_else(|| {
        xrpc_typed_error(
            StatusCode::BAD_REQUEST,
            SongsCoverError::InvalidRequest(xrpc_message("cover file is required")),
        )
    })?;

    let song = state
        .radio
        .set_song_cover(&song_id, filename, mime_type, bytes)
        .await
        .map_err(|error| {
            xrpc_typed_error(
                StatusCode::BAD_REQUEST,
                SongsCoverError::SongNotFound(Some(CowStr::from(error.to_string()))),
            )
        })?;

    Ok(Json(SongsCoverOutput {
        song: xrpc_song(song),
        extra_data: None,
    }))
}

pub(crate) async fn xrpc_chat_send(
    State(state): State<AppState>,
    ExtractServiceAuth(auth): ExtractServiceAuth,
    ExtractXrpc(request): ExtractXrpc<ChatSendRequest>,
) -> Result<Json<ChatSendOutput<'static>>, XrpcErrorResponse<ChatSendError<'static>>> {
    require_xrpc_lxm(
        &auth,
        XRPC_CHAT_SEND_NSID,
        ChatSendError::AuthenticationRequired,
    )?;

    let body = request.text.as_ref().trim();
    if body.is_empty() || body.chars().count() > MAX_CHAT_BODY_LEN {
        return Err(xrpc_typed_error(
            StatusCode::BAD_REQUEST,
            ChatSendError::InvalidRequest(xrpc_message("invalid chat body")),
        ));
    }

    let did = auth.did().as_str();
    if state.chat.is_banned(did).await.map_err(|error| {
        tracing::error!(?error, "xrpc chat.send ban check failed");
        xrpc_typed_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            ChatSendError::InvalidRequest(xrpc_message("internal server error")),
        )
    })? {
        return Err(xrpc_typed_error(
            StatusCode::FORBIDDEN,
            ChatSendError::Banned(xrpc_message("chat sender is banned")),
        ));
    }

    let message = state.chat.post(did, body).await.map_err(|error| {
        tracing::error!(?error, "xrpc chat.send failed");
        xrpc_typed_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            ChatSendError::InvalidRequest(xrpc_message("internal server error")),
        )
    })?;

    Ok(Json(ChatSendOutput {
        message: xrpc_chat_message(message),
        extra_data: None,
    }))
}

pub(crate) async fn xrpc_chat_bans_list(
    State(state): State<AppState>,
    ExtractServiceAuth(auth): ExtractServiceAuth,
) -> Result<Json<ChatBansListOutput<'static>>, XrpcErrorResponse<ChatBansListError<'static>>> {
    require_xrpc_admin(
        &state,
        &auth,
        XRPC_CHAT_BANS_LIST_NSID,
        ChatBansListError::AuthenticationRequired,
        ChatBansListError::AdminRequired,
        ChatBansListError::InvalidRequest,
    )
    .await?;

    let bans = state.chat.list_bans().await.map_err(|error| {
        tracing::error!(?error, "xrpc chat.bans.list failed");
        xrpc_typed_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            ChatBansListError::InvalidRequest(xrpc_message("internal server error")),
        )
    })?;

    Ok(Json(ChatBansListOutput {
        bans: bans.into_iter().map(xrpc_chat_ban).collect(),
        extra_data: None,
    }))
}

pub(crate) async fn xrpc_chat_bans_modify(
    State(state): State<AppState>,
    ExtractServiceAuth(auth): ExtractServiceAuth,
    ExtractXrpc(request): ExtractXrpc<ChatBansModifyRequest>,
) -> Result<Json<ChatBansModifyOutput<'static>>, XrpcErrorResponse<ChatBansModifyError<'static>>> {
    let admin_did = require_xrpc_admin(
        &state,
        &auth,
        XRPC_CHAT_BANS_MODIFY_NSID,
        ChatBansModifyError::AuthenticationRequired,
        ChatBansModifyError::AdminRequired,
        ChatBansModifyError::InvalidRequest,
    )
    .await?;

    let did = request.did.as_ref().trim();
    if !valid_listener_did(did) {
        return Err(xrpc_typed_error(
            StatusCode::BAD_REQUEST,
            ChatBansModifyError::InvalidRequest(xrpc_message("invalid did")),
        ));
    }

    match request.action {
        ChatBansModifyAction::Create => {
            let reason = request
                .reason
                .as_ref()
                .map(|value| value.as_ref().trim())
                .filter(|value| !value.is_empty());
            let ban = state
                .chat
                .ban_did(did, &admin_did, reason)
                .await
                .map_err(|error| {
                    tracing::error!(?error, "xrpc chat.bans.modify create failed");
                    xrpc_typed_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        ChatBansModifyError::InvalidRequest(xrpc_message("internal server error")),
                    )
                })?;
            Ok(Json(ChatBansModifyOutput {
                ban: Some(xrpc_chat_ban(ban)),
                extra_data: None,
            }))
        }
        ChatBansModifyAction::Remove => {
            let removed = state.chat.unban_did(did).await.map_err(|error| {
                tracing::error!(?error, "xrpc chat.bans.modify remove failed");
                xrpc_typed_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    ChatBansModifyError::InvalidRequest(xrpc_message("internal server error")),
                )
            })?;
            if !removed {
                return Err(xrpc_typed_error(
                    StatusCode::NOT_FOUND,
                    ChatBansModifyError::BanNotFound(xrpc_message("ban not found")),
                ));
            }
            Ok(Json(ChatBansModifyOutput {
                ban: None,
                extra_data: None,
            }))
        }
        ChatBansModifyAction::Other(action) => Err(xrpc_typed_error(
            StatusCode::BAD_REQUEST,
            ChatBansModifyError::InvalidRequest(Some(CowStr::from(format!(
                "unknown ban action: {action}"
            )))),
        )),
    }
}

pub(crate) async fn xrpc_chat_messages_modify(
    State(state): State<AppState>,
    ExtractServiceAuth(auth): ExtractServiceAuth,
    ExtractXrpc(request): ExtractXrpc<ChatMessagesModifyRequest>,
) -> Result<
    Json<ChatMessagesModifyOutput<'static>>,
    XrpcErrorResponse<ChatMessagesModifyError<'static>>,
> {
    require_xrpc_admin(
        &state,
        &auth,
        XRPC_CHAT_MESSAGES_MODIFY_NSID,
        ChatMessagesModifyError::AuthenticationRequired,
        ChatMessagesModifyError::AdminRequired,
        ChatMessagesModifyError::InvalidRequest,
    )
    .await?;

    match request.action {
        ChatMessagesModifyAction::Delete => {}
        ChatMessagesModifyAction::Other(action) => {
            return Err(xrpc_typed_error(
                StatusCode::BAD_REQUEST,
                ChatMessagesModifyError::InvalidRequest(Some(CowStr::from(format!(
                    "unknown message action: {action}"
                )))),
            ));
        }
    }

    let removed = state
        .chat
        .delete_message(request.message_id.as_ref())
        .await
        .map_err(|error| {
            tracing::error!(?error, "xrpc chat.messages.modify failed");
            xrpc_typed_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                ChatMessagesModifyError::InvalidRequest(xrpc_message("internal server error")),
            )
        })?;

    if !removed {
        return Err(xrpc_typed_error(
            StatusCode::NOT_FOUND,
            ChatMessagesModifyError::MessageNotFound(xrpc_message("message not found")),
        ));
    }

    Ok(Json(ChatMessagesModifyOutput { extra_data: None }))
}

pub(crate) async fn xrpc_subsonic_search(
    State(state): State<AppState>,
    ExtractServiceAuth(auth): ExtractServiceAuth,
    ExtractXrpc(request): ExtractXrpc<SubsonicSearchRequest>,
) -> Result<Json<SubsonicSearchOutput<'static>>, XrpcErrorResponse<SubsonicSearchError<'static>>> {
    require_xrpc_admin(
        &state,
        &auth,
        XRPC_SUBSONIC_SEARCH_NSID,
        SubsonicSearchError::AuthenticationRequired,
        SubsonicSearchError::AdminRequired,
        SubsonicSearchError::InvalidRequest,
    )
    .await?;

    let creds = super::subsonic_import::SubsonicCreds {
        server_url: request.server_url.as_str().to_owned(),
        username: request.username.as_ref().to_owned(),
        password: request.password.as_ref().to_owned(),
    };
    let Json(results) =
        super::subsonic_import::search_subsonic_catalog(&creds, request.query.as_ref())
            .await
            .map_err(|(status, Json(error))| {
                let typed = match error.error.as_str() {
                    "subsonic_unreachable" | "subsonic_parse_error" => {
                        SubsonicSearchError::SubsonicFailed(Some(CowStr::from(error.error)))
                    }
                    _ => SubsonicSearchError::InvalidRequest(Some(CowStr::from(error.error))),
                };
                xrpc_typed_error(status, typed)
            })?;

    Ok(Json(SubsonicSearchOutput {
        results: results.into_iter().map(xrpc_subsonic_result).collect(),
        extra_data: None,
    }))
}

pub(crate) async fn xrpc_subsonic_import(
    State(state): State<AppState>,
    ExtractServiceAuth(auth): ExtractServiceAuth,
    ExtractXrpc(request): ExtractXrpc<SubsonicImportRequest>,
) -> Result<Json<SubsonicImportOutput<'static>>, XrpcErrorResponse<SubsonicImportError<'static>>> {
    let admin_did = require_xrpc_admin(
        &state,
        &auth,
        XRPC_SUBSONIC_IMPORT_NSID,
        SubsonicImportError::AuthenticationRequired,
        SubsonicImportError::AdminRequired,
        SubsonicImportError::InvalidRequest,
    )
    .await?;

    let song = match request.source {
        ImportSource::Song => {
            let server_url = request.server_url.ok_or_else(|| {
                xrpc_typed_error(
                    StatusCode::BAD_REQUEST,
                    SubsonicImportError::InvalidRequest(xrpc_message(
                        "serverUrl is required for song import",
                    )),
                )
            })?;
            let username = request.username.ok_or_else(|| {
                xrpc_typed_error(
                    StatusCode::BAD_REQUEST,
                    SubsonicImportError::InvalidRequest(xrpc_message(
                        "username is required for song import",
                    )),
                )
            })?;
            let password = request.password.ok_or_else(|| {
                xrpc_typed_error(
                    StatusCode::BAD_REQUEST,
                    SubsonicImportError::InvalidRequest(xrpc_message(
                        "password is required for song import",
                    )),
                )
            })?;
            let song_id = request.song_id.ok_or_else(|| {
                xrpc_typed_error(
                    StatusCode::BAD_REQUEST,
                    SubsonicImportError::InvalidRequest(xrpc_message(
                        "songId is required for song import",
                    )),
                )
            })?;

            let payload = super::subsonic_import::SubsonicImportRequest {
                creds: super::subsonic_import::SubsonicCreds {
                    server_url: server_url.as_str().to_owned(),
                    username: username.as_ref().to_owned(),
                    password: password.as_ref().to_owned(),
                },
                song_id: song_id.as_ref().to_owned(),
                cover_art_id: request.cover_art_id.map(|value| value.as_ref().to_owned()),
                add_to_queue: Some(request.add_to_queue),
            };
            super::subsonic_import::import_subsonic_song(&state, &admin_did, payload).await
        }
        ImportSource::Share => {
            let share_url = request.share_url.ok_or_else(|| {
                xrpc_typed_error(
                    StatusCode::BAD_REQUEST,
                    SubsonicImportError::InvalidRequest(xrpc_message(
                        "shareUrl is required for share import",
                    )),
                )
            })?;
            let payload = super::subsonic_import::SubsonicShareImportRequest {
                share_url: share_url.as_str().to_owned(),
                add_to_queue: Some(request.add_to_queue),
            };
            super::subsonic_import::import_subsonic_share(&state, &admin_did, payload).await
        }
        ImportSource::Other(action) => {
            return Err(xrpc_typed_error(
                StatusCode::BAD_REQUEST,
                SubsonicImportError::InvalidRequest(Some(CowStr::from(format!(
                    "unknown subsonic import source: {action}"
                )))),
            ));
        }
    }
    .map_err(|(status, Json(error))| {
        let typed = match error.error.as_str() {
            "subsonic_unreachable"
            | "subsonic_parse_error"
            | "subsonic_stream_failed"
            | "share_info_missing"
            | "share_info_invalid"
            | "share_has_no_tracks"
            | "share_track_id_missing" => {
                SubsonicImportError::SubsonicFailed(Some(CowStr::from(error.error)))
            }
            _ => SubsonicImportError::InvalidRequest(Some(CowStr::from(error.error))),
        };
        xrpc_typed_error(status, typed)
    })?;

    Ok(Json(SubsonicImportOutput {
        song: xrpc_song(song),
        extra_data: None,
    }))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{
        ADMIN_XRPC_METHODS, LISTENER_XRPC_METHODS, XRPC_ADMIN_MODIFY_NSID,
        XRPC_ADMIN_PERMISSIONS_NSID, XRPC_ALBUMS_LIST_NSID, XRPC_ALBUMS_MODIFY_NSID,
        XRPC_CHAT_BANS_LIST_NSID, XRPC_CHAT_BANS_MODIFY_NSID, XRPC_CHAT_MESSAGES_MODIFY_NSID,
        XRPC_CHAT_SEND_NSID, XRPC_CONTROL_NSID, XRPC_PLAYLISTS_LIST_NSID,
        XRPC_PLAYLISTS_MODIFY_NSID, XRPC_QUEUE_LIST_NSID, XRPC_QUEUE_MODIFY_NSID,
        XRPC_SONGS_ADD_NSID, XRPC_SONGS_COVER_NSID, XRPC_SONGS_LIST_NSID, XRPC_SONGS_MODIFY_NSID,
        XRPC_SONGS_UPLOAD_NSID, XRPC_SUBSONIC_IMPORT_NSID, XRPC_SUBSONIC_SEARCH_NSID,
        is_admin_xrpc_method, xrpc_radio_snapshot,
    };
    use crate::radio::{QueueItem, RadioSnapshot, RadioState, Song};

    #[test]
    fn admin_xrpc_methods_are_explicitly_whitelisted() {
        let expected = BTreeSet::from([
            XRPC_QUEUE_MODIFY_NSID,
            XRPC_SONGS_ADD_NSID,
            XRPC_SONGS_UPLOAD_NSID,
            XRPC_SONGS_COVER_NSID,
            XRPC_SONGS_MODIFY_NSID,
            XRPC_ADMIN_PERMISSIONS_NSID,
            XRPC_ADMIN_MODIFY_NSID,
            XRPC_CONTROL_NSID,
            XRPC_ALBUMS_LIST_NSID,
            XRPC_ALBUMS_MODIFY_NSID,
            XRPC_PLAYLISTS_LIST_NSID,
            XRPC_PLAYLISTS_MODIFY_NSID,
            XRPC_CHAT_BANS_LIST_NSID,
            XRPC_CHAT_BANS_MODIFY_NSID,
            XRPC_CHAT_MESSAGES_MODIFY_NSID,
            XRPC_SUBSONIC_SEARCH_NSID,
            XRPC_SUBSONIC_IMPORT_NSID,
        ]);
        let actual = ADMIN_XRPC_METHODS.iter().copied().collect::<BTreeSet<_>>();

        assert_eq!(
            ADMIN_XRPC_METHODS.len(),
            actual.len(),
            "duplicate admin XRPC method"
        );
        assert_eq!(actual, expected);
        assert!(is_admin_xrpc_method(XRPC_ADMIN_MODIFY_NSID));
        assert!(!is_admin_xrpc_method(XRPC_CHAT_SEND_NSID));
        assert_eq!(
            LISTENER_XRPC_METHODS,
            &[
                XRPC_QUEUE_LIST_NSID,
                XRPC_SONGS_LIST_NSID,
                XRPC_CHAT_SEND_NSID
            ]
        );
    }

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
