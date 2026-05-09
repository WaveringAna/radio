use std::{collections::HashSet, sync::Arc};

use anyhow::{Context, anyhow};
use jacquard::{
    IntoStatic,
    common::session::SessionStoreError,
    deps::fluent_uri::Uri,
    identity::JacquardResolver,
    oauth::{
        atproto::{AtprotoClientMetadata, GrantType, atproto_client_metadata},
        authstore::ClientAuthStore,
        client::OAuthClient,
        scopes::Scope,
        session::{AuthRequestData, ClientData, ClientSessionData},
        types::{AuthorizeOptions, CallbackParams},
    },
    types::did::Did,
};
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::db::Database;

/// Auth-specific runtime configuration.
#[derive(Clone, Debug)]
pub(crate) struct AuthConfig {
    /// Public app URL used for OAuth callback and client metadata.
    pub(crate) app_url: String,
    /// Frontend URL used for post-auth redirects.
    pub(crate) frontend_url: String,
    /// Cookie name for browser app sessions.
    pub(crate) session_cookie_name: String,
    /// Lifetime of app sessions in days.
    pub(crate) session_ttl_days: i64,
    /// Admin DIDs allowed to manage privileged radio features.
    pub(crate) admin_dids: Vec<String>,
}

impl AuthConfig {
    /// Returns the OAuth callback URL.
    pub(crate) fn callback_url(&self) -> String {
        format!("{}/api/oauth/callback", self.app_url)
    }

    /// Returns the post-auth success redirect URL with the session token in the query string.
    pub(crate) fn success_redirect_with_token(&self, token: &str) -> String {
        format!("{}/?token={token}", self.frontend_url)
    }

    /// Returns the frontend error redirect URL for the given code.
    pub(crate) fn error_redirect_url(&self, code: &str) -> String {
        format!("{}/?error={code}", self.frontend_url)
    }

    /// Returns the unix timestamp at which a fresh app session should expire.
    pub(crate) fn session_expires_at(&self) -> i64 {
        OffsetDateTime::now_utc().unix_timestamp() + (self.session_ttl_days * 24 * 60 * 60)
    }

    /// Returns whether browser cookies should be marked secure.
    pub(crate) fn secure_cookies(&self) -> bool {
        self.app_url.starts_with("https://")
    }
}

/// Parses a comma-separated admin DID whitelist, preserving first-seen order.
pub(crate) fn parse_admin_dids(value: &str) -> Vec<String> {
    let mut seen = HashSet::new();

    value
        .split(',')
        .map(str::trim)
        .filter(|did| !did.is_empty())
        .filter_map(|did| {
            let did = did.to_owned();
            seen.insert(did.clone()).then_some(did)
        })
        .collect()
}

/// Minimal authenticated app-session view.
#[derive(FromRow)]
pub(crate) struct AppSession {
    /// DID of the signed-in account.
    pub(crate) account_did: String,
}

/// Result of a successful sign-in flow.
pub(crate) struct CompletedSignIn {
    session_token: String,
}

impl CompletedSignIn {
    /// Returns the newly created app-session token.
    pub(crate) fn session_token(&self) -> &str {
        &self.session_token
    }
}

/// Auth service combining Jacquard OAuth with sqlite-backed session persistence.
#[derive(Clone)]
pub(crate) struct AuthService {
    config: AuthConfig,
    db: Database,
    oauth: Arc<OAuthClient<JacquardResolver, SqliteAuthStore>>,
}

impl AuthService {
    /// Creates a new auth service.
    ///
    /// # Errors
    /// Returns an error when OAuth client metadata cannot be constructed.
    pub(crate) fn new(config: AuthConfig, db: Database) -> anyhow::Result<Self> {
        let store = SqliteAuthStore::new(db.clone());
        let oauth = Arc::new(OAuthClient::new(store, oauth_client_data(&config)?));

        Ok(Self { config, db, oauth })
    }

