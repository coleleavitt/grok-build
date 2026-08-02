//! Live credential handle: async refresh + sync cache for per-request auth.
//!
//! The sampler's [`BearerResolver`](https://docs.rs) seam is intentionally
//! synchronous (it runs inside header assembly). OAuth refresh is async. This
//! module bridges the two: callers `ensure_fresh()` before a turn (or when a
//! 401 arrives), and the sampler reads the cached access token via
//! [`LiveCredential::current_bearer`] on every request.

use std::sync::{Arc, Mutex};

use crate::error::Result;
use crate::manager::AnthropicAuthManager;
use crate::request::HeaderMutation;
use crate::store::AccountStore;
use crate::token::{AuthHeader, Credential};
use chrono::Utc;

/// Shared handle around [`AnthropicAuthManager`] with a process-local cache of
/// the last successfully resolved credential.
///
/// Cheap to clone (`Arc` interior). Safe to share across tasks; `ensure_fresh`
/// serializes refresh via the store's own atomic read-modify-write, and the
/// cache is a single mutex.
#[derive(Clone)]
pub struct LiveCredential {
    inner: Arc<LiveCredentialInner>,
}

struct LiveCredentialInner {
    manager: AnthropicAuthManager,
    cache: Mutex<Option<CachedCredential>>,
}

#[derive(Clone)]
struct CachedCredential {
    account_name: String,
    credential: Credential,
}

impl std::fmt::Debug for LiveCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveCredential")
            .field(
                "has_cache",
                &self.inner.cache.lock().ok().is_some_and(|g| g.is_some()),
            )
            .finish()
    }
}

impl LiveCredential {
    /// Wrap an existing manager (tests inject mock endpoints here).
    pub fn new(manager: AnthropicAuthManager) -> Self {
        Self {
            inner: Arc::new(LiveCredentialInner {
                manager,
                cache: Mutex::new(None),
            }),
        }
    }

    /// Production handle: env-resolved store path and OAuth endpoints.
    pub fn from_env() -> Self {
        Self::new(AnthropicAuthManager::from_env())
    }

    /// Underlying manager (for store inspection / login flows).
    pub fn manager(&self) -> &AnthropicAuthManager {
        &self.inner.manager
    }

    /// Whether the on-disk store currently has at least one usable account
    /// (enabled, not permanently rate-limited, has a refresh token). Used to
    /// decide whether to prefer OAuth over a static API key.
    pub fn has_usable_accounts(&self) -> bool {
        let now = Utc::now();
        self.inner
            .manager
            .store()
            .load()
            .map(|data| data.select(now).is_some())
            .unwrap_or(false)
    }

    /// Resolve (refresh if needed), update the cache, and return the credential.
    pub async fn ensure_fresh(&self) -> Result<Credential> {
        let (account_name, credential) = self.inner.manager.resolve_named_credential().await?;
        if let Ok(mut guard) = self.inner.cache.lock() {
            *guard = Some(CachedCredential {
                account_name,
                credential: credential.clone(),
            });
        }
        Ok(credential)
    }

