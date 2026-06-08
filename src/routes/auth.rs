use axum::{Json, extract::{Query, State}, http::StatusCode, response::{IntoResponse, Redirect}};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use jacquard::oauth::types::CallbackParams;
use serde::{Deserialize, Serialize};

use super::{AppState, ErrorResponse, SessionToken, internal_api_error};

#[derive(Deserialize)]
pub(crate) struct StartOauthQuery {
    pub(crate) input: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct OAuthCallbackQuery {
    pub(crate) code: Option<String>,
    pub(crate) state: Option<String>,
    pub(crate) iss: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionResponse {
    pub(crate) authenticated: bool,
    pub(crate) account_did: Option<String>,
    pub(crate) is_admin: bool,
}

pub(crate) async fn start_oauth(
    State(state): State<AppState>,
    Query(query): Query<StartOauthQuery>,
) -> Redirect {
    let Some(input) = query.input else {
        return Redirect::temporary(&state.auth.config().error_redirect_url("missing_input"));
    };

    match state.auth.start_sign_in(&input).await {
        Ok(url) => Redirect::temporary(&url),
        Err(error) => {
            tracing::error!(?error, "failed to start oauth flow");
            Redirect::temporary(&state.auth.config().error_redirect_url("oauth_start_failed"))
        }
    }
}

pub(crate) async fn oauth_callback(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<OAuthCallbackQuery>,
) -> impl IntoResponse {
    let Some(code) = query.code else {
        return Redirect::temporary(&state.auth.config().error_redirect_url("missing_code"))
            .into_response();
    };

    let params = CallbackParams {
        code: code.into(),
        state: query.state.map(Into::into),
        iss: query.iss.map(Into::into),
    };

    match state.auth.finish_sign_in(params).await {
        Ok(sign_in) => {
            let jar = jar.add(build_session_cookie(&state.auth, sign_in.session_token()));
            (
                jar,
                Redirect::to(
                    &state
                        .auth
                        .config()
                        .success_redirect_with_token(sign_in.session_token()),
                ),
            )
                .into_response()
        }
        Err(error) => {
            tracing::error!(?error, "oauth callback failed");
            Redirect::temporary(
                &state
                    .auth
                    .config()
                    .error_redirect_url("oauth_callback_failed"),
            )
            .into_response()
        }
    }
}

pub(crate) async fn get_session(
    State(state): State<AppState>,
    session_token: SessionToken,
) -> Result<Json<SessionResponse>, (StatusCode, Json<ErrorResponse>)> {
    let session = state
        .auth
        .session(session_token.0.as_deref())
        .await
        .map_err(internal_api_error)?;

    if let Some(session) = session {
        let is_admin = state
            .auth
            .is_admin_did(&session.account_did)
            .await
            .map_err(internal_api_error)?;

        return Ok(Json(SessionResponse {
            authenticated: true,
            account_did: Some(session.account_did),
            is_admin,
        }));
    }

    Ok(Json(SessionResponse {
        authenticated: false,
        account_did: None,
        is_admin: false,
    }))
}

pub(crate) async fn logout(
    State(state): State<AppState>,
    session_token: SessionToken,
    jar: CookieJar,
) -> Result<(CookieJar, StatusCode), (StatusCode, Json<ErrorResponse>)> {
    state
        .auth
        .sign_out(session_token.0.as_deref())
        .await
        .map_err(internal_api_error)?;

    Ok((
        jar.add(clear_session_cookie(&state.auth)),
        StatusCode::NO_CONTENT,
    ))
}

pub(crate) fn build_session_cookie(
    auth: &crate::auth::AuthService,
    session_token: &str,
) -> Cookie<'static> {
    let mut cookie = Cookie::new(
        auth.config().session_cookie_name.clone(),
        session_token.to_owned(),
    );
    cookie.set_path("/");
    cookie.set_http_only(true);
    cookie.set_same_site(SameSite::None);
    cookie.set_secure(true);
    cookie.set_max_age(time::Duration::days(auth.config().session_ttl_days));
    cookie
}

pub(crate) fn clear_session_cookie(auth: &crate::auth::AuthService) -> Cookie<'static> {
    let mut cookie = Cookie::new(auth.config().session_cookie_name.clone(), String::new());
    cookie.set_path("/");
    cookie.make_removal();
    cookie
}
