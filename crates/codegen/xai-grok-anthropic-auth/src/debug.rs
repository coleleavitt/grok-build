//! Auth-flow debug instrumentation: redaction helpers, per-account
//! snapshots for structured logs, and the `GROK_AUTH_TRACE` gate that turns
//! on `linkscope` call-flow tracing.
//!
//! Everything here is diagnostics-only: no control flow depends on it, and
//! secrets never leave as more than a short prefix ([`key_prefix`]).
//!
//! Enablement:
//!
//! - `GROK_AUTH_TRACE=1` — turn on `linkscope` tracing (near-zero cost when
//!   off). The shell dumps `linkscope::trace_tree()` into the log directory
//!   when a turn exhausts its auth/rotation retries.
//! - `tracing` events are always emitted at `debug!`/`info!` level with the
//!   `anthropic_auth` target; route them via `RUST_LOG=anthropic_auth=debug`.

use chrono::Utc;

use crate::account::Account;
use crate::store::AccountData;

/// `tracing` target for all auth-flow debug events, so they can be enabled
/// in isolation (`RUST_LOG=anthropic_auth=debug`).
pub const TRACE_TARGET: &str = "anthropic_auth";

/// True when `GROK_AUTH_TRACE` is set to a truthy value. First truthy read
/// also enables `linkscope` tracing, exactly once.
pub fn trace_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        let on = std::env::var("GROK_AUTH_TRACE")
            .map(|v| {
                let v = v.trim();
                !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false")
            })
            .unwrap_or(false);
        if on {
            linkscope::trace_enable();
        }
        on
    })
}

/// A safe display prefix for a secret (token / api key): the first 12
/// characters, never more than half the secret. Enough to tell two
/// credentials apart (`sk-ant-oat01…` vs `sk-ant-api03…`) without leaking
/// usable material.
pub fn key_prefix(secret: &str) -> String {
    let max = (secret.len() / 2).min(12);
    secret.chars().take(max).collect()
}

/// Diagnostics projection of one [`Account`] — everything a log line needs
/// to explain selection/rotation decisions, and nothing secret.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AccountDebug {
    pub name: String,
    /// `unified_status` as a string (`"Allowed"`, `"Rejected"`, …).
    pub status: Option<String>,
    /// Seconds until the rate-limit cooldown clears; `0` when cleared,
    /// `None` when the account was never limited.
    pub rate_limit_clears_in_secs: Option<i64>,
    /// Seconds until the access token expires (negative = expired);
    /// `None` when there is no access token.
    pub token_expires_in_secs: Option<i64>,
    /// Safe prefix of the access token, to correlate with request logs.
    pub token_prefix: Option<String>,
    pub request_ready: bool,
    pub usable: bool,
}

impl AccountDebug {
    pub fn of(account: &Account) -> Self {
        let now = Utc::now();
        Self {
            name: account.name.clone(),
            status: account.unified_status.map(|s| format!("{s:?}")),
            rate_limit_clears_in_secs: account
                .rate_limit_reset_time
                .map(|ms| ((ms - now.timestamp_millis()) / 1000).max(0)),
            token_expires_in_secs: account
                .expires_at
                .map(|at| at.timestamp() - now.timestamp()),
            token_prefix: account
                .access_token
                .as_ref()
                .map(|t| key_prefix(t.expose())),
            request_ready: account.is_request_ready(now),
            usable: account.is_usable(now),
        }
    }
}

/// Snapshot every account for a structured log payload. Order matches the
/// store, so the sticky/active account is identifiable by name.
pub fn snapshot_accounts(data: &AccountData) -> Vec<AccountDebug> {
    data.accounts.iter().map(AccountDebug::of).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::RoutingStatus;
    use crate::token::{AccessToken, RefreshToken};

    #[test]
    fn key_prefix_never_exposes_more_than_half() {
        assert_eq!(key_prefix("sk-ant-oat01-abcdefghijklmnop"), "sk-ant-oat01");
        assert_eq!(key_prefix("abcd"), "ab");
        assert_eq!(key_prefix(""), "");
    }

    #[test]
    fn account_debug_reports_cooldown_and_expiry() {
        let now = Utc::now();
        let mut account = Account::new("a", RefreshToken::new("sk-ant-ort01-refreshrefresh"));
        account.access_token = Some(AccessToken::new("sk-ant-oat01-tokentokentoken01"));
        account.expires_at = Some(now + chrono::Duration::seconds(3600));
        account.unified_status = Some(RoutingStatus::Rejected);
        account.rate_limit_reset_time =
            Some((now + chrono::Duration::seconds(120)).timestamp_millis());

        let debug = AccountDebug::of(&account);
        assert_eq!(debug.name, "a");
        assert_eq!(debug.status.as_deref(), Some("Rejected"));
        let cooldown = debug.rate_limit_clears_in_secs.unwrap();
        assert!((115..=120).contains(&cooldown), "cooldown {cooldown}");
        let expiry = debug.token_expires_in_secs.unwrap();
        assert!((3595..=3600).contains(&expiry), "expiry {expiry}");
        assert_eq!(debug.token_prefix.as_deref(), Some("sk-ant-oat01"));
        assert!(!debug.request_ready, "cooling account is not request-ready");
    }

    #[test]
    fn snapshot_lists_all_accounts_in_store_order() {
        let mut data = AccountData::default();
        data.accounts
            .push(Account::new("b", RefreshToken::new("sk-ant-ort01-x")));
        data.accounts
            .push(Account::new("a", RefreshToken::new("sk-ant-ort01-y")));
        let snap = snapshot_accounts(&data);
        assert_eq!(
            snap.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            vec!["b", "a"]
        );
    }
}
