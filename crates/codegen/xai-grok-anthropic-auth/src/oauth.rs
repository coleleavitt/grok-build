//! The OAuth authorization-code and refresh-token flows against the Anthropic
//! token endpoint: the wire request/response types, an authorize-URL builder,
//! the manual-paste redirect parser, and an async client.

use crate::endpoints::{self, OAuthEndpoints, Scope};
use crate::error::{AnthropicAuthError, Result};
use crate::pkce::{CODE_CHALLENGE_METHOD, PkcePair, PkceVerifier};
use crate::token::{self, AccessToken, OAuthTokens, RefreshToken, TokenAccount, TokenOrganization};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// OAuth error codes that make a `400` response permanent (no retry can help).
const KNOWN_PERMANENT_OAUTH_ERRORS: [&str; 7] = [
    "invalid_grant",
    "invalid_client",
    "invalid_request",
    "unauthorized_client",
    "access_denied",
    "unsupported_grant_type",
    "invalid_scope",
];

/// Upper bound on any server-advised retry cooldown: 24 hours in milliseconds.
const MAX_RETRY_AFTER_MS: i64 = 24 * 60 * 60 * 1000;

/// The POST body sent to the token endpoint. One enum, two grants — the
/// `grant_type` discriminant is serialized inline.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "grant_type", rename_all = "snake_case")]
pub enum TokenRequest {
    /// Exchange an authorization code for tokens.
    AuthorizationCode {
        /// The authorization code returned to the redirect.
        code: String,
        /// The redirect URI used in the authorize request.
        redirect_uri: String,
        /// OAuth client id.
        client_id: String,
        /// The PKCE verifier matching the challenge sent at authorize time.
        code_verifier: String,
        /// The anti-CSRF state echoed back.
        state: String,
    },
    /// Renew an access token from a refresh token.
    RefreshToken {
        /// The refresh token.
        refresh_token: String,
        /// OAuth client id.
        client_id: String,
        /// Requested scopes (refresh set; excludes `org:create_api_key`).
        scope: String,
    },
}

/// Raw token-endpoint response.
#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    /// The new access token.
    pub access_token: String,
    /// A rotated refresh token, when the server issues one.
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Lifetime of the access token in seconds.
    pub expires_in: i64,
    /// Space-separated granted scopes.
    #[serde(default)]
    pub scope: Option<String>,
    /// Token type (`Bearer`).
    #[serde(default)]
    pub token_type: Option<String>,
    /// Account descriptor.
    #[serde(default)]
    pub account: Option<TokenAccount>,
    /// Organization descriptor.
    #[serde(default)]
    pub organization: Option<TokenOrganization>,
}

impl TokenResponse {
    /// Fold a response into stored [`OAuthTokens`], computing the absolute
    /// expiry from `now` and carrying `prior_refresh` forward when the response
    /// omits a new refresh token (Claude Code CLI behavior). Validates token
    /// formats, a positive lifetime, and — when the server echoes scopes — that
    /// inference was granted.
    pub fn into_tokens(
        self,
        now: DateTime<Utc>,
        prior_refresh: Option<&RefreshToken>,
    ) -> Result<OAuthTokens> {
        if !token::is_valid_access_token(&self.access_token) {
            return Err(AnthropicAuthError::MalformedTokenResponse("access_token"));
        }
        let refresh = match self.refresh_token {
            Some(raw) if token::is_valid_refresh_token(&raw) => RefreshToken::new(raw),
            Some(_) => return Err(AnthropicAuthError::MalformedTokenResponse("refresh_token")),
            None => prior_refresh
                .cloned()
                .ok_or(AnthropicAuthError::MalformedTokenResponse("refresh_token"))?,
        };
        if self.expires_in <= 0 {
            return Err(AnthropicAuthError::MalformedTokenResponse("expires_in"));
        }
        let scopes: Vec<String> = self
            .scope
            .as_deref()
            .map(|s| s.split_whitespace().map(str::to_owned).collect())
            .unwrap_or_default();
        if !scopes.is_empty() && !endpoints::grants_inference(&scopes) {
            return Err(AnthropicAuthError::MalformedTokenResponse(
                "granted scopes lack user:inference",
            ));
        }
        Ok(OAuthTokens {
            access: AccessToken::new(self.access_token),
            refresh,
            expires_at: now + Duration::seconds(self.expires_in),
            scopes,
            account: self.account,
            organization: self.organization,
        })
    }
}