    /// Mark the currently selected account rate-limited and drop the cached
    /// bearer so the next resolve can rotate to another account.
    pub fn record_rate_limit(
        &self,
        retry_after_secs: Option<u64>,
        message: &str,
    ) -> Result<Option<String>> {
        let cached_account = self
            .inner
            .cache
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|c| c.account_name.clone()));
        let marked = if let Some(account) = cached_account {
            self.inner
                .manager
                .record_account_rate_limit(&account, retry_after_secs, message)?
        } else {
            self.inner
                .manager
                .record_selected_rate_limit(retry_after_secs, message)?
        };
        if marked.is_some()
            && let Ok(mut guard) = self.inner.cache.lock()
        {
            *guard = None;
        }
        Ok(marked)
    }

    /// Last cached credential, if any. Does not hit the network.
    pub fn current(&self) -> Option<Credential> {
        self.inner
            .cache
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|c| c.credential.clone()))
    }

    /// Name of the account backing the cached credential, if any. Does not
    /// hit the network — diagnostics for rotation / retry logs.
    pub fn current_account_name(&self) -> Option<String> {
        self.inner
            .cache
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|c| c.account_name.clone()))
    }

    /// Redacted per-account diagnostics snapshot of the on-disk store —
    /// status, cooldown, token expiry — for structured logs when rotation
    /// or retries fail. Empty when the store cannot be read.
    pub fn debug_snapshot(&self) -> Vec<crate::debug::AccountDebug> {
        self.inner
            .manager
            .store()
            .load()
            .map(|data| crate::debug::snapshot_accounts(&data))
            .unwrap_or_default()
    }

    /// Access-token string for sampler `BearerResolver` / construction-time
    /// `api_key`. OAuth only — API-key credentials use `x-api-key` instead
    /// and return `None` here so callers do not stamp both headers.
    pub fn current_bearer(&self) -> Option<String> {
        match self.current()?.auth_header() {
            AuthHeader::Bearer(token) => Some(token),
            AuthHeader::ApiKey(_) => None,
        }
    }

    /// Static API key when the cached credential is key-based.
    pub fn current_api_key(&self) -> Option<String> {
        match self.current()?.auth_header() {
            AuthHeader::ApiKey(key) => Some(key),
            AuthHeader::Bearer(_) => None,
        }
    }

    /// Header mutation for the cached credential (OAuth beta, version, etc.).
    pub fn current_mutation(&self) -> Option<HeaderMutation> {
        Some(HeaderMutation::for_credential(&self.current()?))
    }

    /// Convenience: whether the env store path currently exists as a file.
    pub fn store_path_exists() -> bool {
        AccountStore::from_env().path().exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{Account, RoutingStatus};
    use crate::endpoints::OAuthEndpoints;
    use crate::oauth::OAuthClient;
    use crate::store::{AccountData, AccountStore};
    use crate::token::{AccessToken, RefreshToken};
    use chrono::TimeZone;

    const VALID_ACCESS: &str = "sk-ant-oat01-abcdefghijklmnopqrstuvwxyz012345";
    const VALID_REFRESH: &str = "sk-ant-ort01-abcdefghijklmnopqrstuvwxyz012345";

    fn temp_store(tag: &str) -> (AccountStore, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "grok-anthropic-auth-live-{tag}-{}",
            std::process::id()
        ));
        let path = dir.join("anthropic-accounts.json");
        (AccountStore::at(&path), dir)
    }

    #[tokio::test]
    async fn ensure_fresh_caches_bearer_for_sync_reads() {
        let (store, dir) = temp_store("cache");
        let mut data = AccountData::default();
        let mut account = Account::new("primary", RefreshToken::new(VALID_REFRESH));
        account.access_token = Some(AccessToken::new(VALID_ACCESS));
        account.expires_at = Some(Utc.timestamp_opt(Utc::now().timestamp() + 3600, 0).unwrap());
        data.accounts.push(account);
        store.save(&data).unwrap();

        let endpoints = OAuthEndpoints {
            token_url: "http://127.0.0.1:1/v1/oauth/token".into(),
            ..OAuthEndpoints::prod()
        };
        let live = LiveCredential::new(AnthropicAuthManager::new(
            store,
            OAuthClient::new(endpoints),
        ));
        assert!(live.has_usable_accounts());
        assert!(live.current_bearer().is_none());

        live.ensure_fresh().await.unwrap();
        assert_eq!(live.current_bearer().as_deref(), Some(VALID_ACCESS));
        let mutation = live.current_mutation().unwrap();
        assert!(
            mutation
                .set
                .iter()
                .any(|(k, v)| k == "authorization" && v == &format!("Bearer {VALID_ACCESS}"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn record_rate_limit_clears_cached_bearer() {
        let (store, dir) = temp_store("rate-limit-cache");
        let mut data = AccountData::default();
        let mut account = Account::new("primary", RefreshToken::new(VALID_REFRESH));
        account.access_token = Some(AccessToken::new(VALID_ACCESS));
        account.expires_at = Some(Utc.timestamp_opt(Utc::now().timestamp() + 3600, 0).unwrap());
        data.accounts.push(account);
        store.save(&data).unwrap();

        let endpoints = OAuthEndpoints {
            token_url: "http://127.0.0.1:1/v1/oauth/token".into(),
            ..OAuthEndpoints::prod()
        };
        let live = LiveCredential::new(AnthropicAuthManager::new(
            store,
            OAuthClient::new(endpoints),
        ));
        live.ensure_fresh().await.unwrap();
        assert_eq!(live.current_bearer().as_deref(), Some(VALID_ACCESS));

        assert_eq!(
            live.record_rate_limit(Some(60), "usage exhausted")
                .unwrap()
                .as_deref(),
            Some("primary")
        );
        assert!(live.current_bearer().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn record_rate_limit_marks_cached_account_not_new_selector() {
        let (store, dir) = temp_store("rate-limit-cached-account");
        let mut data = AccountData::default();
        let mut primary = Account::new("primary", RefreshToken::new(VALID_REFRESH));
        primary.access_token = Some(AccessToken::new(VALID_ACCESS));
        primary.expires_at = Some(Utc.timestamp_opt(Utc::now().timestamp() + 3600, 0).unwrap());
        let mut secondary = Account::new("secondary", RefreshToken::new(VALID_REFRESH));
        secondary.access_token = Some(AccessToken::new("sk-ant-oat01-secondtokenvalue000001"));
        secondary.expires_at = Some(Utc.timestamp_opt(Utc::now().timestamp() + 3600, 0).unwrap());
        data.active_index = Some(0);
        data.accounts.push(primary);
        data.accounts.push(secondary);
        store.save(&data).unwrap();

        let endpoints = OAuthEndpoints {
            token_url: "http://127.0.0.1:1/v1/oauth/token".into(),
            ..OAuthEndpoints::prod()
        };
        let live = LiveCredential::new(AnthropicAuthManager::new(
            store.clone(),
            OAuthClient::new(endpoints),
        ));
        live.ensure_fresh().await.unwrap();
        assert_eq!(live.current_bearer().as_deref(), Some(VALID_ACCESS));

        store
            .read_modify_write(|data| data.active_index = Some(1))
            .unwrap();

        assert_eq!(
            live.record_rate_limit(Some(120), "rate_limit_error")
                .unwrap()
                .as_deref(),
            Some("primary")
        );
        let reloaded = store.load().unwrap();
        assert_eq!(
            reloaded.find("primary").unwrap().unified_status,
            Some(RoutingStatus::Rejected)
        );
        assert!(reloaded.find("secondary").unwrap().unified_status.is_none());
        assert!(live.current_bearer().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
