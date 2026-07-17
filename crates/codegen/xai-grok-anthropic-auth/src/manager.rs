//! The end-to-end entry point: ties the credential [`AccountStore`], the
//! [`OAuthClient`], and rotation [`select`](crate::store::AccountData::select)
//! together into "give me a credential ready to sign a request", refreshing and
//! persisting when the selected account's access token is missing or stale.

use crate::account::{Account, RoutingStatus};
use crate::endpoints::OAuthEndpoints;
use crate::error::{AnthropicAuthError, Result};
use crate::oauth::OAuthClient;
use crate::request::HeaderMutation;
use crate::store::AccountStore;
use crate::token::{Credential, OAuthTokens};
use chrono::{Duration, Utc};

// ponytail: fallback only when Anthropic omits Retry-After; replace with
// provider reset metadata if Anthropic exposes a stable per-account reset.
const DEFAULT_RATE_LIMIT_COOLDOWN_SECS: i64 = 5 * 60;

/// Composes credential storage and the OAuth client into the high-level
/// operations a plugin transport needs.
pub struct AnthropicAuthManager {
    store: AccountStore,
    oauth: OAuthClient,
}

impl AnthropicAuthManager {
    /// Compose from an explicit store and OAuth client.
    pub fn new(store: AccountStore, oauth: OAuthClient) -> Self {
        Self { store, oauth }
    }

    /// The production manager: env-resolved store path and OAuth endpoints.
    pub fn from_env() -> Self {
        Self {
            store: AccountStore::from_env(),
            oauth: OAuthClient::new(OAuthEndpoints::from_env()),
        }
    }

    /// The underlying credential store.
    pub fn store(&self) -> &AccountStore {
        &self.store
    }

    /// Resolve a credential ready to authenticate a request. Selects an account
    /// by rotation policy; if its access token is present and unexpired, returns
    /// it directly, otherwise refreshes, persists the rotated tokens, and
    /// returns the fresh credential. A permanent refresh failure disables the
    /// account before propagating.
    pub async fn resolve_credential(&self) -> Result<Credential> {
        let now = Utc::now();
        let data = self.store.load()?;
        let account = data
            .select(now)
            .ok_or(AnthropicAuthError::ExpiredNoRefresh)?;

        if account.is_request_ready(now) {
            if let Some(credential) = account.credential() {
                return Ok(credential);
            }
        }

        let name = account.name.clone();
        let refresh_token = account.refresh_token.clone();

        match self.oauth.refresh(&refresh_token).await {
            Ok(tokens) => {
                self.store.read_modify_write(|data| {
                    if let Some(account) = data.find_mut(&name) {
                        apply_refreshed_tokens(account, &tokens);
                    }
                })?;
                Ok(Credential::Oauth(tokens))
            }
            Err(err) => {
                if err.is_permanent() {
                    let reason = disabled_reason(&err);
                    let _ = self.store.read_modify_write(|data| {
                        if let Some(account) = data.find_mut(&name) {
                            account.enabled = Some(false);
                            account.disabled_reason = Some(reason);
                            account.last_auth_error = Some(err.to_string());
                            account.refresh_failure_count =
                                Some(account.refresh_failure_count.unwrap_or(0) + 1);
                        }
                    });
                }
                Err(err)
            }
        }
    }

    /// Resolve a credential and produce the header mutation to apply to the
    /// outgoing request.
    pub async fn resolve_headers(&self) -> Result<HeaderMutation> {
        let credential = self.resolve_credential().await?;
        Ok(HeaderMutation::for_credential(&credential))
    }

    /// Mark the account selected by current rotation as rate-limited so the
    /// next resolve can pick another usable account.
    pub fn record_selected_rate_limit(
        &self,
        retry_after_secs: Option<u64>,
        message: &str,
    ) -> Result<Option<String>> {
        let now = Utc::now();
        let reset_at = now
            + Duration::seconds(
                retry_after_secs
                    .and_then(|s| i64::try_from(s).ok())
                    .unwrap_or(DEFAULT_RATE_LIMIT_COOLDOWN_SECS),
            );
        let mut marked = None;
        self.store.read_modify_write(|data| {
            let Some(name) = data.select(now).map(|a| a.name.clone()) else {
                return;
            };
            if let Some(account) = data.find_mut(&name) {
                account.unified_status = Some(RoutingStatus::Rejected);
                account.rate_limit_reset_time = Some(reset_at.timestamp_millis());
                account.last_auth_error = Some(message.to_owned());
                marked = Some(name);
            }
        })?;
        Ok(marked)
    }
}

/// Write freshly refreshed tokens back onto the account and clear failure state.
fn apply_refreshed_tokens(account: &mut Account, tokens: &OAuthTokens) {
    account.access_token = Some(tokens.access.clone());
    account.refresh_token = tokens.refresh.clone();
    account.expires_at = Some(tokens.expires_at);
    if !tokens.scopes.is_empty() {
        account.scopes = Some(tokens.scopes.clone());
    }
    account.rate_limit_reset_time = Some(0);
    account.refresh_failure_count = Some(0);
    account.last_auth_error = None;
    account.enabled = Some(true);
}