/// Builds the browser authorization URL for a login attempt.
#[derive(Debug)]
pub struct AuthorizeRequest<'a> {
    /// Endpoint set (authorize URL, client id, redirect).
    pub endpoints: &'a OAuthEndpoints,
    /// PKCE pair whose challenge is embedded in the URL.
    pub pkce: &'a PkcePair,
    /// Anti-CSRF state.
    pub state: &'a str,
    /// Scopes to request.
    pub scopes: &'a [Scope],
}

impl AuthorizeRequest<'_> {
    /// Render the full authorization URL.
    pub fn to_url(&self) -> Result<url::Url> {
        let mut url = url::Url::parse(&self.endpoints.authorize_url)?;
        url.query_pairs_mut()
            .append_pair("code", "true")
            .append_pair("client_id", &self.endpoints.client_id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", &self.endpoints.redirect_uri)
            .append_pair("scope", &endpoints::scope_param(self.scopes))
            .append_pair("code_challenge", &self.pkce.challenge)
            .append_pair("code_challenge_method", CODE_CHALLENGE_METHOD)
            .append_pair("state", self.state);
        Ok(url)
    }
}

/// Split a manual-paste redirect value (`code#state`) and CSRF-verify the
/// returned state against the locally generated one (constant-time). Returns
/// just the authorization code.
pub fn parse_redirect_code(pasted: &str, expected_state: &str) -> Result<String> {
    let (code, state) = pasted
        .trim()
        .rsplit_once('#')
        .ok_or(AnthropicAuthError::InvalidRedirect)?;
    if !constant_time_eq(state.as_bytes(), expected_state.as_bytes()) {
        return Err(AnthropicAuthError::StateMismatch);
    }
    Ok(code.to_owned())
}

/// Constant-time byte comparison — avoids leaking the state via timing.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Async client for the Anthropic OAuth token endpoint.
#[derive(Clone)]
pub struct OAuthClient {
    http: reqwest::Client,
    endpoints: OAuthEndpoints,
}

impl OAuthClient {
    /// Client with a fresh default [`reqwest::Client`].
    pub fn new(endpoints: OAuthEndpoints) -> Self {
        Self {
            http: reqwest::Client::new(),
            endpoints,
        }
    }

    /// Client reusing a caller-provided [`reqwest::Client`] (shared pool).
    pub fn with_http(http: reqwest::Client, endpoints: OAuthEndpoints) -> Self {
        Self { http, endpoints }
    }

    /// The endpoint set this client targets.
    pub fn endpoints(&self) -> &OAuthEndpoints {
        &self.endpoints
    }

    /// Exchange an authorization code (already CSRF-checked via
    /// [`parse_redirect_code`]) for a token set.
    pub async fn exchange_code(
        &self,
        code: &str,
        verifier: &PkceVerifier,
        state: &str,
    ) -> Result<OAuthTokens> {
        let body = TokenRequest::AuthorizationCode {
            code: code.to_owned(),
            redirect_uri: self.endpoints.redirect_uri.clone(),
            client_id: self.endpoints.client_id.clone(),
            code_verifier: verifier.expose().to_owned(),
            state: state.to_owned(),
        };
        self.post_token(body, None).await
    }