    /// Starts the server-side OAuth flow.
    ///
    /// # Errors
    /// Returns an error when the input is empty or Jacquard fails to create the auth request.
    pub(crate) async fn start_sign_in(&self, input: &str) -> anyhow::Result<String> {
        let value: String = input
            .chars()
            .filter(|c| c.is_ascii() && !c.is_ascii_control())
            .collect();
        let value = value.trim();
        if value.is_empty() {
            return Err(anyhow!("missing oauth input"));
        }

        self.oauth
            .start_auth(value.to_owned(), AuthorizeOptions::default())
            .await
            .context("starting oauth flow")
    }

    /// Finishes the OAuth callback and creates an app session.
    ///
    /// # Errors
    /// Returns an error when callback handling or sqlite persistence fails.
    pub(crate) async fn finish_sign_in(
        &self,
        params: CallbackParams<'_>,
    ) -> anyhow::Result<CompletedSignIn> {
        let session = self
            .oauth
            .callback(params)
            .await
            .context("handling oauth callback")?;
        let (account_did, oauth_session_id) = session.session_info().await;
        let session_token = Uuid::new_v4().to_string();

        insert_app_session(
            &self.db,
            &session_token,
            account_did.as_str(),
            oauth_session_id.as_str(),
            self.config.session_expires_at(),
        )
        .await?;

        Ok(CompletedSignIn { session_token })
    }

    /// Loads the current app session.
    ///
    /// # Errors
    /// Returns an error when the sqlite query fails.
    pub(crate) async fn session(
        &self,
        session_token: Option<&str>,
    ) -> anyhow::Result<Option<AppSession>> {
        lookup_app_session(&self.db, session_token).await
    }

    /// Deletes the current app session.
    ///
    /// # Errors
    /// Returns an error when the sqlite delete fails.
    pub(crate) async fn sign_out(&self, session_token: Option<&str>) -> anyhow::Result<()> {
        delete_app_session(&self.db, session_token).await
    }

    /// Lists configured and persisted admin DIDs.
    ///
    /// # Errors
    /// Returns an error when the sqlite query fails.
    pub(crate) async fn admin_dids(&self) -> anyhow::Result<Vec<String>> {
        let mut dids = self.config.admin_dids.clone();
        let stored =
            sqlx::query_scalar::<_, String>("select did from admin_dids order by created_at asc")
                .fetch_all(self.db.pool())
                .await
                .context("listing admin dids")?;

        for did in stored {
            if !dids.contains(&did) {
                dids.push(did);
            }
        }

        Ok(dids)
    }

    /// Returns whether a DID is an admin.
    ///
    /// # Errors
    /// Returns an error when the sqlite query fails.
    pub(crate) async fn is_admin_did(&self, did: &str) -> anyhow::Result<bool> {
        if self
            .config
            .admin_dids
            .iter()
            .any(|admin_did| admin_did == did)
        {
            return Ok(true);
        }

        sqlx::query_scalar::<_, i64>("select count(*) from admin_dids where did = ?")
            .bind(did)
            .fetch_one(self.db.pool())
            .await
            .map(|count| count > 0)
            .context("checking admin did")
    }

    /// Adds a DID to the persisted admin whitelist.
    ///
    /// # Errors
    /// Returns an error when sqlite persistence fails.
    pub(crate) async fn add_admin_did(&self, did: &str) -> anyhow::Result<()> {
        sqlx::query("insert or ignore into admin_dids (did) values (?)")
            .bind(did.trim())
            .execute(self.db.pool())
            .await
            .context("adding admin did")?;
        Ok(())
    }

    /// Removes a DID from the persisted admin whitelist.
    ///
    /// # Errors
    /// Returns an error when sqlite deletion fails.
    pub(crate) async fn remove_admin_did(&self, did: &str) -> anyhow::Result<()> {
        sqlx::query("delete from admin_dids where did = ?")
            .bind(did)
            .execute(self.db.pool())
            .await
            .context("removing admin did")?;
        Ok(())
    }

