//! Turning a resolved [`Credential`] into concrete request-header mutations.
//!
//! This is the reusable Anthropic OAuth header contract: the bearer token (or
//! api key), the `anthropic-beta: oauth-2025-04-20` opt-in, and
//! `anthropic-version`. Per-request Claude Code identity, billing prompt, and
//! CCH body attestation live in the sampler's Anthropic adapter.

use crate::endpoints::{ANTHROPIC_VERSION, OAUTH_BETA};
use crate::token::{AuthHeader, Credential};

/// The header changes a credential requires on an outgoing Anthropic request.
///
/// `set` overwrites, `remove` deletes, and `ensure_beta` names beta tokens that
/// must be present in the (comma-joined) `anthropic-beta` header — the caller
/// merges these with any betas it already sends rather than overwriting them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeaderMutation {
    /// Headers to set outright.
    pub set: Vec<(String, String)>,
    /// Header names to remove.
    pub remove: Vec<String>,
    /// Beta tokens that must appear in `anthropic-beta` (merge, don't clobber).
    pub ensure_beta: Vec<String>,
}

impl HeaderMutation {
    /// The mutation for a resolved credential. OAuth and API-key auth are
    /// mutually exclusive, so each removes the other's header.
    pub fn for_credential(credential: &Credential) -> Self {
        let mut set = vec![("anthropic-version".to_owned(), ANTHROPIC_VERSION.to_owned())];
        let mut remove = Vec::new();
        let mut ensure_beta = Vec::new();
        match credential.auth_header() {
            AuthHeader::Bearer(token) => {
                set.push(("authorization".to_owned(), format!("Bearer {token}")));
                ensure_beta.push(OAUTH_BETA.to_owned());
                remove.push("x-api-key".to_owned());
            }
            AuthHeader::ApiKey(key) => {
                set.push(("x-api-key".to_owned(), key));
                remove.push("authorization".to_owned());
            }
        }
        Self {
            set,
            remove,
            ensure_beta,
        }
    }

    /// Merge [`HeaderMutation::ensure_beta`] into an existing `anthropic-beta`
    /// value (comma-separated), preserving order and dropping duplicates.
    pub fn merge_beta(&self, existing: &str) -> String {
        let mut betas: Vec<&str> = existing
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        for want in &self.ensure_beta {
            if !betas.contains(&want.as_str()) {
                betas.push(want);
            }
        }
        betas.join(",")
    }

    /// Apply this mutation to a `reqwest` header map: removals first, then
    /// sets, then merge `ensure_beta` into `anthropic-beta`.
    pub fn apply_to_header_map(&self, headers: &mut reqwest::header::HeaderMap) {
        use reqwest::header::{HeaderName, HeaderValue};

        for name in &self.remove {
            if let Ok(n) = HeaderName::try_from(name.as_str()) {
                headers.remove(n);
            }
        }
        for (key, value) in &self.set {
            let Ok(name) = HeaderName::try_from(key.as_str()) else {
                continue;
            };
            let Ok(value) = HeaderValue::from_str(value) else {
                continue;
            };
            headers.insert(name, value);
        }
        if !self.ensure_beta.is_empty() {
            let existing = headers
                .get("anthropic-beta")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            let merged = self.merge_beta(existing);
            if let Ok(value) = HeaderValue::from_str(&merged)
                && let Ok(name) = HeaderName::try_from("anthropic-beta")
            {
                headers.insert(name, value);
            }
        }
    }

    /// Non-auth header sets (skips `authorization` / `x-api-key`) for folding
    /// into construction-time `extra_headers`. Includes a merged
    /// `anthropic-beta` when [`Self::ensure_beta`] is non-empty.
    pub fn non_auth_headers(&self, existing_beta: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (key, value) in &self.set {
            if key.eq_ignore_ascii_case("authorization") || key.eq_ignore_ascii_case("x-api-key") {
                continue;
            }
            out.push((key.clone(), value.clone()));
        }
        if !self.ensure_beta.is_empty() {
            out.push(("anthropic-beta".to_owned(), self.merge_beta(existing_beta)));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::{AccessToken, ApiKey, OAuthTokens, RefreshToken};
    use chrono::{TimeZone, Utc};

    fn oauth_credential() -> Credential {
        Credential::Oauth(OAuthTokens {
            access: AccessToken::new("sk-ant-oat01-access"),
            refresh: RefreshToken::new("sk-ant-ort01-refresh"),
            expires_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
            scopes: vec!["user:inference".into()],
            account: None,
            organization: None,
        })
    }

    #[test]
    fn oauth_sets_bearer_and_removes_api_key() {
        let mutation = HeaderMutation::for_credential(&oauth_credential());
        assert!(
            mutation
                .set
                .contains(&("authorization".into(), "Bearer sk-ant-oat01-access".into()))
        );
        assert!(mutation.remove.contains(&"x-api-key".to_owned()));
        assert_eq!(mutation.ensure_beta, vec![OAUTH_BETA.to_owned()]);
    }

    #[test]
    fn api_key_sets_x_api_key_and_removes_authorization() {
        let cred = Credential::ApiKey(ApiKey::new("sk-ant-api01-static"));
        let mutation = HeaderMutation::for_credential(&cred);
        assert!(
            mutation
                .set
                .contains(&("x-api-key".into(), "sk-ant-api01-static".into()))
        );
        assert!(mutation.remove.contains(&"authorization".to_owned()));
        assert!(mutation.ensure_beta.is_empty());
    }

    #[test]
    fn merge_beta_adds_without_clobbering_or_duplicating() {
        let mutation = HeaderMutation::for_credential(&oauth_credential());
        assert_eq!(
            mutation.merge_beta("prompt-caching-2024-07-31"),
            format!("prompt-caching-2024-07-31,{OAUTH_BETA}")
        );
        // Already present → no duplicate.
        assert_eq!(mutation.merge_beta(OAUTH_BETA), OAUTH_BETA);
        // Empty existing.
        assert_eq!(mutation.merge_beta(""), OAUTH_BETA);
    }
}
