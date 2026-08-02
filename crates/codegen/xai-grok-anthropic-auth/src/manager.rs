//! The end-to-end entry point: ties the credential [`AccountStore`], the
//! [`OAuthClient`], and rotation [`select`](crate::store::AccountData::select)
//! together into "give me a credential ready to sign a request", refreshing and
//! persisting when the selected account's access token is missing or stale.

use crate::account::{Account, RoutingStatus};
use crate::endpoints::OAuthEndpoints;
use crate::error::{AnthropicAuthError, Result};
use crate::oauth::OAuthClient;
use crate::request::HeaderMutation;
use crate::store::{AccountData, AccountStore};
use crate::token::{Credential, OAuthTokens};
use chrono::{DateTime, Duration, Utc};

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
        self.resolve_named_credential()
            .await
            .map(|(_, credential)| credential)
    }

    /// Resolve a credential together with the account name selected to sign the
    /// request.
    pub async fn resolve_named_credential(&self) -> Result<(String, Credential)> {
        let _trace = linkscope::trace("anthropic.resolve_credential");
        let now = Utc::now();
        let data = self.store.load()?;
        let ready_count = data
            .accounts
            .iter()
            .filter(|a| a.is_request_ready(now))
            .count();
        let usable_count = data.accounts.iter().filter(|a| a.is_usable(now)).count();
        let account = data.select(now).ok_or_else(|| {
            tracing::warn!(
                target: crate::debug::TRACE_TARGET,
                total_accounts = data.accounts.len(),
                accounts = ?crate::debug::snapshot_accounts(&data),
                "credential resolve failed: no selectable account"
            );
            linkscope::event_fields(
                "anthropic.resolve.no_account",
                [linkscope::TraceField::count(
                    "total",
                    data.accounts.len() as u64,
                )],
            );
            AnthropicAuthError::ExpiredNoRefresh
        })?;
        let name = account.name.clone();

        if account.is_request_ready(now)
            && let Some(credential) = account.credential()
        {
            tracing::debug!(
                target: crate::debug::TRACE_TARGET,
                account = %name,
                path = "cached",
                ready_count,
                usable_count,
                total_accounts = data.accounts.len(),
                token_expires_in_secs = account
                    .expires_at
                    .map(|at| at.timestamp() - now.timestamp()),
                "credential resolved from store"
            );
            linkscope::event_fields(
                "anthropic.resolve.cached",
                [
                    linkscope::TraceField::text("account", name.clone()),
                    linkscope::TraceField::count("ready", ready_count as u64),
                    linkscope::TraceField::count("usable", usable_count as u64),
                ],
            );
            return Ok((name, credential));
        }

        let refresh_token = account.refresh_token.clone();

        let refresh_started = std::time::Instant::now();
        match self.oauth.refresh(&refresh_token).await {
            Ok(tokens) => {
                self.store.read_modify_write(|data| {
                    if let Some(account) = data.find_mut(&name) {
                        apply_refreshed_tokens(account, &tokens);
                    }
                })?;
                tracing::debug!(
                    target: crate::debug::TRACE_TARGET,
                    account = %name,
                    path = "refreshed",
                    refresh_ms = refresh_started.elapsed().as_millis() as u64,
                    token_prefix = %crate::debug::key_prefix(tokens.access.expose()),
                    token_expires_in_secs = tokens.expires_at.timestamp() - now.timestamp(),
                    "credential refreshed via OAuth"
                );
                linkscope::event_fields(
                    "anthropic.resolve.refreshed",
                    [
                        linkscope::TraceField::text("account", name.clone()),
                        linkscope::TraceField::count(
                            "refresh_ms",
                            refresh_started.elapsed().as_millis() as u64,
                        ),
                    ],
                );
                Ok((name, Credential::Oauth(tokens)))
            }
            Err(err) => {
                tracing::warn!(
                    target: crate::debug::TRACE_TARGET,
                    account = %name,
                    error = %err,
                    permanent = err.is_permanent(),
                    refresh_ms = refresh_started.elapsed().as_millis() as u64,
                    "OAuth refresh failed"
                );
                linkscope::event("anthropic.resolve.refresh_failed", err.to_string());
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
        log_rate_limit_mark("selected", marked.as_deref(), retry_after_secs, reset_at);
        Ok(marked)
    }

    /// Mark a specific account rate-limited. Used when a cached credential has
    /// already signed the failed request.
    pub fn record_account_rate_limit(
        &self,
        account_name: &str,
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
            if let Some(account) = data.find_mut(account_name) {
                account.unified_status = Some(RoutingStatus::Rejected);
                account.rate_limit_reset_time = Some(reset_at.timestamp_millis());
                account.last_auth_error = Some(message.to_owned());
                marked = Some(account_name.to_owned());
            }
        })?;
        log_rate_limit_mark("cached", marked.as_deref(), retry_after_secs, reset_at);
        Ok(marked)
    }

    /// Refresh `/api/oauth/usage` for every enabled account and persist both
    /// the raw response and normalized display/selection fields.
    pub async fn refresh_usage_for_all(&self) -> Result<AccountData> {
        let names: Vec<String> = self
            .store
            .load()?
            .accounts
            .iter()
            .filter(|account| account.is_enabled())
            .map(|account| account.name.clone())
            .collect();
        for name in names {
            self.refresh_usage_for_account(&name).await?;
        }
        self.store.load()
    }

    async fn refresh_usage_for_account(&self, account_name: &str) -> Result<()> {
        let Some(account) = self.store.load()?.find(account_name).cloned() else {
            return Ok(());
        };
        let mut refreshed_tokens = None;
        let mut access = account
            .access_token
            .clone()
            .filter(|_| !account.is_token_expired(Utc::now()));
        if access.is_none() {
            match self.oauth.refresh(&account.refresh_token).await {
                Ok(tokens) => {
                    access = Some(tokens.access.clone());
                    refreshed_tokens = Some(tokens);
                }
                Err(err) => {
                    self.record_usage_error(account_name, err.to_string())?;
                    return Ok(());
                }
            }
        }
        let mut access = access.expect("access is set or usage error returned");
        let usage = match self.oauth.usage(&access).await {
            Ok(usage) => usage,
            Err(AnthropicAuthError::Endpoint { status: 401, .. }) => {
                match self.oauth.refresh(&account.refresh_token).await {
                    Ok(tokens) => {
                        access = tokens.access.clone();
                        refreshed_tokens = Some(tokens);
                        match self.oauth.usage(&access).await {
                            Ok(usage) => usage,
                            Err(err) => {
                                self.record_usage_error(account_name, err.to_string())?;
                                return Ok(());
                            }
                        }
                    }
                    Err(err) => {
                        self.record_usage_error(account_name, err.to_string())?;
                        return Ok(());
                    }
                }
            }
            Err(err) => {
                self.record_usage_error(account_name, err.to_string())?;
                return Ok(());
            }
        };
        let fetched_at_ms = Utc::now().timestamp_millis();
        self.store.read_modify_write(|data| {
            if let Some(account) = data.find_mut(account_name) {
                if let Some(tokens) = &refreshed_tokens {
                    apply_refreshed_tokens(account, tokens);
                }
                apply_usage_snapshot(account, usage.clone(), fetched_at_ms);
            }
        })?;
        Ok(())
    }

    fn record_usage_error(&self, account_name: &str, error: String) -> Result<()> {
        self.store.read_modify_write(|data| {
            if let Some(account) = data.find_mut(account_name) {
                account.usage_error = Some(error);
                account.usage_fetched_at = Some(Utc::now().timestamp_millis());
            }
        })?;
        Ok(())
    }
}

