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
    cache: Mutex<Option<Credential>>,
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
        let credential = self.inner.manager.resolve_credential().await?;
        if let Ok(mut guard) = self.inner.cache.lock() {
            *guard = Some(credential.clone());
        }
        Ok(credential)
    }

    /// Last cached credential, if any. Does not hit the network.
    pub fn current(&self) -> Option<Credential> {
        self.inner.cache.lock().ok().and_then(|g| g.clone())
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
    use crate::account::Account;
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
}
