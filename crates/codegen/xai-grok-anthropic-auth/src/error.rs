//! The crate error type.

use thiserror::Error;

/// Crate result alias.
pub type Result<T> = std::result::Result<T, AnthropicAuthError>;

/// Everything that can go wrong resolving, refreshing, or persisting an
/// Anthropic credential.
#[derive(Debug, Error)]
pub enum AnthropicAuthError {
    /// The token endpoint (or an OAuth helper endpoint) returned a non-success
    /// status. Use [`AnthropicAuthError::is_permanent`] to decide whether a
    /// retry could ever succeed.
    #[error("oauth endpoint returned HTTP {status}: {body}")]
    Endpoint {
        /// HTTP status code.
        status: u16,
        /// Whether the grant failure is unrecoverable (bad code, revoked
        /// refresh token) as opposed to transient (rate limit, 5xx).
        permanent: bool,
        /// Machine-readable OAuth error code, when the body carried one.
        oauth_error: Option<String>,
        /// Server-advised cooldown before retrying, parsed from a
        /// `retry-after` / `retry-after-ms` header (clamped to 24h).
        retry_after_ms: Option<i64>,
        /// Raw (token-redacted) response body, for diagnostics.
        body: String,
    },

    /// The returned `state` did not match the locally generated one — a
    /// possible CSRF / interception attempt. The exchange is aborted.
    #[error("oauth state mismatch (possible CSRF); refusing code exchange")]
    StateMismatch,

    /// The access token is expired and no refresh token is available to renew
    /// it; the user must re-authenticate.
    #[error("access token expired and no refresh token is available")]
    ExpiredNoRefresh,

    /// The token response was missing or malformed in a field required for a
    /// usable session (e.g. no `access_token`, or scopes lacking
    /// `user:inference`).
    #[error("token response invalid: {0}")]
    MalformedTokenResponse(&'static str),

    /// A manual-paste redirect value could not be parsed into `code#state`.
    #[error("could not parse authorization redirect value")]
    InvalidRedirect,

    /// Writing the credential store would have deleted the last account; the
    /// write was refused to avoid wiping credentials.
    #[error("refusing to persist a credential store with zero accounts")]
    WouldDeleteAllAccounts,

    /// The credential store path is a symlink; refused to read or write through
    /// it (a symlink-swap tampering guard).
    #[error("credential store path is a symlink; refusing to follow it")]
    StoreIsSymlink,

    /// Failed to acquire the advisory lock on the credential store in time.
    #[error("timed out acquiring credential store lock")]
    LockTimeout,

    /// An authorization URL or endpoint could not be parsed.
    #[error("invalid url: {0}")]
    Url(#[from] url::ParseError),

    /// Underlying HTTP transport failure (connection, TLS, timeout).
    #[error("http transport error: {0}")]
    Http(#[from] reqwest::Error),

    /// Credential store I/O failure.
    #[error("credential store i/o error: {0}")]
    Io(#[from] std::io::Error),

    /// Credential (de)serialization failure.
    #[error("credential serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

impl AnthropicAuthError {
    /// Whether this represents an unrecoverable failure. Callers use this to
    /// decide between disabling an account and scheduling a retry.
    pub fn is_permanent(&self) -> bool {
        match self {
            AnthropicAuthError::Endpoint { permanent, .. } => *permanent,
            AnthropicAuthError::StateMismatch
            | AnthropicAuthError::ExpiredNoRefresh
            | AnthropicAuthError::MalformedTokenResponse(_)
            | AnthropicAuthError::InvalidRedirect
            | AnthropicAuthError::WouldDeleteAllAccounts
            | AnthropicAuthError::StoreIsSymlink
            | AnthropicAuthError::Url(_) => true,
            AnthropicAuthError::LockTimeout
            | AnthropicAuthError::Http(_)
            | AnthropicAuthError::Io(_)
            | AnthropicAuthError::Serde(_) => false,
        }
    }
}
