//! Anthropic OAuth endpoints, client id, scopes, and header constants.
//!
//! Ported verbatim from the Claude Code CLI production configuration
//! (v2.1.207, cross-checked against v2.1.141). Runtime env overrides mirror the
//! CLI: `CLAUDE_CODE_OAUTH_CLIENT_ID` overrides the client id.

use std::fmt;

/// Production public OAuth client id (PKCE flow; no client secret).
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

/// `anthropic-beta` value that opts an API request into OAuth bearer auth.
pub const OAUTH_BETA: &str = "oauth-2025-04-20";

/// `anthropic-version` sent on every Anthropic API request.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// OAuth token endpoint (authorization-code exchange and refresh grant).
pub const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";

/// Authorize endpoint for Claude.ai subscription login.
pub const CLAUDE_AI_AUTHORIZE_URL: &str = "https://claude.com/cai/oauth/authorize";

/// Authorize endpoint for Console / API-key login.
pub const CONSOLE_AUTHORIZE_URL: &str = "https://platform.claude.com/oauth/authorize";

/// Manual (copy/paste) redirect target; the callback page shows `code#state`.
pub const MANUAL_REDIRECT_URL: &str = "https://platform.claude.com/oauth/code/callback";

/// Endpoint that mints a long-lived `sk-ant-api01-*` key from an OAuth session.
pub const CREATE_API_KEY_URL: &str =
    "https://api.anthropic.com/api/oauth/claude_cli/create_api_key";

/// Base Anthropic API origin.
pub const BASE_API_URL: &str = "https://api.anthropic.com";

/// Environment variable that overrides [`CLIENT_ID`] at runtime.
pub const CLIENT_ID_ENV: &str = "CLAUDE_CODE_OAUTH_CLIENT_ID";

/// A single OAuth scope. [`Scope::as_str`] yields the exact wire literal
/// (which contains colons), so this cannot rely on serde `rename_all`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    /// `org:create_api_key` — permits minting an `sk-ant-api01-*` key.
    OrgCreateApiKey,
    /// `user:profile` — read the account profile.
    UserProfile,
    /// `user:inference` — the scope that enables model inference; a session
    /// without it cannot be used to sample.
    UserInference,
    /// `user:sessions:claude_code`.
    UserSessionsClaudeCode,
    /// `user:mcp_servers`.
    UserMcpServers,
    /// `user:file_upload`.
    UserFileUpload,
}

impl Scope {
    /// The exact wire string for this scope.
    pub const fn as_str(self) -> &'static str {
        match self {
            Scope::OrgCreateApiKey => "org:create_api_key",
            Scope::UserProfile => "user:profile",
            Scope::UserInference => "user:inference",
            Scope::UserSessionsClaudeCode => "user:sessions:claude_code",
            Scope::UserMcpServers => "user:mcp_servers",
            Scope::UserFileUpload => "user:file_upload",
        }
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Scope set requested at authorization (includes `org:create_api_key`, in the
/// order the CLI sends them).
pub const AUTHORIZE_SCOPES: [Scope; 6] = [
    Scope::OrgCreateApiKey,
    Scope::UserProfile,
    Scope::UserInference,
    Scope::UserSessionsClaudeCode,
    Scope::UserMcpServers,
    Scope::UserFileUpload,
];

/// Scope set sent on the refresh grant (the CLI omits `org:create_api_key`).
pub const REFRESH_SCOPES: [Scope; 5] = [
    Scope::UserProfile,
    Scope::UserInference,
    Scope::UserSessionsClaudeCode,
    Scope::UserMcpServers,
    Scope::UserFileUpload,
];

/// Space-join a scope set into a `scope` query parameter / body field value.
pub fn scope_param(scopes: &[Scope]) -> String {
    scopes
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether a granted-scope list (space-separated, as returned by the token
/// endpoint) permits inference. A session that cannot infer is unusable.
pub fn grants_inference<S: AsRef<str>>(granted: &[S]) -> bool {
    granted
        .iter()
        .any(|s| s.as_ref() == Scope::UserInference.as_str())
}

/// The concrete set of OAuth URLs plus the client id for a login attempt.
///
/// Grouping the endpoints as one value (rather than reading loose constants at
/// each call site) keeps a custom / staging deployment coherent: override the
/// client id or base once and the whole flow follows.
#[derive(Debug, Clone)]
pub struct OAuthEndpoints {
    /// OAuth public client id.
    pub client_id: String,
    /// Token endpoint (exchange + refresh).
    pub token_url: String,
    /// Browser authorize endpoint.
    pub authorize_url: String,
    /// Redirect URI registered for the flow.
    pub redirect_uri: String,
}

impl OAuthEndpoints {
    /// The production Claude.ai subscription configuration.
    pub fn prod() -> Self {
        Self {
            client_id: CLIENT_ID.to_owned(),
            token_url: TOKEN_URL.to_owned(),
            authorize_url: CLAUDE_AI_AUTHORIZE_URL.to_owned(),
            redirect_uri: MANUAL_REDIRECT_URL.to_owned(),
        }
    }

    /// Production configuration with the client id overridden from
    /// [`CLIENT_ID_ENV`] when that environment variable is set and non-empty.
    pub fn from_env() -> Self {
        let mut endpoints = Self::prod();
        if let Ok(id) = std::env::var(CLIENT_ID_ENV) {
            let id = id.trim();
            if !id.is_empty() {
                endpoints.client_id = id.to_owned();
            }
        }
        endpoints
    }
}

impl Default for OAuthEndpoints {
    fn default() -> Self {
        Self::prod()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_param_joins_in_order_with_spaces() {
        assert_eq!(
            scope_param(&AUTHORIZE_SCOPES),
            "org:create_api_key user:profile user:inference \
             user:sessions:claude_code user:mcp_servers user:file_upload"
        );
        assert_eq!(
            scope_param(&REFRESH_SCOPES),
            "user:profile user:inference user:sessions:claude_code \
             user:mcp_servers user:file_upload"
        );
    }

    #[test]
    fn grants_inference_detects_the_inference_scope() {
        assert!(grants_inference(&["user:profile", "user:inference"]));
        assert!(!grants_inference(&["user:profile", "user:mcp_servers"]));
    }
}
