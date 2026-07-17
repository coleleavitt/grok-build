//! Anthropic credential domain types: secret newtypes, the credential-kind
//! enum, the OAuth token set with expiry logic, and token-format validators.

use crate::endpoints;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Refresh proactively when the access token is within this window of expiry.
/// Matches the Claude Code CLI's 5-minute buffer.
pub const REFRESH_LEEWAY_SECS: i64 = 300;

/// Maximum accepted length of any `sk-ant-*` token.
pub const MAX_TOKEN_LEN: usize = 500;

const ACCESS_TOKEN_PREFIX: &str = "sk-ant-oat";
const REFRESH_TOKEN_PREFIX: &str = "sk-ant-ort";

/// A short-lived OAuth access token (`sk-ant-oat…`). Redacted from `Debug`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AccessToken(String);

/// A long-lived OAuth refresh token (`sk-ant-ort…`). Redacted from `Debug`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RefreshToken(String);

/// A static Anthropic API key (`sk-ant-api…`). Redacted from `Debug`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ApiKey(String);

macro_rules! secret_newtype {
    ($ty:ident) => {
        impl $ty {
            /// Wrap a raw secret string.
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// Read the raw secret. The explicit name keeps deliberate secret
            /// access grep-able across the codebase.
            pub fn expose(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Debug for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}(***)", stringify!($ty))
            }
        }
    };
}

// A single declarative macro for three byte-identical secret wrappers is
// clearer than three hand-copied impl blocks and keeps the redaction guarantee
// in one place. Expanded model: `new`, `expose`, and a redacting `Debug`.
secret_newtype!(AccessToken);
secret_newtype!(RefreshToken);
secret_newtype!(ApiKey);

/// Whether a string is a well-formed OAuth access token.
pub fn is_valid_access_token(s: &str) -> bool {
    is_valid_sk_ant_token(s, ACCESS_TOKEN_PREFIX)
}

/// Whether a string is a well-formed OAuth refresh token.
pub fn is_valid_refresh_token(s: &str) -> bool {
    is_valid_sk_ant_token(s, REFRESH_TOKEN_PREFIX)
}

/// Shared shape: `<prefix><version-digits>-<>=20 base64url-ish chars>`, capped
/// at [`MAX_TOKEN_LEN`]. Mirrors the CLI's `sk-ant-oat\d+-[A-Za-z0-9_-]{20,}`.
fn is_valid_sk_ant_token(s: &str, prefix: &str) -> bool {
    if s.len() > MAX_TOKEN_LEN {
        return false;
    }
    let Some(rest) = s.strip_prefix(prefix) else {
        return false;
    };
    let Some((version, body)) = rest.split_once('-') else {
        return false;
    };
    if version.is_empty() || !version.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    body.len() >= 20
        && body
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The account descriptor returned alongside a token grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenAccount {
    /// Account UUID.
    pub uuid: String,
    /// Account email, when the grant included it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email_address: Option<String>,
}

/// The organization descriptor returned alongside a token grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenOrganization {
    /// Organization UUID.
    pub uuid: String,
}

/// A complete OAuth session: the token pair, its absolute expiry, the granted
/// scopes, and the account/org it belongs to.
#[derive(Clone, Serialize, Deserialize)]
pub struct OAuthTokens {
    /// Current access token.
    pub access: AccessToken,
    /// Refresh token used to renew [`OAuthTokens::access`].
    pub refresh: RefreshToken,
    /// Absolute expiry, persisted as epoch milliseconds (CLI-compatible).
    #[serde(with = "chrono::serde::ts_milliseconds")]
    pub expires_at: DateTime<Utc>,
    /// Granted scopes (space-split from the token response).
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Account descriptor, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<TokenAccount>,
    /// Organization descriptor, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization: Option<TokenOrganization>,
}

impl OAuthTokens {
    /// Whether the access token is already past its expiry at `now`.
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires_at
    }

    /// Whether the access token is expired or within [`REFRESH_LEEWAY_SECS`] of
    /// expiry — i.e. it should be refreshed proactively.
    pub fn needs_refresh(&self, now: DateTime<Utc>) -> bool {
        now + Duration::seconds(REFRESH_LEEWAY_SECS) >= self.expires_at
    }

    /// Whether the granted scopes permit inference. A session without
    /// `user:inference` cannot be used to sample and should be treated as
    /// unusable.
    pub fn grants_inference(&self) -> bool {
        endpoints::grants_inference(&self.scopes)
    }
}

impl fmt::Debug for OAuthTokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthTokens")
            .field("access", &self.access)
            .field("refresh", &self.refresh)
            .field("expires_at", &self.expires_at)
            .field("scopes", &self.scopes)
            .field("account", &self.account)
            .field("organization", &self.organization)
            .finish()
    }
}

/// The kind of credential backing a request: an OAuth session or a static API
/// key. Serialized with an internal `type` tag, matching the on-disk shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Credential {
    /// A Claude.ai / Console subscription OAuth session.
    Oauth(OAuthTokens),
    /// A static `sk-ant-api…` key.
    ApiKey(ApiKey),
}

