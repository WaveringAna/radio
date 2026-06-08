use axum::{Json, extract::{Path, State}, http::StatusCode};
use serde::{Deserialize, Serialize};

use super::{AppState, ErrorResponse, SessionToken, api_error, internal_api_error, admin_session};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AdminPermissionsResponse {
    pub(crate) whitelisted_dids: Vec<String>,
    pub(crate) permissions: Vec<AdminPermission>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AdminPermission {
    pub(crate) key: &'static str,
    pub(crate) description: &'static str,
}

#[derive(Deserialize)]
pub(crate) struct AdminDidRequest {
    pub(crate) did: String,
}

pub(crate) async fn get_admin_permissions(
    State(state): State<AppState>,
    session_token: SessionToken,
) -> Result<Json<AdminPermissionsResponse>, (StatusCode, Json<ErrorResponse>)> {
    let session = state
        .auth
        .session(session_token.0.as_deref())
        .await
        .map_err(internal_api_error)?
        .ok_or_else(|| api_error(StatusCode::UNAUTHORIZED, "unauthenticated"))?;

    if !state
        .auth
        .is_admin_did(&session.account_did)
        .await
        .map_err(internal_api_error)?
    {
        return Err(api_error(StatusCode::FORBIDDEN, "admin_required"));
    }

    Ok(Json(AdminPermissionsResponse {
        whitelisted_dids: state.auth.admin_dids().await.map_err(internal_api_error)?,
        permissions: admin_permissions(),
    }))
}

pub(crate) async fn add_admin_did(
    State(state): State<AppState>,
    session_token: SessionToken,
    Json(payload): Json<AdminDidRequest>,
) -> Result<Json<AdminPermissionsResponse>, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, session_token.0.as_deref()).await?;
    state
        .auth
        .add_admin_did(&payload.did)
        .await
        .map_err(internal_api_error)?;

    Ok(Json(AdminPermissionsResponse {
        whitelisted_dids: state.auth.admin_dids().await.map_err(internal_api_error)?,
        permissions: admin_permissions(),
    }))
}

pub(crate) async fn remove_admin_did(
    State(state): State<AppState>,
    session_token: SessionToken,
    Path(did): Path<String>,
) -> Result<Json<AdminPermissionsResponse>, (StatusCode, Json<ErrorResponse>)> {
    let _session = admin_session(&state, session_token.0.as_deref()).await?;
    state
        .auth
        .remove_admin_did(&did)
        .await
        .map_err(internal_api_error)?;

    Ok(Json(AdminPermissionsResponse {
        whitelisted_dids: state.auth.admin_dids().await.map_err(internal_api_error)?,
        permissions: admin_permissions(),
    }))
}

pub(crate) fn admin_permissions() -> Vec<AdminPermission> {
    vec![
        AdminPermission {
            key: "songs:add",
            description: "add songs to the radio catalog",
        },
        AdminPermission {
            key: "radio:control",
            description: "control radio playback and queue state",
        },
    ]
}