/// The `disabled_reason` to record for a permanent refresh failure.
fn disabled_reason(err: &AnthropicAuthError) -> String {
    match err {
        AnthropicAuthError::Endpoint {
            oauth_error: Some(code),
            ..
        } => code.clone(),
        _ => "permanent_oauth_error".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{AccountData, AccountStore};
    use crate::token::{AccessToken, RefreshToken};
    use chrono::{TimeZone, Utc};

    const VALID_ACCESS: &str = "sk-ant-oat01-abcdefghijklmnopqrstuvwxyz012345";
    const VALID_REFRESH: &str = "sk-ant-ort01-abcdefghijklmnopqrstuvwxyz012345";
    const REFRESHED_ACCESS: &str = "sk-ant-oat01-REFRESHEDtokenvalue000001";

    fn temp_store(tag: &str) -> (AccountStore, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "grok-anthropic-auth-mgr-{tag}-{}",
            std::process::id()
        ));
        let path = dir.join("anthropic-accounts.json");
        (AccountStore::at(&path), dir)
    }

    #[tokio::test]
    async fn ready_account_resolves_without_network() {
        let (store, dir) = temp_store("ready");
        let mut data = AccountData::default();
        let mut account = Account::new("primary", RefreshToken::new(VALID_REFRESH));
        account.access_token = Some(AccessToken::new(VALID_ACCESS));
        account.expires_at = Some(Utc.timestamp_opt(Utc::now().timestamp() + 3600, 0).unwrap());
        data.accounts.push(account);
        store.save(&data).unwrap();

        // OAuth client points at an unroutable URL: a ready account must not
        // touch the network.
        let endpoints = OAuthEndpoints {
            token_url: "http://127.0.0.1:1/v1/oauth/token".into(),
            ..OAuthEndpoints::prod()
        };
        let manager = AnthropicAuthManager::new(store, OAuthClient::new(endpoints));
        let credential = manager.resolve_credential().await.unwrap();
        assert!(credential.is_oauth());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn stale_account_refreshes_and_persists() {
        let mut server = mockito::Server::new_async().await;
        let body = format!(
            r#"{{"access_token":"{REFRESHED_ACCESS}","refresh_token":"{VALID_REFRESH}",
                 "expires_in":3600,"token_type":"Bearer",
                 "scope":"user:profile user:inference"}}"#
        );
        let mock = server
            .mock("POST", "/v1/oauth/token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let (store, dir) = temp_store("stale");
        let mut data = AccountData::default();
        // No access token → not request-ready → must refresh.
        data.accounts
            .push(Account::new("primary", RefreshToken::new(VALID_REFRESH)));
        store.save(&data).unwrap();

        let endpoints = OAuthEndpoints {
            token_url: format!("{}/v1/oauth/token", server.url()),
            ..OAuthEndpoints::prod()
        };
        let manager = AnthropicAuthManager::new(store.clone(), OAuthClient::new(endpoints));

        let credential = manager.resolve_credential().await.unwrap();
        assert!(credential.is_oauth());
        mock.assert_async().await;

        // The refreshed access token was persisted.
        let reloaded = store.load().unwrap();
        let account = reloaded.find("primary").unwrap();
        assert_eq!(
            account.access_token.as_ref().unwrap().expose(),
            REFRESHED_ACCESS
        );
        assert!(account.is_request_ready(Utc::now()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn permanent_refresh_failure_disables_account() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/oauth/token")
            .with_status(400)
            .with_header("content-type", "application/json")
            .with_body(r#"{"error":"invalid_grant"}"#)
            .create_async()
            .await;

        let (store, dir) = temp_store("perm-fail");
        let mut data = AccountData::default();
        data.accounts
            .push(Account::new("primary", RefreshToken::new(VALID_REFRESH)));
        store.save(&data).unwrap();

        let endpoints = OAuthEndpoints {
            token_url: format!("{}/v1/oauth/token", server.url()),
            ..OAuthEndpoints::prod()
        };
        let manager = AnthropicAuthManager::new(store.clone(), OAuthClient::new(endpoints));

        let err = manager.resolve_credential().await.unwrap_err();
        assert!(err.is_permanent());
        mock.assert_async().await;

        let reloaded = store.load().unwrap();
        let account = reloaded.find("primary").unwrap();
        assert_eq!(account.enabled, Some(false));
        assert_eq!(account.disabled_reason.as_deref(), Some("invalid_grant"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn rate_limit_mark_rotates_to_next_ready_account() {
        let (store, dir) = temp_store("rate-limit-rotate");
        let mut data = AccountData::default();
        let mut first = Account::new("a", RefreshToken::new(VALID_REFRESH));
        first.access_token = Some(AccessToken::new(VALID_ACCESS));
        first.expires_at = Some(Utc.timestamp_opt(Utc::now().timestamp() + 3600, 0).unwrap());
        let mut second = Account::new("b", RefreshToken::new(VALID_REFRESH));
        second.access_token = Some(AccessToken::new("sk-ant-oat01-secondtokenvalue000001"));
        second.expires_at = Some(Utc.timestamp_opt(Utc::now().timestamp() + 3600, 0).unwrap());
        data.accounts.push(first);
        data.accounts.push(second);
        store.save(&data).unwrap();

        let endpoints = OAuthEndpoints {
            token_url: "http://127.0.0.1:1/v1/oauth/token".into(),
            ..OAuthEndpoints::prod()
        };
        let manager = AnthropicAuthManager::new(store.clone(), OAuthClient::new(endpoints));

        assert_eq!(
            manager
                .record_selected_rate_limit(Some(60), "Claude Fable usage exhausted")
                .unwrap()
                .as_deref(),
            Some("a")
        );
        let credential = manager.resolve_credential().await.unwrap();
        match credential {
            Credential::Oauth(tokens) => {
                assert_eq!(
                    tokens.access.expose(),
                    "sk-ant-oat01-secondtokenvalue000001"
                );
            }
            _ => panic!("expected oauth credential"),
        }
        let reloaded = store.load().unwrap();
        let first = reloaded.find("a").unwrap();
        assert_eq!(first.unified_status, Some(RoutingStatus::Rejected));
        assert!(first.rate_limit_reset_time.unwrap() > Utc::now().timestamp_millis());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
