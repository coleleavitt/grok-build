//! Compatibility re-exports for the shared project-neutral Anthropic SDK.

pub use anthropic::endpoints::*;

/// Back-compat name used by Grok callers before the shared SDK renamed it.
pub type OAuthEndpoints = anthropic::Endpoints;

/// Historical helper endpoint. The shared SDK no longer needs this for refresh,
/// but keep the public constant for older Grok call sites/plugins.
pub const CREATE_API_KEY_URL: &str =
    "https://api.anthropic.com/api/oauth/claude_cli/create_api_key";