    /// Returns auth runtime configuration.
    pub(crate) fn config(&self) -> &AuthConfig {
        &self.config
    }

    /// Serializes the OAuth client metadata to JSON for serving at the client_id URL.
    ///
    /// # Errors
    /// Returns an error when metadata conversion or serialization fails.
    pub(crate) fn client_metadata_json(&self) -> anyhow::Result<String> {
        let data = &self.oauth.registry.client_data;
        let metadata = atproto_client_metadata(data.config.clone(), &data.keyset)
            .context("converting client metadata")?;
        serde_json::to_string(&metadata).context("serializing client metadata")
    }
}

#[derive(Clone)]
struct SqliteAuthStore {
    db: Database,
}

impl SqliteAuthStore {
    fn new(db: Database) -> Self {
        Self { db }
    }
}

impl ClientAuthStore for SqliteAuthStore {
    async fn get_session(
        &self,
        did: &Did<'_>,
        session_id: &str,
    ) -> Result<Option<ClientSessionData<'_>>, SessionStoreError> {
        let payload = sqlx::query_scalar::<_, String>(
            "select session_json from oauth_sessions where account_did = ? and session_id = ?",
        )
        .bind(did.as_str())
        .bind(session_id)
        .fetch_optional(self.db.pool())
        .await
        .map_err(session_store_error)?;

        payload
            .map(|json| {
                serde_json::from_str::<ClientSessionData<'_>>(&json).map(IntoStatic::into_static)
            })
            .transpose()
            .map_err(Into::into)
    }

    async fn upsert_session(
        &self,
        session: ClientSessionData<'_>,
    ) -> Result<(), SessionStoreError> {
        let payload = serde_json::to_string(&session)?;

        sqlx::query(
            r#"
            insert into oauth_sessions (account_did, session_id, session_json)
            values (?, ?, ?)
            on conflict(account_did, session_id) do update set
                session_json = excluded.session_json,
                updated_at = unixepoch()
            "#,
        )
        .bind(session.account_did.as_str())
        .bind(session.session_id.as_str())
        .bind(payload)
        .execute(self.db.pool())
        .await
        .map_err(session_store_error)?;

        Ok(())
    }

    async fn delete_session(
        &self,
        did: &Did<'_>,
        session_id: &str,
    ) -> Result<(), SessionStoreError> {
        sqlx::query("delete from oauth_sessions where account_did = ? and session_id = ?")
            .bind(did.as_str())
            .bind(session_id)
            .execute(self.db.pool())
            .await
            .map_err(session_store_error)?;

        Ok(())
    }

    async fn get_auth_req_info(
        &self,
        state: &str,
    ) -> Result<Option<AuthRequestData<'_>>, SessionStoreError> {
        let payload = sqlx::query_scalar::<_, String>(
            "select auth_request_json from oauth_auth_requests where state = ?",
        )
        .bind(state)
        .fetch_optional(self.db.pool())
        .await
        .map_err(session_store_error)?;

        payload
            .map(|json| {
                serde_json::from_str::<AuthRequestData<'_>>(&json).map(IntoStatic::into_static)
            })
            .transpose()
            .map_err(Into::into)
    }

    async fn save_auth_req_info(
        &self,
        auth_req_info: &AuthRequestData<'_>,
    ) -> Result<(), SessionStoreError> {
        let payload = serde_json::to_string(auth_req_info)?;

        sqlx::query(
            r#"
            insert into oauth_auth_requests (state, auth_request_json)
            values (?, ?)
            on conflict(state) do update set
                auth_request_json = excluded.auth_request_json,
                created_at = unixepoch()
            "#,
        )
        .bind(auth_req_info.state.as_str())
        .bind(payload)
        .execute(self.db.pool())
        .await
        .map_err(session_store_error)?;

        Ok(())
    }

    async fn delete_auth_req_info(&self, state: &str) -> Result<(), SessionStoreError> {
        sqlx::query("delete from oauth_auth_requests where state = ?")
            .bind(state)
            .execute(self.db.pool())
            .await
            .map_err(session_store_error)?;

        Ok(())
    }
}

