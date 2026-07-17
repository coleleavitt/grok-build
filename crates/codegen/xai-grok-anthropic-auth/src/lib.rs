//! Anthropic OAuth authentication for Grok model providers.
//!
//! A transport-agnostic domain library. Anthropic credentials are modeled as
//! types — secret newtypes ([`AccessToken`], [`RefreshToken`], [`ApiKey`]), a
//! [`Credential`] kind, and an [`OAuthTokens`] session with expiry logic — the
//! PKCE authorization-code and refresh-token flows run against the Anthropic
//! token endpoint ([`OAuthClient`]), and multiple subscription accounts are
//! stored and rotated on disk ([`store`]). Whatever plugin transport eventually
//! drives it — subprocess, WASM, or a static build-in — depends on these types,
//! not on a bespoke pile of functions.
//!
//! Ported from the `opencode-anthropic-auth` plugin and the Claude Code CLI
//! OAuth configuration (v2.1.207). This crate implements credential management
//! and the reusable Anthropic OAuth headers; the sampler-side Anthropic adapter
//! adds Claude Code-style request identity, billing prompt, and CCH attestation.
#![forbid(unsafe_code)]

pub mod account;
pub mod endpoints;
pub mod error;
pub mod live;
pub mod manager;
pub mod oauth;
pub mod pkce;
pub mod request;
pub mod store;
pub mod token;

pub use account::{Account, AccountCapabilities, PlanType, RoutingStatus};
pub use endpoints::{OAuthEndpoints, Scope};
pub use error::{AnthropicAuthError, Result};
pub use live::LiveCredential;
pub use manager::AnthropicAuthManager;
pub use oauth::{AuthorizeRequest, OAuthClient, TokenRequest, TokenResponse, parse_redirect_code};
pub use pkce::{PkcePair, PkceVerifier};
pub use request::HeaderMutation;
pub use store::{AccountData, AccountStore};
pub use token::{AccessToken, ApiKey, AuthHeader, Credential, OAuthTokens, RefreshToken};
