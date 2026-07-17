//! The multi-account domain model: a persisted [`Account`], its plan/tier/
//! routing enums, and the usability, tier-ranking, and health-scoring logic
//! that drives rotation.

use crate::token::{
    AccessToken, Credential, OAuthTokens, REFRESH_LEEWAY_SECS, RefreshToken, TokenAccount,
    TokenOrganization,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// Subscription plan class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanType {
    /// Claude Max subscription.
    #[serde(alias = "max")]
    ClaudeMax,
    /// Claude Pro subscription.
    #[serde(alias = "pro")]
    ClaudePro,
    /// Plan not yet determined.
    #[default]
    Unknown,
}

/// Unified rate-limit routing status reported by the API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingStatus {
    /// Requests permitted.
    Allowed,
    /// Permitted but approaching a limit.
    AllowedWarning,
    /// Currently rejected.
    Rejected,
}

impl RoutingStatus {
    /// Whether requests are permitted (allowed, possibly with a warning).
    pub fn is_allowed(self) -> bool {
        matches!(self, RoutingStatus::Allowed | RoutingStatus::AllowedWarning)
    }
}

/// Per-account, tri-state beta capability flags. `Some(true)` = confirmed
/// supported, `Some(false)` = confirmed unsupported (a 400 was seen),
/// `None` = untested (will attempt).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountCapabilities {
    /// 1M context beta.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context1m: Option<bool>,
    /// AFK mode beta.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub afk_mode: Option<bool>,
    /// Fast mode beta.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast_mode: Option<bool>,
    /// Task budgets beta.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_budgets: Option<bool>,
}

/// A single persisted Anthropic subscription account. The on-disk field names
/// are camelCase to stay compatible with the `anthropic-accounts.json` written
/// by the reference plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    /// Stable primary identity (a user-chosen label or email).
    pub name: String,
    /// The refresh token — the durable secret; always present.
    pub refresh_token: RefreshToken,
    /// The current access token, when one has been fetched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_token: Option<AccessToken>,
    /// Absolute access-token expiry (epoch ms). `None` = lifetime token.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "chrono::serde::ts_milliseconds_option"
    )]
    pub expires_at: Option<DateTime<Utc>>,
    /// Granted scopes, when we recorded them from a token grant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<String>>,
    /// Account email.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// When the account was added (epoch ms).
    #[serde(default)]
    pub added_at: i64,
    /// When the account was last selected (epoch ms).
    #[serde(default)]
    pub last_used: i64,
    /// Account UUID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    /// Subscription plan class.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<PlanType>,
    /// Organization UUID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_uuid: Option<String>,
    /// Organization display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_name: Option<String>,
    /// Raw rate-limit tier string (ranked by [`tier_rank`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_tier: Option<String>,
    /// Whether extra (overage) usage is enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_extra_usage_enabled: Option<bool>,
    /// Whether the account participates in rotation. `Some(false)` skips it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Why the account was disabled, if it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
    /// Tri-state beta capabilities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<AccountCapabilities>,
    /// 5-hour utilization fraction (0.0–1.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utilization5h: Option<f64>,
    /// 7-day utilization fraction (0.0–1.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utilization7d: Option<f64>,
    /// Unified routing status from the last response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unified_status: Option<RoutingStatus>,
    /// When the current rate-limit cooldown clears (epoch ms). `Some(0)` means
    /// explicitly cleared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_reset_time: Option<i64>,
    /// Consecutive refresh failures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_failure_count: Option<u32>,
    /// Last auth error message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_auth_error: Option<String>,
}

impl Account {
    /// Minimal constructor from a name and refresh token (all optional state
    /// defaulted); the token is fetched lazily on first use.
    pub fn new(name: impl Into<String>, refresh_token: RefreshToken) -> Self {
        Self {
            name: name.into(),
            refresh_token,
            access_token: None,
            expires_at: None,
            scopes: None,
            email: None,
            added_at: 0,
            last_used: 0,
            uuid: None,
            plan: None,
            organization_uuid: None,
            organization_name: None,
            rate_limit_tier: None,
            has_extra_usage_enabled: None,
            enabled: None,
            disabled_reason: None,
            capabilities: None,
            utilization5h: None,
            utilization7d: None,
            unified_status: None,
            rate_limit_reset_time: None,
            refresh_failure_count: None,
            last_auth_error: None,
        }
    }

    /// Whether the account participates in rotation (`enabled` unset ⇒ true).
    pub fn is_enabled(&self) -> bool {
        self.enabled != Some(false)
    }

    /// Whether the access token is expired or within the refresh leeway. A
    /// `None` expiry is a lifetime token (never expired); an expiry of exactly
    /// epoch 0 is treated as expired.
    pub fn is_token_expired(&self, now: DateTime<Utc>) -> bool {
        match self.expires_at {
            None => false,
            Some(exp) if exp.timestamp_millis() == 0 => true,
            Some(exp) => now + Duration::seconds(REFRESH_LEEWAY_SECS) >= exp,
        }
    }

    /// Whether any rate-limit cooldown has elapsed.
    pub fn is_rate_limit_cleared(&self, now: DateTime<Utc>) -> bool {
        match self.rate_limit_reset_time {
            None => true,
            Some(reset_ms) => now.timestamp_millis() >= reset_ms,
        }
    }

    /// Ready to send a request right now: enabled, cooldown cleared, and holding
    /// a non-expired access token.
    pub fn is_request_ready(&self, now: DateTime<Utc>) -> bool {
        self.is_enabled()
            && self.is_rate_limit_cleared(now)
            && self.access_token.is_some()
            && !self.is_token_expired(now)
    }