    /// Renew an access token from its refresh token, carrying the old refresh
    /// token forward if the server does not rotate it.
    pub async fn refresh(&self, refresh: &RefreshToken) -> Result<OAuthTokens> {
        let body = TokenRequest::RefreshToken {
            refresh_token: refresh.expose().to_owned(),
            client_id: self.endpoints.client_id.clone(),
            scope: endpoints::scope_param(&endpoints::REFRESH_SCOPES),
        };
        self.post_token(body, Some(refresh)).await
    }

    async fn post_token(
        &self,
        body: TokenRequest,
        prior_refresh: Option<&RefreshToken>,
    ) -> Result<OAuthTokens> {
        let resp = self
            .http
            .post(&self.endpoints.token_url)
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        if status.is_success() {
            let token: TokenResponse = resp.json().await?;
            return token.into_tokens(Utc::now(), prior_refresh);
        }
        let retry_after_ms = parse_retry_after_ms(resp.headers());
        let raw = resp.text().await.unwrap_or_default();
        let oauth_error = parse_oauth_error(&raw);
        let code = status.as_u16();
        let permanent = code == 401
            || code == 403
            || (code == 400
                && oauth_error
                    .as_deref()
                    .is_some_and(|e| KNOWN_PERMANENT_OAUTH_ERRORS.contains(&e)));
        Err(AnthropicAuthError::Endpoint {
            status: code,
            permanent,
            oauth_error,
            retry_after_ms,
            body: redact_tokens(&raw),
        })
    }
}

/// Extract the OAuth `error` code from a token-endpoint error body, accepting
/// both `{"error":"invalid_grant"}` and `{"error":{"type":"invalid_grant"}}`.
fn parse_oauth_error(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let error = value.get("error")?;
    if let Some(code) = error.as_str() {
        return Some(code.to_owned());
    }
    error
        .get("type")
        .and_then(|t| t.as_str())
        .map(str::to_owned)
}

/// Parse a retry cooldown (ms) from `retry-after-ms` (preferred) or
/// `retry-after` (seconds), clamped to [`MAX_RETRY_AFTER_MS`].
fn parse_retry_after_ms(headers: &reqwest::header::HeaderMap) -> Option<i64> {
    if let Some(ms) = headers
        .get("retry-after-ms")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<i64>().ok())
    {
        return Some(ms.clamp(0, MAX_RETRY_AFTER_MS));
    }
    let secs = headers
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<i64>().ok())?;
    Some((secs.saturating_mul(1000)).clamp(0, MAX_RETRY_AFTER_MS))
}