fn apply_usage_snapshot(account: &mut Account, usage: serde_json::Value, fetched_at_ms: i64) {
    account.utilization5h = usage_utilization(&usage, "five_hour");
    account.utilization7d = usage_utilization(&usage, "seven_day");
    account.usage_five_hour_resets_at = usage_reset_ms(&usage, "five_hour");
    account.usage_seven_day_resets_at = usage_reset_ms(&usage, "seven_day");
    account.usage = Some(usage);
    account.usage_fetched_at = Some(fetched_at_ms);
    account.usage_error = None;
}

fn usage_utilization(usage: &serde_json::Value, key: &str) -> Option<f64> {
    let raw = usage.get(key)?.get("utilization")?.as_f64()?;
    Some(if raw > 1.0 { raw / 100.0 } else { raw })
}

fn usage_reset_ms(usage: &serde_json::Value, key: &str) -> Option<i64> {
    let raw = usage.get(key)?.get("resets_at")?.as_str()?;
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

/// One structured line per rate-limit mark: which account, which selection
/// source (`cached` request signer vs freshly-`selected`), and when the
/// cooldown clears — the fields needed to tell rotation churn apart from
/// genuine pool exhaustion in the logs.
fn log_rate_limit_mark(
    source: &'static str,
    account: Option<&str>,
    retry_after_secs: Option<u64>,
    reset_at: chrono::DateTime<Utc>,
) {
    tracing::warn!(
        target: crate::debug::TRACE_TARGET,
        source,
        account = account.unwrap_or("<none>"),
        retry_after_secs,
        cooldown_from_header = retry_after_secs.is_some(),
        reset_at = %reset_at.to_rfc3339(),
        "account marked rate-limited"
    );
    linkscope::event_fields(
        "anthropic.rate_limit.marked",
        [
            linkscope::TraceField::text("source", source),
            linkscope::TraceField::text("account", account.unwrap_or("<none>").to_owned()),
            linkscope::TraceField::count("retry_after_secs", retry_after_secs.unwrap_or(0)),
        ],
    );
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
            error_code: Some(code),
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

    #[tokio::test]
    async fn refresh_usage_for_all_persists_endpoint_usage() {
        let mut server = mockito::Server::new_async().await;
        let usage_body = r#"{
            "five_hour": {
                "utilization": 100.0,
                "resets_at": "2026-07-18T22:09:59.571748+00:00"
            },
            "seven_day": {
                "utilization": 61.0,
                "resets_at": "2026-07-24T06:59:59.571776+00:00"
            },
            "limits": [
                {"kind": "session", "percent": 100, "is_active": true}
            ]
        }"#;
        let mock = server
            .mock("GET", "/api/oauth/usage")
            .match_header("authorization", format!("Bearer {VALID_ACCESS}").as_str())
            .match_header("anthropic-beta", crate::endpoints::OAUTH_BETA)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(usage_body)
            .create_async()
            .await;

        let (store, dir) = temp_store("usage");
        let mut data = AccountData::default();
        let mut account = Account::new("primary", RefreshToken::new(VALID_REFRESH));
        account.access_token = Some(AccessToken::new(VALID_ACCESS));
        account.expires_at = Some(Utc.timestamp_opt(Utc::now().timestamp() + 3600, 0).unwrap());
        data.accounts.push(account);
        store.save(&data).unwrap();

        let endpoints = OAuthEndpoints {
            token_url: "http://127.0.0.1:1/v1/oauth/token".into(),
            usage_url: format!("{}/api/oauth/usage", server.url()),
            ..OAuthEndpoints::prod()
        };
        let manager = AnthropicAuthManager::new(store.clone(), OAuthClient::new(endpoints));

        let refreshed = manager.refresh_usage_for_all().await.unwrap();
        mock.assert_async().await;
        let account = refreshed.find("primary").unwrap();
        assert_eq!(account.utilization5h, Some(1.0));
        assert_eq!(account.utilization7d, Some(0.61));
        assert!(account.usage_five_hour_resets_at.is_some());
        assert!(account.usage_seven_day_resets_at.is_some());
        assert_eq!(account.usage_error, None);
        assert_eq!(
            account
                .usage
                .as_ref()
                .and_then(|usage| usage.get("limits"))
                .and_then(|limits| limits.as_array())
                .map(Vec::len),
            Some(1)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rate_limit_mark_can_target_cached_account() {
        let (store, dir) = temp_store("rate-limit-exact");
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
                .record_account_rate_limit("b", Some(120), "rate_limit_error")
                .unwrap()
                .as_deref(),
            Some("b")
        );
        let reloaded = store.load().unwrap();
        assert!(reloaded.find("a").unwrap().unified_status.is_none());
        assert_eq!(
            reloaded.find("b").unwrap().unified_status,
            Some(RoutingStatus::Rejected)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
