//! Per-request auth/payload debug instrumentation for provider requests.
//!
//! Answers "which exact request was this?" during auth incidents: every
//! Messages-API attempt gets a structured line with a **payload
//! fingerprint** (stable hash of the serialized body), size/shape stats, and
//! the redacted auth identity that signed it. A retry that resends the same
//! conversation carries the same fingerprint, so working requests and
//! rejected retries can be correlated 1:1 in the logs.
//!
//! Enablement:
//!
//! - Always on at `debug!` level under the `anthropic_auth` tracing target
//!   (`RUST_LOG=anthropic_auth=debug`), and mirrored as `linkscope` events
//!   when `GROK_AUTH_TRACE=1`.
//! - `GROK_AUTH_TRACE_BODY=1` additionally dumps each request body to
//!   `~/.grok/logs/anthropic-requests/<utc>-<req>.json` (bodies contain
//!   conversation content but no credentials — auth lives in headers, which
//!   are never dumped).

use sha2::{Digest, Sha256};

/// `tracing` target shared with `xai-grok-anthropic-auth` so one filter
/// (`anthropic_auth=debug`) captures the whole auth story.
pub const TRACE_TARGET: &str = "anthropic_auth";

/// True when `GROK_AUTH_TRACE` is truthy; first truthy read enables
/// `linkscope` tracing for the process.
pub fn trace_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        let on = env_truthy("GROK_AUTH_TRACE");
        if on {
            linkscope::trace_enable();
        }
        on
    })
}

fn env_truthy(key: &str) -> bool {
    std::env::var(key)
        .map(|v| {
            let v = v.trim();
            !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false")
        })
        .unwrap_or(false)
}

/// Shape/size summary of one serialized request body.
#[derive(Debug, Clone)]
pub struct PayloadStats {
    /// Stable fingerprint of the exact bytes: identical resubmits share it.
    pub fingerprint: String,
    pub bytes: usize,
    pub message_count: usize,
    pub tool_count: usize,
    pub system_bytes: usize,
}

/// Fingerprint + stats for a serialized Messages-API body.
pub fn payload_stats(body: &[u8]) -> PayloadStats {
    let fingerprint = format!("{:x}", Sha256::digest(body));

    let (mut message_count, mut tool_count, mut system_bytes) = (0, 0, 0);
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) {
        message_count = value
            .get("messages")
            .and_then(|m| m.as_array())
            .map_or(0, Vec::len);
        tool_count = value
            .get("tools")
            .and_then(|t| t.as_array())
            .map_or(0, Vec::len);
        system_bytes = value
            .get("system")
            .map(|s| s.to_string().len())
            .unwrap_or(0);
    }
    PayloadStats {
        fingerprint,
        bytes: body.len(),
        message_count,
        tool_count,
        system_bytes,
    }
}

/// Redacted auth identity of an outgoing request: scheme plus a short
/// credential prefix, read back from the final header map.
pub fn auth_identity(headers: &reqwest::header::HeaderMap) -> (&'static str, String) {
    if let Some(v) = headers
        .get(reqwest::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        let token = v.strip_prefix("Bearer ").unwrap_or(v);
        return ("bearer", prefix12(token));
    }
    if let Some(v) = headers.get("x-api-key").and_then(|v| v.to_str().ok()) {
        return ("x-api-key", prefix12(v));
    }
    ("none", String::new())
}

fn prefix12(secret: &str) -> String {
    secret.chars().take(12).collect()
}

/// Anthropic response headers that explain a rejection: `request-id`,
/// `retry-after`, and every `anthropic-ratelimit-*` bucket header.
pub fn rate_limit_headers(headers: &reqwest::header::HeaderMap) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (name, value) in headers {
        let name = name.as_str();
        if (name == "request-id"
            || name == "retry-after"
            || name.starts_with("anthropic-ratelimit-"))
            && let Ok(v) = value.to_str()
        {
            out.push((name.to_owned(), v.to_owned()));
        }
    }
    out.sort();
    out
}

/// Emit the standard "request sent" record for a Messages-API attempt.
pub fn log_request(
    context: &'static str,
    req_id: &str,
    model_id: &str,
    base_url: &str,
    headers: &reqwest::header::HeaderMap,
    body: &[u8],
) -> PayloadStats {
    let stats = payload_stats(body);
    let (auth_kind, auth_prefix) = auth_identity(headers);
    tracing::debug!(
        target: TRACE_TARGET,
        context,
        req_id,
        model = model_id,
        base_url,
        auth_kind,
        auth_prefix = %auth_prefix,
        payload_fingerprint = %stats.fingerprint,
        payload_bytes = stats.bytes,
        message_count = stats.message_count,
        tool_count = stats.tool_count,
        system_bytes = stats.system_bytes,
        "anthropic request sent"
    );
    if trace_enabled() {
        linkscope::event_fields(
            "anthropic.request",
            [
                linkscope::TraceField::text("context", context),
                linkscope::TraceField::text("req_id", req_id.to_owned()),
                linkscope::TraceField::text("auth", format!("{auth_kind}:{auth_prefix}")),
                linkscope::TraceField::text("fingerprint", stats.fingerprint.clone()),
                linkscope::TraceField::bytes("payload", stats.bytes as u64),
                linkscope::TraceField::count("messages", stats.message_count as u64),
            ],
        );
    }
    dump_body_if_enabled(context, req_id, &stats, body);
    stats
}