    /// Usable for a request possibly after a refresh: request-ready, or enabled
    /// with a cleared cooldown (the refresh token is always present).
    pub fn is_usable(&self, now: DateTime<Utc>) -> bool {
        self.is_request_ready(now) || (self.is_enabled() && self.is_rate_limit_cleared(now))
    }

    /// Reconstruct the OAuth [`Credential`] from stored fields, if a usable
    /// access token and expiry are present.
    pub fn credential(&self) -> Option<Credential> {
        let access = self.access_token.clone()?;
        let expires_at = self.expires_at?;
        Some(Credential::Oauth(OAuthTokens {
            access,
            refresh: self.refresh_token.clone(),
            expires_at,
            scopes: self.scopes.clone().unwrap_or_default(),
            account: self.uuid.clone().map(|uuid| TokenAccount {
                uuid,
                email_address: self.email.clone(),
            }),
            organization: self
                .organization_uuid
                .clone()
                .map(|uuid| TokenOrganization { uuid }),
        }))
    }

    /// Priority rank of this account's rate-limit tier (higher = preferred).
    pub fn tier_rank(&self) -> u32 {
        tier_rank(self.rate_limit_tier.as_deref().unwrap_or(""))
    }

    /// A composite health score used to order rotation candidates. Higher is
    /// healthier. Combines routing status and recent utilization.
    pub fn health_score(&self) -> i32 {
        let mut score = 0;
        match self.unified_status {
            Some(RoutingStatus::Allowed) => score += 4,
            Some(RoutingStatus::AllowedWarning) => score += 2,
            _ => {}
        }
        let max_util = self
            .utilization5h
            .unwrap_or(0.0)
            .max(self.utilization7d.unwrap_or(0.0));
        if max_util < 0.5 {
            score += 2;
        } else if max_util < 0.7 {
            score += 1;
        } else if max_util >= 0.9 {
            score -= 1;
        }
        score
    }
}

/// Rank a rate-limit tier string (higher = preferred). Substring match on the
/// lowercased tier, mirroring the reference plugin's `getTierRank`.
pub fn tier_rank(tier: &str) -> u32 {
    let t = tier.to_lowercase();
    if t.is_empty() {
        0
    } else if t.contains("raven") {
        100
    } else if t.contains("claude_max_20x") {
        90
    } else if t.contains("claude_max_5x") {
        80
    } else if t.contains("claude_max") {
        70
    } else if t.contains("claude_pro") {
        50
    } else if t.contains("claude_ai") || t.contains("default_claude") {
        40
    } else if t.contains("free") {
        10
    } else {
        30
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000, 0).unwrap()
    }

    fn account_with_token(expiry_secs: i64) -> Account {
        let mut a = Account::new(
            "primary",
            RefreshToken::new("sk-ant-ort01-abcdefghijklmnopqrstuvwxyz012345"),
        );
        a.access_token = Some(AccessToken::new(
            "sk-ant-oat01-abcdefghijklmnopqrstuvwxyz012345",
        ));
        a.expires_at = Some(Utc.timestamp_opt(expiry_secs, 0).unwrap());
        a
    }

    #[test]
    fn tier_rank_orders_plans() {
        assert!(tier_rank("claude_max_20x") > tier_rank("claude_max_5x"));
        assert!(tier_rank("claude_max_5x") > tier_rank("claude_max"));
        assert!(tier_rank("claude_max") > tier_rank("claude_pro"));
        assert!(tier_rank("claude_pro") > tier_rank("free"));
        assert_eq!(tier_rank(""), 0);
        assert_eq!(tier_rank("raven_special"), 100);
    }

    #[test]
    fn plan_type_accepts_reference_plugin_names() {
        assert_eq!(
            serde_json::from_str::<PlanType>("\"max\"").unwrap(),
            PlanType::ClaudeMax
        );
        assert_eq!(
            serde_json::from_str::<PlanType>("\"pro\"").unwrap(),
            PlanType::ClaudePro
        );
    }

    #[test]
    fn request_readiness_and_usability() {
        // Fresh token far in the future → request-ready and usable.
        let ready = account_with_token(now().timestamp() + 3600);
        assert!(ready.is_request_ready(now()));
        assert!(ready.is_usable(now()));

        // Expired token → not request-ready, but still usable (can refresh).
        let expired = account_with_token(now().timestamp() - 10);
        assert!(!expired.is_request_ready(now()));
        assert!(expired.is_usable(now()));

        // Disabled → neither.
        let mut disabled = ready.clone();
        disabled.enabled = Some(false);
        assert!(!disabled.is_request_ready(now()));
        assert!(!disabled.is_usable(now()));
    }

    #[test]
    fn rate_limit_cooldown_gates_usability() {
        let mut a = account_with_token(now().timestamp() + 3600);
        a.rate_limit_reset_time = Some(now().timestamp_millis() + 60_000);
        assert!(!a.is_rate_limit_cleared(now()));
        assert!(!a.is_request_ready(now()));
        a.rate_limit_reset_time = Some(0);
        assert!(a.is_rate_limit_cleared(now()));
    }

    #[test]
    fn credential_reconstructs_oauth_session() {
        let a = account_with_token(now().timestamp() + 3600);
        let cred = a.credential().expect("has token");
        assert!(cred.is_oauth());
    }

    #[test]
    fn lifetime_token_never_expires() {
        let mut a = account_with_token(0);
        a.expires_at = None;
        assert!(!a.is_token_expired(now()));
    }

    #[test]
    fn account_roundtrips_camelcase() {
        let a = account_with_token(now().timestamp() + 3600);
        let json = serde_json::to_string(&a).unwrap();
        assert!(json.contains("\"refreshToken\""));
        assert!(json.contains("\"accessToken\""));
        assert!(json.contains("\"expiresAt\""));
        let back: Account = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, a.name);
        assert_eq!(back.expires_at, a.expires_at);
    }
}