/// The HTTP header a credential contributes to an outgoing Anthropic request.
/// OAuth and API-key auth are mutually exclusive: an OAuth request carries a
/// bearer token (and the OAuth beta) and never an `x-api-key`, and vice versa.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthHeader {
    /// `Authorization: Bearer <access token>`, paired with the OAuth beta.
    Bearer(String),
    /// `x-api-key: <api key>`.
    ApiKey(String),
}

impl Credential {
    /// The auth header this credential contributes.
    pub fn auth_header(&self) -> AuthHeader {
        match self {
            Credential::Oauth(tokens) => AuthHeader::Bearer(tokens.access.expose().to_owned()),
            Credential::ApiKey(key) => AuthHeader::ApiKey(key.expose().to_owned()),
        }
    }

    /// Whether this credential authenticates via OAuth (and therefore needs the
    /// `anthropic-beta: oauth-2025-04-20` header).
    pub fn is_oauth(&self) -> bool {
        matches!(self, Credential::Oauth(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0).unwrap()
    }

    #[test]
    fn token_validators_accept_real_shapes_reject_junk() {
        assert!(is_valid_access_token(
            "sk-ant-oat01-abcdefghijklmnopqrstuvwxyz012345"
        ));
        assert!(is_valid_refresh_token(
            "sk-ant-ort01-abcdefghijklmnopqrstuvwxyz012345"
        ));
        // Wrong family.
        assert!(!is_valid_access_token(
            "sk-ant-ort01-abcdefghijklmnopqrstuvwxyz012345"
        ));
        // Body too short.
        assert!(!is_valid_access_token("sk-ant-oat01-tooshort"));
        // Missing version digits.
        assert!(!is_valid_access_token(
            "sk-ant-oat-abcdefghijklmnopqrstuvwxyz012345"
        ));
        // Plain API key is neither.
        assert!(!is_valid_access_token("sk-ant-api01-whatever"));
    }

    #[test]
    fn expiry_and_refresh_window() {
        let tokens = OAuthTokens {
            access: AccessToken::new("a"),
            refresh: RefreshToken::new("r"),
            expires_at: at(1_000),
            scopes: vec!["user:inference".into()],
            account: None,
            organization: None,
        };
        assert!(!tokens.is_expired(at(900)));
        assert!(tokens.is_expired(at(1_000)));
        // Inside the 5-minute (300s) leeway → needs refresh though not expired.
        assert!(!tokens.is_expired(at(800)));
        assert!(tokens.needs_refresh(at(800)));
        assert!(!tokens.needs_refresh(at(699)));
    }

    #[test]
    fn grants_inference_reflects_scopes() {
        let mut tokens = OAuthTokens {
            access: AccessToken::new("a"),
            refresh: RefreshToken::new("r"),
            expires_at: at(1_000),
            scopes: vec!["user:profile".into()],
            account: None,
            organization: None,
        };
        assert!(!tokens.grants_inference());
        tokens.scopes.push("user:inference".into());
        assert!(tokens.grants_inference());
    }

    #[test]
    fn credential_auth_header_is_mutually_exclusive() {
        let oauth = Credential::Oauth(OAuthTokens {
            access: AccessToken::new("secret-access"),
            refresh: RefreshToken::new("secret-refresh"),
            expires_at: at(1_000),
            scopes: vec![],
            account: None,
            organization: None,
        });
        assert_eq!(
            oauth.auth_header(),
            AuthHeader::Bearer("secret-access".into())
        );
        assert!(oauth.is_oauth());

        let key = Credential::ApiKey(ApiKey::new("sk-ant-api01-xyz"));
        assert_eq!(
            key.auth_header(),
            AuthHeader::ApiKey("sk-ant-api01-xyz".into())
        );
        assert!(!key.is_oauth());
    }

    #[test]
    fn debug_redacts_secrets() {
        let t = AccessToken::new("sk-ant-oat01-supersecret");
        assert_eq!(format!("{t:?}"), "AccessToken(***)");
        let r = RefreshToken::new("sk-ant-ort01-supersecret");
        assert_eq!(format!("{r:?}"), "RefreshToken(***)");
    }

    #[test]
    fn oauth_tokens_roundtrip_epoch_millis() {
        let tokens = OAuthTokens {
            access: AccessToken::new("a"),
            refresh: RefreshToken::new("r"),
            expires_at: at(1_700_000_000),
            scopes: vec!["user:inference".into()],
            account: Some(TokenAccount {
                uuid: "u".into(),
                email_address: Some("e@example.com".into()),
            }),
            organization: Some(TokenOrganization { uuid: "o".into() }),
        };
        let json = serde_json::to_string(&tokens).unwrap();
        assert!(
            json.contains("1700000000000"),
            "expires_at as epoch ms: {json}"
        );
        let back: OAuthTokens = serde_json::from_str(&json).unwrap();
        assert_eq!(back.expires_at, tokens.expires_at);
        assert_eq!(back.access.expose(), "a");
    }
}