/// Emit the standard "request rejected" record, including the provider's
/// rate-limit bucket headers — the part a plain status line never shows.
pub fn log_rejection(
    context: &'static str,
    req_id: &str,
    model_id: &str,
    status: u16,
    stats: Option<&PayloadStats>,
    headers: &reqwest::header::HeaderMap,
) {
    let limits = rate_limit_headers(headers);
    tracing::warn!(
        target: TRACE_TARGET,
        context,
        req_id,
        model = model_id,
        status,
        payload_fingerprint = stats.map(|s| s.fingerprint.as_str()).unwrap_or(""),
        rate_limit_headers = ?limits,
        "anthropic request rejected"
    );
    if trace_enabled() {
        let mut fields = vec![
            linkscope::TraceField::text("context", context),
            linkscope::TraceField::text("req_id", req_id.to_owned()),
            linkscope::TraceField::count("status", u64::from(status)),
        ];
        if let Some(stats) = stats {
            fields.push(linkscope::TraceField::text(
                "fingerprint",
                stats.fingerprint.clone(),
            ));
        }
        if !limits.is_empty() {
            let joined = limits
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect::<Vec<_>>()
                .join(" ");
            fields.push(linkscope::TraceField::text("limits", joined));
        }
        linkscope::event_fields("anthropic.rejected", fields);
    }
}

/// Emit the standard "request accepted" record so successful Anthropic
/// attempts can be paired with retry/rejection attempts by fingerprint.
pub fn log_success(
    context: &'static str,
    req_id: &str,
    model_id: &str,
    status: u16,
    stats: Option<&PayloadStats>,
    headers: &reqwest::header::HeaderMap,
) {
    let limits = rate_limit_headers(headers);
    tracing::info!(
        target: TRACE_TARGET,
        context,
        req_id,
        model = model_id,
        status,
        payload_fingerprint = stats.map(|s| s.fingerprint.as_str()).unwrap_or(""),
        rate_limit_headers = ?limits,
        "anthropic request accepted"
    );
    if trace_enabled() {
        linkscope::event_fields(
            "anthropic.accepted",
            [
                linkscope::TraceField::text("context", context),
                linkscope::TraceField::text("req_id", req_id.to_owned()),
                linkscope::TraceField::count("status", u64::from(status)),
                linkscope::TraceField::text(
                    "fingerprint",
                    stats.map(|s| s.fingerprint.clone()).unwrap_or_default(),
                ),
            ],
        );
    }
}

/// When `GROK_AUTH_TRACE_BODY` is truthy, write the body to
/// `~/.grok/logs/anthropic-requests/`. Failures are swallowed — dumping is
/// best-effort diagnostics, never load-bearing.
fn dump_body_if_enabled(context: &str, req_id: &str, stats: &PayloadStats, body: &[u8]) {
    static DUMP: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    let dir = DUMP.get_or_init(|| {
        if !env_truthy("GROK_AUTH_TRACE_BODY") {
            return None;
        }
        let dir = std::env::var_os("HOME").map(|home| {
            std::path::Path::new(&home)
                .join(".grok")
                .join("logs")
                .join("anthropic-requests")
        })?;
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir)
    });
    let Some(dir) = dir else { return };
    let ts = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
    let safe_req: String = req_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(40)
        .collect();
    let path = dir.join(format!(
        "{ts}-{context}-{safe_req}-{fp}.json",
        fp = &stats.fingerprint[..8.min(stats.fingerprint.len())]
    ));
    let _ = std::fs::write(path, body);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_stats_fingerprint_is_stable_and_shape_aware() {
        let body = serde_json::json!({
            "model": "claude-x",
            "system": [{"type": "text", "text": "sys"}],
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "yo"},
            ],
            "tools": [{"name": "bash"}],
        })
        .to_string();
        let a = payload_stats(body.as_bytes());
        let b = payload_stats(body.as_bytes());
        assert_eq!(a.fingerprint, b.fingerprint, "same bytes, same fingerprint");
        assert_eq!(a.message_count, 2);
        assert_eq!(a.tool_count, 1);
        assert!(a.system_bytes > 0);
        assert_eq!(a.bytes, body.len());

        let other = payload_stats(br#"{"messages": []}"#);
        assert_ne!(a.fingerprint, other.fingerprint);
    }

    #[test]
    fn auth_identity_redacts_to_prefix() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            "Bearer sk-ant-oat01-supersecretsupersecret"
                .parse()
                .unwrap(),
        );
        let (kind, prefix) = auth_identity(&headers);
        assert_eq!(kind, "bearer");
        assert_eq!(prefix, "sk-ant-oat01");

        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-api-key", "sk-ant-api03-alsosecretalso".parse().unwrap());
        let (kind, prefix) = auth_identity(&headers);
        assert_eq!(kind, "x-api-key");
        assert_eq!(prefix, "sk-ant-api03");

        assert_eq!(auth_identity(&reqwest::header::HeaderMap::new()).0, "none");
    }

    #[test]
    fn rate_limit_headers_filter_and_sort() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("request-id", "req_123".parse().unwrap());
        headers.insert("retry-after", "120".parse().unwrap());
        headers.insert(
            "anthropic-ratelimit-unified-remaining",
            "0".parse().unwrap(),
        );
        headers.insert("content-type", "application/json".parse().unwrap());
        let out = rate_limit_headers(&headers);
        assert_eq!(
            out,
            vec![
                (
                    "anthropic-ratelimit-unified-remaining".to_owned(),
                    "0".to_owned()
                ),
                ("request-id".to_owned(), "req_123".to_owned()),
                ("retry-after".to_owned(), "120".to_owned()),
            ]
        );
    }
}
