use std::collections::HashSet;

use anyhow::{Context, anyhow};
use jacquard::{
    deps::fluent_uri::Uri,
    oauth::{
        atproto::{AtprotoClientMetadata, GrantType, atproto_client_metadata},
        scopes::Scope,
        session::ClientData,
    },
};

use crate::db::Database;

pub(crate) const ATPROTO_OAUTH_SCOPE: &str = concat!(
    "atproto ",
    "rpc?aud=*",
    "&lxm=pet.nkp.radio.admin.modify",
    "&lxm=pet.nkp.radio.admin.permissions",
    "&lxm=pet.nkp.radio.albums.list",
    "&lxm=pet.nkp.radio.albums.modify",
    "&lxm=pet.nkp.radio.chat.bans.list",
    "&lxm=pet.nkp.radio.chat.bans.modify",
    "&lxm=pet.nkp.radio.chat.messages.modify",
    "&lxm=pet.nkp.radio.chat.send",
    "&lxm=pet.nkp.radio.control",
    "&lxm=pet.nkp.radio.playlists.list",
    "&lxm=pet.nkp.radio.playlists.modify",
    "&lxm=pet.nkp.radio.queue.list",
    "&lxm=pet.nkp.radio.queue.modify",
    "&lxm=pet.nkp.radio.songs.add",
    "&lxm=pet.nkp.radio.songs.cover",
    "&lxm=pet.nkp.radio.songs.list",
    "&lxm=pet.nkp.radio.songs.modify",
    "&lxm=pet.nkp.radio.songs.upload",
    "&lxm=pet.nkp.radio.subsonic.import",
    "&lxm=pet.nkp.radio.subsonic.search",
);

/// Auth-specific runtime configuration.
#[derive(Clone, Debug)]
pub(crate) struct AuthConfig {
    /// Public backend URL used for OAuth client metadata.
    pub(crate) app_url: String,
    /// Frontend URL used as the browser OAuth redirect base.
    pub(crate) frontend_url: String,
    /// Admin DIDs allowed to manage privileged radio features.
    pub(crate) admin_dids: Vec<String>,
}

impl AuthConfig {
    /// Returns the frontend OAuth callback URL used by client-side OAuth.
    pub(crate) fn frontend_callback_url(&self) -> String {
        format!("{}/auth", self.frontend_url)
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

/// Auth service for client metadata and admin DID whitelisting.
#[derive(Clone)]
pub(crate) struct AuthService {
    config: AuthConfig,
    db: Database,
    client_data: ClientData<'static>,
}

impl AuthService {
    /// Creates a new auth service.
    ///
    /// # Errors
    /// Returns an error when OAuth client metadata cannot be constructed.
    pub(crate) fn new(config: AuthConfig, db: Database) -> anyhow::Result<Self> {
        let client_data = oauth_client_data(&config)?;

        Ok(Self {
            config,
            db,
            client_data,
        })
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

    /// Serializes the OAuth client metadata to JSON for serving at the client_id URL.
    ///
    /// # Errors
    /// Returns an error when metadata conversion or serialization fails.
    pub(crate) fn client_metadata_json(&self) -> anyhow::Result<String> {
        let metadata =
            atproto_client_metadata(self.client_data.config.clone(), &self.client_data.keyset)
                .context("converting client metadata")?;
        let mut metadata =
            serde_json::to_value(&metadata).context("serializing client metadata")?;
        metadata["scope"] = serde_json::Value::String(ATPROTO_OAUTH_SCOPE.to_owned());
        serde_json::to_string(&metadata).context("serializing client metadata")
    }
}

fn oauth_client_data(config: &AuthConfig) -> anyhow::Result<ClientData<'static>> {
    let frontend_callback_uri = Uri::parse(config.frontend_callback_url())
        .map_err(|error| anyhow!("invalid FRONTEND_URL callback URI: {error:?}"))?
        .to_owned();

    let scopes = Scope::parse_multiple(ATPROTO_OAUTH_SCOPE)
        .map_err(|error| anyhow!("invalid OAuth scopes: {error}"))?;

    let host = frontend_callback_uri
        .authority()
        .map(|authority| authority.host())
        .unwrap_or("");
    let is_loopback = host == "127.0.0.1" || host == "::1" || host == "[::1]";

    if host == "localhost" {
        return Err(anyhow!(
            "FRONTEND_URL cannot use localhost for atproto oauth. use a loopback ip like http://127.0.0.1:5173 instead"
        ));
    }

    if is_loopback {
        return Ok(ClientData {
            keyset: None,
            config: AtprotoClientMetadata::new_localhost(
                Some(vec![frontend_callback_uri]),
                Some(scopes),
            ),
        });
    }

    let client_id = Uri::parse(format!("{}/client-metadata.json", config.app_url))
        .map_err(|error| anyhow!("invalid client_id URI: {error:?}"))?
        .to_owned();
    let client_uri = Uri::parse(config.frontend_url.clone())
        .map_err(|error| anyhow!("invalid client_uri: {error:?}"))?
        .to_owned();

    Ok(ClientData {
        keyset: None,
        config: AtprotoClientMetadata {
            client_id,
            client_uri: Some(client_uri),
            redirect_uris: vec![frontend_callback_uri],
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