#[derive(FromRow)]
struct AppSessionRow {
    account_did: String,
}

async fn insert_app_session(
    db: &Database,
    session_token: &str,
    account_did: &str,
    oauth_session_id: &str,
    expires_at: i64,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        insert into app_sessions (session_token, account_did, oauth_session_id, expires_at)
        values (?, ?, ?, ?)
        "#,
    )
    .bind(session_token)
    .bind(account_did)
    .bind(oauth_session_id)
    .bind(expires_at)
    .execute(db.pool())
    .await
    .context("inserting app session")?;

    Ok(())
}

async fn lookup_app_session(
    db: &Database,
    session_token: Option<&str>,
) -> anyhow::Result<Option<AppSession>> {
    let Some(session_token) = session_token else {
        return Ok(None);
    };

    let row = sqlx::query_as::<_, AppSessionRow>(
        r#"
        select account_did
        from app_sessions
        where session_token = ? and expires_at > ?
        "#,
    )
    .bind(session_token)
    .bind(OffsetDateTime::now_utc().unix_timestamp())
    .fetch_optional(db.pool())
    .await
    .context("loading app session")?;

    Ok(row.map(|row| AppSession {
        account_did: row.account_did,
    }))
}

async fn delete_app_session(db: &Database, session_token: Option<&str>) -> anyhow::Result<()> {
    let Some(session_token) = session_token else {
        return Ok(());
    };

    sqlx::query("delete from app_sessions where session_token = ?")
        .bind(session_token)
        .execute(db.pool())
        .await
        .context("deleting app session")?;

    Ok(())
}

fn oauth_client_data(config: &AuthConfig) -> anyhow::Result<ClientData<'static>> {
    let callback_uri = Uri::parse(config.callback_url())
        .map_err(|error| anyhow!("invalid APP_URL callback URI: {error:?}"))?
        .to_owned();

    let scopes = Scope::parse_multiple("atproto")
        .map_err(|error| anyhow!("invalid OAuth scopes: {error}"))?;

    let host = callback_uri.authority().map(|a| a.host()).unwrap_or("");
    let is_loopback = host == "127.0.0.1" || host == "::1" || host == "[::1]";

    if host == "localhost" {
        return Err(anyhow!(
            "APP_URL cannot use localhost for atproto oauth. use a loopback ip like http://127.0.0.1:3000 instead"
        ));
    }

    if is_loopback {
        return Ok(ClientData {
            keyset: None,
            config: AtprotoClientMetadata::new_localhost(Some(vec![callback_uri]), Some(scopes)),
        });
    }

    let client_id = Uri::parse(format!("{}/client-metadata.json", config.app_url))
        .map_err(|error| anyhow!("invalid client_id URI: {error:?}"))?
        .to_owned();
    let client_uri = Uri::parse(config.app_url.clone())
        .map_err(|error| anyhow!("invalid client_uri: {error:?}"))?
        .to_owned();

    Ok(ClientData {
        keyset: None,
        config: AtprotoClientMetadata {
            client_id,
            client_uri: Some(client_uri),
            redirect_uris: vec![callback_uri],
            grant_types: vec![GrantType::AuthorizationCode, GrantType::RefreshToken],
            scopes,
            jwks_uri: None,
            client_name: Some("radio".into()),
            logo_uri: None,
            tos_uri: None,
            privacy_policy_uri: None,
        },
    })
}

fn session_store_error(error: sqlx::Error) -> SessionStoreError {
    SessionStoreError::Other(Box::new(error))
}