/// Replace any `sk-ant-…` token run with a placeholder so error bodies are safe
/// to log. Char-boundary safe.
fn redact_tokens(input: &str) -> String {
    const MARKER: &str = "sk-ant-";
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(pos) = rest.find(MARKER) {
        out.push_str(&rest[..pos]);
        out.push_str("sk-ant-***REDACTED***");
        let after = &rest[pos + MARKER.len()..];
        let end = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
            .unwrap_or(after.len());
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoints::AUTHORIZE_SCOPES;

    fn now() -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.timestamp_opt(1_700_000_000, 0).unwrap()
    }

    const VALID_ACCESS: &str = "sk-ant-oat01-abcdefghijklmnopqrstuvwxyz012345";
    const VALID_REFRESH: &str = "sk-ant-ort01-abcdefghijklmnopqrstuvwxyz012345";
    const NEW_REFRESH: &str = "sk-ant-ort01-ZZZZZZZZZZZZZZZZZZZZ99999";

    #[test]
    fn token_request_serializes_grant_type_inline() {
        let req = TokenRequest::RefreshToken {
            refresh_token: "r".into(),
            client_id: "c".into(),
            scope: "user:inference".into(),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["grant_type"], "refresh_token");
        assert_eq!(json["refresh_token"], "r");
    }

    #[test]
    fn into_tokens_keeps_prior_refresh_when_absent() {
        let prior = RefreshToken::new(VALID_REFRESH);
        let resp = TokenResponse {
            access_token: VALID_ACCESS.into(),
            refresh_token: None,
            expires_in: 3600,
            scope: Some("user:profile user:inference".into()),
            token_type: Some("Bearer".into()),
            account: None,
            organization: None,
        };
        let tokens = resp.into_tokens(now(), Some(&prior)).unwrap();
        assert_eq!(tokens.refresh.expose(), VALID_REFRESH);
        assert_eq!(tokens.expires_at, now() + Duration::seconds(3600));
    }

    #[test]
    fn into_tokens_takes_rotated_refresh_when_present() {
        let prior = RefreshToken::new(VALID_REFRESH);
        let resp = TokenResponse {
            access_token: VALID_ACCESS.into(),
            refresh_token: Some(NEW_REFRESH.into()),
            expires_in: 3600,
            scope: None,
            token_type: None,
            account: None,
            organization: None,
        };
        let tokens = resp.into_tokens(now(), Some(&prior)).unwrap();
        assert_eq!(tokens.refresh.expose(), NEW_REFRESH);
    }

    #[test]
    fn into_tokens_rejects_scopes_without_inference() {
        let resp = TokenResponse {
            access_token: VALID_ACCESS.into(),
            refresh_token: Some(VALID_REFRESH.into()),
            expires_in: 3600,
            scope: Some("user:profile user:mcp_servers".into()),
            token_type: None,
            account: None,
            organization: None,
        };
        let err = resp.into_tokens(now(), None).unwrap_err();
        assert!(matches!(err, AnthropicAuthError::MalformedTokenResponse(_)));
    }

    #[test]
    fn authorize_url_carries_pkce_and_scopes() {
        let endpoints = OAuthEndpoints::prod();
        let pkce = PkcePair::from_verifier(PkceVerifier::new(
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
        ));
        let req = AuthorizeRequest {
            endpoints: &endpoints,
            pkce: &pkce,
            state: "the-state",
            scopes: &AUTHORIZE_SCOPES,
        };
        let url = req.to_url().unwrap();
        let pairs: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(pairs["client_id"], endpoints::CLIENT_ID);
        assert_eq!(pairs["response_type"], "code");
        assert_eq!(pairs["code_challenge_method"], "S256");
        assert_eq!(pairs["code_challenge"], pkce.challenge);
        assert_eq!(pairs["state"], "the-state");
        assert!(pairs["scope"].contains("user:inference"));
    }

    #[test]
    fn redirect_parse_splits_and_verifies_state() {
        assert_eq!(
            parse_redirect_code("thecode#thestate", "thestate").unwrap(),
            "thecode"
        );
        assert!(matches!(
            parse_redirect_code("thecode#wrong", "thestate"),
            Err(AnthropicAuthError::StateMismatch)
        ));
        assert!(matches!(
            parse_redirect_code("nostate", "thestate"),
            Err(AnthropicAuthError::InvalidRedirect)
        ));
    }

    #[test]
    fn oauth_error_parses_both_shapes() {
        assert_eq!(
            parse_oauth_error(r#"{"error":"invalid_grant"}"#).as_deref(),
            Some("invalid_grant")
        );
        assert_eq!(
            parse_oauth_error(r#"{"error":{"type":"invalid_client"}}"#).as_deref(),
            Some("invalid_client")
        );
        assert_eq!(parse_oauth_error("not json"), None);
    }

    #[test]
    fn retry_after_prefers_ms_then_seconds() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("retry-after", "30".parse().unwrap());
        assert_eq!(parse_retry_after_ms(&headers), Some(30_000));
        headers.insert("retry-after-ms", "1500".parse().unwrap());
        assert_eq!(parse_retry_after_ms(&headers), Some(1_500));
    }

    #[test]
    fn redact_tokens_hides_secrets_but_keeps_context() {
        let msg = "grant failed for sk-ant-ort01-supersecretvalue123 at endpoint";
        let redacted = redact_tokens(msg);
        assert!(!redacted.contains("supersecret"));
        assert!(redacted.contains("sk-ant-***REDACTED***"));
        assert!(redacted.contains("at endpoint"));
    }
}
