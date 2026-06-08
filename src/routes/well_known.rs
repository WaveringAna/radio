use axum::{Json, extract::State, http::header, http::StatusCode, response::{IntoResponse, Response}};

use super::AppState;

#[derive(serde::Serialize)]
pub(crate) struct HealthResponse {
    pub(crate) ok: bool,
}

pub(crate) async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { ok: true })
}

pub(crate) async fn client_metadata(State(state): State<AppState>) -> Response {
    match state.auth.client_metadata_json() {
        Ok(json) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            json,
        )
            .into_response(),
        Err(error) => {
            tracing::error!(%error, "failed to serialize client metadata");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub(crate) async fn atproto_did(State(state): State<AppState>) -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        state.service_did.as_str().to_owned(),
    )
        .into_response()
}

pub(crate) async fn did_json(State(state): State<AppState>) -> Response {
    let services: Vec<_> = state
        .service_ids
        .iter()
        .map(|id| {
            serde_json::json!({
                "id": id,
                "type": "AtprotoService",
                "serviceEndpoint": state.service_endpoint.as_str(),
            })
        })
        .collect();

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/did+json"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Json(serde_json::json!({
            "@context": ["https://www.w3.org/ns/did/v1"],
            "id": state.service_did.as_str(),
            "service": services,
        })),
    )
        .into_response()
}

pub(crate) async fn oauth_protected_resource(
    State(state): State<AppState>,
) -> Json<serde_json::Value> {
    let authorization_server = std::env::var("OAUTH_AUTHORIZATION_SERVER")
        .unwrap_or_else(|_| "https://bsky.social".into());

    Json(serde_json::json!({
        "resource": state.service_endpoint.as_str(),
        "authorization_servers": [authorization_server],
        "bearer_methods_supported": ["header"],
    }))
}
