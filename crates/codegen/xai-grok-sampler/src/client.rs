//! HTTP client for the xAI sampling APIs.
//!
//! Owns the `reqwest::Client`, default request headers, and per-method
//! defaults. Talks to three backend shapes:
//!
//! * Chat Completions (`/chat/completions`)
//! * Responses API (`/responses`)
//! * Anthropic Messages API (`/messages`)
//!
//! All trace-upload and URL-based header injection is intentionally
//! *not* here. The session is responsible for putting any per-request
//! headers (proxy auth, OTel context, etc.)
//! into [`SamplerConfig::extra_headers`] before constructing the client.

use std::collections::BTreeMap;
use std::process::Stdio;

use chrono::Utc;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use indexmap::IndexMap;
use reqwest::header::{
    ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, USER_AGENT,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use xai_grok_sampling_types::error::{
    parse_error_bytes, try_parse_stream_error, user_facing_api_error_message,
};
use xai_grok_sampling_types::{
    ChatCompletionChunk, ChatCompletionRequest, ChatCompletionResponse, ConversationRequest,
    ConversationResponse, CreateResponseWrapper, DOOM_LOOP_CHECK_HEADER, MessagesRequestWrapper,
    ResponseModelMetadata, Result, SamplingError, SentCredential, build_messages_request,
    is_check_event, messages, rs,
};

use crate::attribution::bearer_tail_fragment;
use crate::config::{AuthScheme, OriginClientInfo, SamplerConfig};
use xai_grok_sampling_types::{ProviderCommandAdapter, ProviderRequestAdapter};

// Re-export ApiBackend from the shared types crate for downstream callers.
pub use xai_grok_sampling_types::ApiBackend;

/// Process-level fallback for the `x-grok-client-identifier` header.
const DEFAULT_CLIENT_IDENTIFIER: &str = "grok-shell";

/// Product identifier baked into User-Agent strings.
const AGENT_PRODUCT: &str = "grok-shell";
const ANTHROPIC_DEFAULT_MAX_TOKENS: u32 = 128_000;
const CLAUDE_CODE_IDENTITY: &str = "You are Claude Code, Anthropic's official CLI for Claude.";
const CLAUDE_CCH_PLACEHOLDER: &str = "cch=00000";
const CLAUDE_FINGERPRINT_SALT: &str = "59cf53e54c78";
const CLAUDE_BETAS: &[&str] = &[
    "claude-code-20250219",
    "oauth-2025-04-20",
    "interleaved-thinking-2025-05-14",
    "prompt-caching-scope-2026-01-05",
    "extended-cache-ttl-2025-04-11",
    "output-128k-2025-02-19",
    "web-search-2025-03-05",
    "structured-outputs-2025-12-15",
    "advanced-tool-use-2025-11-20",
    "tool-search-tool-2025-10-19",
    "files-api-2025-04-14",
    "cache-diagnosis-2026-04-07",
    "effort-2025-11-24",
    "environments-2025-11-01",
    "context-1m-2025-08-07",
    "fast-mode-2026-02-01",
    "afk-mode-2026-01-31",
    "task-budgets-2026-03-13",
    "advisor-tool-2026-03-01",
];

/// Per-request `x-grok-*` headers. Optional fields are skipped when empty/`None`.
struct GrokRequestHeaders<'a> {
    conv_id: &'a str,
    req_id: &'a str,
    model_id: &'a str,
    session_id: &'a str,
    turn_idx: Option<&'a str>,
    agent_id: &'a str,
    deployment_id: Option<&'a str>,
    user_id: Option<&'a str>,
}

impl GrokRequestHeaders<'_> {
    fn apply(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let mut b = builder
            .header("x-grok-conv-id", self.conv_id)
            .header("x-grok-req-id", self.req_id)
            .header("x-grok-model-override", self.model_id)
            .header("x-grok-session-id", self.session_id)
            .header("x-grok-agent-id", self.agent_id);
        if let Some(idx) = self.turn_idx {
            b = b.header("x-grok-turn-idx", idx);
        }
        if let Some(id) = self.deployment_id.filter(|s| !s.is_empty()) {
            b = b.header("x-grok-deployment-id", id);
        }
        if let Some(id) = self.user_id.filter(|s| !s.is_empty()) {
            b = b.header("x-grok-user-id", id);
        }
        b
    }
}

/// Parse the `Retry-After` response header as delta-seconds.
/// Our inference backends only emit integer seconds (never HTTP-date),
/// so we only handle that form. HTTP-dates silently return `None` and
/// the caller falls back to exponential backoff.
/// Capped at 120s to prevent absurdly long sleeps from a misbehaving upstream.
/// Deserialize a Responses API SSE event, with a fallback for xAI-specific
/// tool types (e.g., `x_search`) that `async_openai` can't parse.
///
/// The API echoes the request's `tools` array in `ResponseCompleted` and
/// `ResponseCreated` events. If we sent `{"type": "x_search"}`, the response
/// includes it, and `rs::Tool` deserialization fails. On failure, we strip
/// unrecognized tools from the raw JSON and retry.
///
/// On `response.completed` / `response.incomplete`, this also rewrites
/// `response.usage.total_tokens` in place to the live context length
/// (`context_details.input_tokens + context_details.output_tokens`)
/// when the API emits the xAI-specific `context_details` field.
/// Async-openai's typed `ResponseUsage` doesn't model `context_details`,
/// so we peek the raw JSON for it. The cumulative `input_tokens` /
/// `output_tokens` / `cached_tokens` continue to flow from the typed
/// `ResponseUsage` unchanged so billing telemetry stays correct. When
/// the API doesn't emit `context_details` (older deployments) `total_tokens`
/// passes through unchanged.
fn deserialize_response_event(data: &str) -> Result<rs::ResponseStreamEvent> {
    let mut event = match serde_json::from_str::<rs::ResponseStreamEvent>(data) {
        Ok(event) => event,
        Err(first_err) => {
            // Try sanitizing: parse as Value, strip unknown tools, retry.
            if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(data) {
                // Strip tools that async_openai's rs::Tool can't deserialize
                // (e.g., xAI-specific "x_search"). Instead of maintaining a
                // hardcoded allowlist, try deserializing each tool entry —
                // if it fails, drop it.
                if let Some(tools) = value
                    .pointer_mut("/response/tools")
                    .and_then(|v| v.as_array_mut())
                {
                    tools.retain(|t| serde_json::from_value::<rs::Tool>(t.clone()).is_ok());
                }
                if let Ok(mut event) = serde_json::from_value::<rs::ResponseStreamEvent>(value) {
                    apply_terminal_event_overrides(&mut event, data);
                    return Ok(event);
                }
            }
            tracing::error!(
                error = %first_err,
                raw_data = %data,
                "Failed to deserialize ResponseStreamEvent from stream"
            );
            return Err(SamplingError::Serialization(first_err));
        }
    };
    apply_terminal_event_overrides(&mut event, data);
    Ok(event)
}

/// On terminal Responses API events (`response.completed` /
/// `response.incomplete`), rewrite `response.usage.total_tokens` to the
/// live context length when the wire includes
/// `response.usage.context_details.{input_tokens, output_tokens}`.
///
/// `total_tokens` drives the CLI's `/context` bar, the auto-compact
/// threshold, and `meta.totalTokens` on persisted sessions. Under
/// server-side multi-turn loops (e.g. `web_search`, `x_search`) the
/// wire's cumulative total inflates as the loop runs; `context_details`
/// reports the final turn's prompt + output tokens — the real live
/// context the model is sitting in. Billing fields
/// (`input_tokens`, `output_tokens`, `input_tokens_details.cached_tokens`,
/// `output_tokens_details.reasoning_tokens`) stay on the cumulative
/// wire values so telemetry is unaffected.
///
/// No-op when:
/// - the event is not terminal,
/// - `response.usage` is `None`,
/// - `context_details` is absent (older backends / non-loop responses),
/// - or either of `context_details.{input_tokens, output_tokens}` is
///   missing — we don't guess the missing half.
fn apply_terminal_event_overrides(event: &mut rs::ResponseStreamEvent, data: &str) {
    let response = match event {
        rs::ResponseStreamEvent::ResponseCompleted(e) => &mut e.response,
        rs::ResponseStreamEvent::ResponseIncomplete(e) => &mut e.response,
        _ => return,
    };
    // Re-parse for fields async_openai's types omit (context total, cost ticks).
    let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
        return;
    };
    // Stash cost ticks in metadata for stream_responses.
    if let Some(ticks) = xai_grok_sampling_types::reported_cost_ticks(
        value
            .pointer("/response/usage/cost_in_usd_ticks")
            .and_then(|v| v.as_i64()),
    ) {
        response
            .metadata
            .get_or_insert_with(Default::default)
            .insert(COST_USD_TICKS_METADATA_KEY.to_owned(), ticks.to_string());
    }
    let Some(usage) = response.usage.as_mut() else {
        return;
    };
    let Some(total) = extract_context_total(&value) else {
        return;
    };
    usage.total_tokens = total;
}

/// Metadata key for cost ticks past typed Response events.
pub(crate) const COST_USD_TICKS_METADATA_KEY: &str = "xai.cost_usd_ticks";

/// Read `response.usage.context_details.{input_tokens, output_tokens}`
/// from the parsed terminal-event JSON and return their sum. Returns `None`
/// if either field is missing or out of `u32` range.
fn extract_context_total(value: &serde_json::Value) -> Option<u32> {
    let cd = value.pointer("/response/usage/context_details")?;
    let i = u32::try_from(cd.get("input_tokens")?.as_u64()?).ok()?;
    let o = u32::try_from(cd.get("output_tokens")?.as_u64()?).ok()?;
    Some(i.saturating_add(o))
}

const RESPONSES_KEEPALIVE_EVENT_TYPE: &str = "keepalive";

/// Returns true for liveness-only Responses API frames that should never reach
/// async-openai's closed `ResponseStreamEvent` enum.
fn is_responses_keepalive_event(event_name: &str, data: &str) -> bool {
    if event_name == RESPONSES_KEEPALIVE_EVENT_TYPE {
        return true;
    }

    data.contains(RESPONSES_KEEPALIVE_EVENT_TYPE)
        && serde_json::from_str::<serde_json::Value>(data).is_ok_and(|value| {
            value.get("type").and_then(|kind| kind.as_str()) == Some(RESPONSES_KEEPALIVE_EVENT_TYPE)
        })
}

/// Record `success=false` + `error` on the active inference span when a stream
/// request fails before any response (transport/connect/TLS errors). Without
/// this the `#[instrument]` span closes with both fields Empty, so an outage
/// shows zero `success=false` and error-rate alerts never fire.
fn record_stream_request_failure(err: &reqwest::Error) {
    let span = tracing::Span::current();
    span.record("success", false);
    span.record("error", err.to_string().as_str());
}

fn extract_retry_after(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get("retry-after-ms")
        .and_then(|v| v.to_str().ok())
        .and_then(parse_retry_after_ms)
        .or_else(|| {
            headers
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_retry_after_header)
        })
        .map(|s| s.min(120))
}

fn parse_retry_after_ms(raw: &str) -> Option<u64> {
    let ms = raw.trim().parse::<u64>().ok()?;
    Some(ms.saturating_add(999) / 1000)
}

fn parse_retry_after_header(raw: &str) -> Option<u64> {
    let trimmed = raw.trim();
    if let Ok(seconds) = trimmed.parse::<u64>() {
        return Some(seconds);
    }
    chrono::DateTime::parse_from_rfc2822(trimmed)
        .ok()
        .map(|dt| {
            dt.with_timezone(&Utc)
                .signed_duration_since(Utc::now())
                .num_seconds()
                .max(0) as u64
        })
}

fn extract_should_retry(headers: &reqwest::header::HeaderMap) -> Option<bool> {
    headers
        .get("x-should-retry")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            if s.eq_ignore_ascii_case("true") {
                Some(true)
            } else if s.eq_ignore_ascii_case("false") {
                Some(false)
            } else {
                None // unknown value — treat as absent
            }
        })
}

/// A request field the provider rejected, removed from the body so the
/// request can be retried once without it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StrippedRequestField {
    /// Dotted path of the containing object (empty for the request root).
    pub path: String,
    /// The removed key.
    pub key: String,
}

impl std::fmt::Display for StrippedRequestField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.path.is_empty() {
            write!(f, "{}", self.key)
        } else {
            write!(f, "{}.{}", self.path, self.key)
        }
    }
}

/// Extract the identifier a provider named in a 400 as unsupported.
///
/// Anthropic reports two shapes we can act on:
/// - `` `temperature` is deprecated for this model. `` — a root parameter.
/// - `output_config.format.schema: For 'array' type, property 'maxItems' is
///   not supported` — a keyword somewhere under a dotted path.
///
/// The named field is the last quoted identifier before the complaint, so
/// `For 'array' type, property 'maxItems' is not supported` yields `maxItems`,
/// not the `'array'` type name it mentions in passing.
fn parse_rejected_request_field(server_message: &str) -> Option<StrippedRequestField> {
    let lower = server_message.to_ascii_lowercase();
    let complaint = ["is not supported", "unsupported", "deprecated"]
        .iter()
        .filter_map(|marker| lower.find(marker))
        .min()?;

    // Path-scoped keyword: `<dotted.path>: ... property 'name' is not supported`.
    if let Some(key) = rejected_identifier(server_message, '\'', complaint) {
        return Some(StrippedRequestField {
            path: rejected_field_path(server_message, complaint),
            key,
        });
    }

    // Root parameter: `` `temperature` is deprecated for this model. ``
    let key = rejected_identifier(server_message, '`', complaint)?;
    Some(StrippedRequestField {
        path: String::new(),
        key,
    })
}

/// Last `delim`-quoted JSON-key-shaped identifier that closes before `limit`.
fn rejected_identifier(text: &str, delim: char, limit: usize) -> Option<String> {
    let mut found = None;
    let mut rest = text;
    let mut offset = 0usize;
    while let Some(open) = rest.find(delim) {
        let value_start = open + delim.len_utf8();
        let Some(close) = rest[value_start..].find(delim) else {
            break;
        };
        let candidate = &rest[value_start..value_start + close];
        let candidate_end = offset + value_start + close;
        if candidate_end > limit {
            break;
        }
        if !candidate.is_empty()
            && candidate
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        {
            found = Some(candidate.to_owned());
        }
        let consumed = value_start + close + delim.len_utf8();
        offset += consumed;
        rest = &rest[consumed..];
    }
    found
}

/// Dotted request path a provider named, e.g. `output_config.format.schema` in
/// `invalid_request_error: output_config.format.schema: For 'array' type, ...`.
///
/// Only tokens that actually look like a nested path (they contain a `.`)
/// qualify, so the leading `invalid_request_error:` error class and a trailing
/// `this model.` are both ignored. An empty path means the request root.
fn rejected_field_path(text: &str, limit: usize) -> String {
    text.get(..limit)
        .unwrap_or(text)
        .split_whitespace()
        .map(|token| token.trim_matches(|ch| matches!(ch, ':' | ',' | '.' | '`' | '\'')))
        .filter(|token| {
            token.contains('.')
                && token
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '.')
        })
        .next_back()
        .unwrap_or_default()
        .to_owned()
}

/// Remove a provider-rejected field from a serialized request body.
///
/// Returns the removed field when the body actually changed, so callers only
/// retry when the retry would differ. A root field is removed from the top
/// level; a path-scoped keyword is removed from every object under that path
/// (JSON Schema nests the same keyword at any depth).
fn strip_rejected_request_field(
    body: &mut serde_json::Value,
    field: &StrippedRequestField,
) -> bool {
    if field.path.is_empty() {
        return body
            .as_object_mut()
            .is_some_and(|map| map.remove(&field.key).is_some());
    }
    let mut node = body;
    for segment in field.path.split('.') {
        let Some(next) = node.get_mut(segment) else {
            return false;
        };
        node = next;
    }
    remove_key_recursively(node, &field.key)
}

fn remove_key_recursively(node: &mut serde_json::Value, key: &str) -> bool {
    match node {
        serde_json::Value::Object(map) => {
            let mut removed = map.remove(key).is_some();
            for value in map.values_mut() {
                removed |= remove_key_recursively(value, key);
            }
            removed
        }
        serde_json::Value::Array(items) => {
            let mut removed = false;
            for item in items {
                removed |= remove_key_recursively(item, key);
            }
            removed
        }
        _ => false,
    }
}

/// Fields a provider has already rejected, keyed by model id.
///
/// A rejection is a stable property of the model ("`temperature` is deprecated
/// for this model"), so remembering it turns the fix into a single wasted
/// round-trip per model per process instead of one on every request. Clients
/// are constructed per request in the shell, so this outlives them.
static REJECTED_REQUEST_FIELDS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, Vec<StrippedRequestField>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

fn remembered_rejected_fields(model_id: &str) -> Vec<StrippedRequestField> {
    REJECTED_REQUEST_FIELDS
        .lock()
        .map(|memo| memo.get(model_id).cloned().unwrap_or_default())
        .unwrap_or_default()
}

fn remember_rejected_field(model_id: &str, field: &StrippedRequestField) {
    if let Ok(mut memo) = REJECTED_REQUEST_FIELDS.lock() {
        let fields = memo.entry(model_id.to_owned()).or_default();
        if !fields.contains(field) {
            fields.push(field.clone());
        }
    }
}

fn encode_provider_tool_name(name: &str, prefix: &str) -> String {
    if name.starts_with(prefix) {
        return name.to_owned();
    }
    if let Some((server, tool)) = name.split_once(':') {
        format!("{prefix}{server}__{tool}")
    } else {
        format!("{prefix}grok__{name}")
    }
}

fn decode_provider_tool_name(name: &str, prefix: &str) -> String {
    let Some(rest) = name.strip_prefix(prefix) else {
        return name.to_owned();
    };
    let Some((server, tool)) = rest.split_once("__") else {
        return name.to_owned();
    };
    if server == "grok" {
        tool.to_owned()
    } else {
        format!("{server}:{tool}")
    }
}

fn strip_provider_tool_prefix_json(data: &str, prefix: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(data) else {
        return data.to_owned();
    };
    strip_provider_tool_prefix_value(&mut value, prefix);
    serde_json::to_string(&value).unwrap_or_else(|_| data.to_owned())
}

fn adapt_messages_event_json(adapter: Option<&ProviderRequestAdapter>, data: &str) -> String {
    let Some(ProviderRequestAdapter::Anthropic {
        tool_name_prefix, ..
    }) = adapter
    else {
        return data.to_owned();
    };
    strip_provider_tool_prefix_json(data, tool_name_prefix)
}

fn is_anthropic_adapter(adapter: Option<&ProviderRequestAdapter>) -> bool {
    matches!(adapter, Some(ProviderRequestAdapter::Anthropic { .. }))
}

fn sanitize_header_value(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !matches!(*ch, '\u{0}'..='\u{1f}' | '\u{7f}'))
        .take(8192)
        .collect()
}

fn claude_cli_version() -> String {
    std::env::var("CLAUDE_CODE_VERSION")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "2.1.212".to_owned())
}

fn first_user_text(request: &messages::MessagesRequest) -> String {
    request
        .messages
        .iter()
        .find(|message| matches!(&message.role, messages::MessageRole::User))
        .map(|message| match &message.content {
            messages::MessageContent::Text(text) => text.clone(),
            messages::MessageContent::Blocks(blocks) => blocks
                .iter()
                .find_map(|block| match block {
                    messages::ContentBlock::Text { text, .. } => Some(text.clone()),
                    _ => None,
                })
                .unwrap_or_default(),
        })
        .unwrap_or_default()
}

fn billing_hash(first_user_message: &str, version: &str) -> String {
    let chars: String = [4, 7, 20]
        .into_iter()
        .filter_map(|idx| first_user_message.chars().nth(idx))
        .collect();
    let digest = Sha256::digest(format!("{CLAUDE_FINGERPRINT_SALT}{chars}{version}").as_bytes());
    format!("{digest:x}")[..3].to_owned()
}

fn billing_header_text(request: &messages::MessagesRequest) -> String {
    let version = claude_cli_version();
    let hash = billing_hash(&first_user_text(request), &version);
    format!(
        "x-anthropic-billing-header: cc_version={version}.{hash}; cc_entrypoint=cli; cch=00000;"
    )
}

fn has_system_text(system: &Option<messages::SystemParam>, needle: &str) -> bool {
    match system {
        Some(messages::SystemParam::Text(text)) => text.contains(needle),
        Some(messages::SystemParam::Blocks(blocks)) => {
            blocks.iter().any(|block| block.text.contains(needle))
        }
        None => false,
    }
}

fn text_block(text: impl Into<String>) -> messages::TextBlock {
    messages::TextBlock {
        r#type: "text".to_owned(),
        text: text.into(),
        cache_control: None,
    }
}

fn ensure_claude_code_system_blocks(request: &mut messages::MessagesRequest) {
    let needs_billing = !has_system_text(&request.system, "x-anthropic-billing-header:");
    let needs_identity = !has_system_text(&request.system, CLAUDE_CODE_IDENTITY);
    if !needs_billing && !needs_identity {
        return;
    }

    let mut blocks = match request.system.take() {
        Some(messages::SystemParam::Blocks(blocks)) => blocks,
        Some(messages::SystemParam::Text(text)) => vec![text_block(text)],
        None => Vec::new(),
    };
    if needs_identity {
        blocks.insert(0, text_block(CLAUDE_CODE_IDENTITY));
    }
    if needs_billing {
        blocks.insert(0, text_block(billing_header_text(request)));
    }
    request.system = Some(messages::SystemParam::Blocks(blocks));
}

/// Strip `thinking` blocks the real Anthropic API would reject.
///
/// The Messages request builder replays every stored reasoning sibling as a
/// `thinking` content block. That is correct for the xAI Messages backend
/// (which wants `tco_*` blobs and prior-turn reasoning back verbatim for
/// prefix-cache continuity), but `api.anthropic.com` cryptographically
/// validates every replayed block and fails the whole request with
/// `messages.N.content.M: Invalid \`signature\` in \`thinking\` block` when
/// any block does not verify — e.g. reasoning that crossed a provider or
/// account switch, was mutated by compaction, or was collapsed from
/// interleaved thinking.
///
/// Per the extended-thinking contract, thinking blocks from prior turns may
/// always be omitted (the server strips them from context anyway); only the
/// final assistant message of an in-flight tool-use loop must retain its
/// thinking. So:
///
/// - thinking disabled for this request → drop every thinking block
///   (the API 400s on thinking blocks without a `thinking` config);
/// - assistant messages before the final one → drop thinking blocks;
/// - final assistant message without `tool_use` → drop thinking blocks
///   (not required for continuation, so sending them is pure risk);
/// - final assistant message with `tool_use` → keep only blocks carrying
///   both thinking text and a signature (unsigned text or signature-only
///   blobs can never validate);
/// - messages left with no content (thinking-only turns) are removed
///   entirely, because the API rejects empty content arrays.
fn sanitize_thinking_blocks(request: &mut messages::MessagesRequest) {
    let thinking_enabled = matches!(
        request.thinking,
        Some(messages::ThinkingConfig::Enabled { .. } | messages::ThinkingConfig::Adaptive { .. })
    );
    let last_assistant = request
        .messages
        .iter()
        .rposition(|message| matches!(message.role, messages::MessageRole::Assistant));

    let mut original_index = 0usize;
    request.messages.retain_mut(|message| {
        let index = original_index;
        original_index += 1;
        if !matches!(message.role, messages::MessageRole::Assistant) {
            return true;
        }
        let messages::MessageContent::Blocks(blocks) = &mut message.content else {
            // Plain-text assistant content cannot contain thinking blocks.
            return true;
        };
        if blocks.is_empty() {
            return true;
        }
        let has_tool_use = blocks
            .iter()
            .any(|block| matches!(block, messages::ContentBlock::ToolUse { .. }));
        let keep_thinking = thinking_enabled && Some(index) == last_assistant && has_tool_use;
        blocks.retain(|block| match block {
            messages::ContentBlock::Thinking {
                thinking,
                signature,
            } => keep_thinking && !thinking.is_empty() && !signature.is_empty(),
            _ => true,
        });
        !blocks.is_empty()
    });
}

fn hex_prefix(bytes: &[u8], len: usize) -> String {
    let mut out = String::with_capacity(len);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
        if out.len() >= len {
            out.truncate(len);
            return out;
        }
    }
    out
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut key_block = [0u8; BLOCK];
    if key.len() > BLOCK {
        key_block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= key_block[i];
        opad[i] ^= key_block[i];
    }
    let inner = Sha256::new()
        .chain_update(ipad)
        .chain_update(data)
        .finalize();
    let out = Sha256::new()
        .chain_update(opad)
        .chain_update(inner)
        .finalize();
    out.into()
}

fn compute_body_attestation(serialized_body: String) -> String {
    if !serialized_body.contains(CLAUDE_CCH_PLACEHOLDER) {
        return serialized_body;
    }
    let mac = hmac_sha256(
        CLAUDE_FINGERPRINT_SALT.as_bytes(),
        serialized_body.as_bytes(),
    );
    let cch = hex_prefix(&mac, 5);
    serialized_body.replacen(CLAUDE_CCH_PLACEHOLDER, &format!("cch={cch}"), 1)
}

fn strip_provider_tool_prefix_value(value: &mut serde_json::Value, prefix: &str) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(name)) = map.get_mut("name") {
                *name = decode_provider_tool_name(name, prefix);
            }
            for child in map.values_mut() {
                strip_provider_tool_prefix_value(child, prefix);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                strip_provider_tool_prefix_value(child, prefix);
            }
        }
        _ => {}
    }
}

fn serializable_headers(headers: &HeaderMap) -> BTreeMap<String, String> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_owned(), value.to_owned()))
        })
        .collect()
}

async fn run_provider_command_adapter(
    adapter: &ProviderCommandAdapter,
    request: ProviderCommandRequest,
) -> Result<ProviderCommandResponse> {
    let input = serde_json::to_vec(&request).map_err(SamplingError::Serialization)?;
    let mut command = tokio::process::Command::new(&adapter.argv[0]);
    command
        .args(&adapter.argv[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|e| {
        SamplingError::serialization_message(format!("provider request adapter spawn failed: {e}"))
    })?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(&input).await.map_err(|e| {
            SamplingError::serialization_message(format!(
                "provider request adapter stdin write failed: {e}"
            ))
        })?;
    }
    let timeout = std::time::Duration::from_millis(adapter.timeout_ms.unwrap_or(10_000));
    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| {
            SamplingError::serialization_message("provider request adapter command timed out")
        })?
        .map_err(|e| {
            SamplingError::serialization_message(format!(
                "provider request adapter wait failed: {e}"
            ))
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SamplingError::serialization_message(format!(
            "provider request adapter command failed: {stderr}"
        )));
    }
    serde_json::from_slice::<ProviderCommandResponse>(&output.stdout)
        .map_err(SamplingError::Serialization)
}

fn extract_model_metadata(headers: &reqwest::header::HeaderMap) -> Option<ResponseModelMetadata> {
    let context_window = headers
        .get("x-grok-context-window")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());

    let max_completion_tokens = headers
        .get("x-grok-max-completion-tokens")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u32>().ok());

    let models_etag = headers
        .get("x-models-etag")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    if context_window.is_some() || max_completion_tokens.is_some() || models_etag.is_some() {
        Some(ResponseModelMetadata {
            context_window,
            max_completion_tokens,
            models_etag,
        })
    } else {
        None
    }
}

/// Wrapper for streaming chat completion requests that adds `stream` and
/// `stream_options` fields without modifying the original `ChatCompletionRequest`.
///
/// Uses `#[serde(flatten)]` to inline all fields from the inner request,
/// allowing single-pass serialization instead of the previous two-pass
/// approach (serialize to `Value`, mutate, serialize to bytes).
#[derive(Serialize)]
struct StreamingChatRequest<'a> {
    #[serde(flatten)]
    inner: &'a ChatCompletionRequest,
    stream: bool,
    stream_options: StreamOptions,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

/// Resolve `env_http_headers` (`header -> env var`) into `headers` via `getenv`, skipping unset/blank/invalid entries and trimming values.
fn apply_env_http_headers(
    env_http_headers: &IndexMap<String, String>,
    getenv: impl Fn(&str) -> Option<String>,
    headers: &mut HeaderMap,
) {
    for (key, env_var) in env_http_headers {
        let Some(value) = getenv(env_var) else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        let (Ok(name), Ok(header_value)) = (
            HeaderName::try_from(key.as_str()),
            HeaderValue::from_str(value),
        ) else {
            tracing::warn!(
                header = %key,
                env_var = %env_var,
                "skipping env_http_header with an invalid header name or value"
            );
            continue;
        };
        headers.insert(name, header_value);
    }
}

/// HTTP client for sampling. Cheap to clone; carries an `Arc`-backed
/// `reqwest::Client` and the default headers/request-defaults computed from a
/// [`SamplerConfig`] at construction time.
#[derive(Clone)]
pub struct SamplingClient {
    http: reqwest::Client,
    default_headers: HeaderMap,
    base_url: String,
    defaults: ClientDefaults,
    /// Optional 401-attribution hook. The shell wires this to emit a
    /// structured event at every UNAUTHORIZED arm so 401s can be
    /// bucketed by stale-snapshot vs. live-token-rejected. `None` for
    /// sampler-only callers and tests.
    attribution_callback: Option<crate::attribution::SharedAttributionCallback>,
    /// Per-request bearer override. See `SamplerConfig::bearer_resolver`.
    bearer_resolver: Option<crate::config::SharedBearerResolver>,
    /// Per-request header injection (OTel traceparent).
    header_injector: Option<crate::config::SharedHeaderInjector>,
    /// Endpoint URL builder, resolved once from `base_url` + `query_params`.
    endpoint: EndpointTemplate,
}

impl std::fmt::Debug for SamplingClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SamplingClient")
            .field("base_url", &self.base_url)
            .field("defaults", &self.defaults)
            .field(
                "has_attribution_callback",
                &self.attribution_callback.is_some(),
            )
            .field("has_bearer_resolver", &self.bearer_resolver.is_some())
            .finish()
    }
}

#[derive(Clone, Debug, Default)]
struct ClientDefaults {
    model: String,
    max_completion_tokens: Option<u32>,
    context_window: u64,
    temperature: Option<f32>,
    top_p: Option<f32>,
    api_backend: ApiBackend,
    auth_scheme: AuthScheme,
    stream_tool_calls: bool,
    doom_loop_recovery: Option<xai_grok_sampling_types::DoomLoopRecoveryPolicy>,
    provider_request_adapter: Option<ProviderRequestAdapter>,
    advisor_server_model: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProviderCommandRequest {
    endpoint: String,
    model: String,
    headers: BTreeMap<String, String>,
    body: serde_json::Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderCommandResponse {
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default)]
    body: Option<serde_json::Value>,
}

/// Endpoint URL builder, resolved once at client construction so each request
/// only appends its path.
#[derive(Clone, Debug)]
enum EndpointTemplate {
    /// No query params and no query on the base URL (or an unparseable base):
    /// append the path to the base verbatim.
    Plain(String),
    /// Query params configured: `{prefix}/{path}{suffix}`. `suffix` starts with
    /// `?` and folds any base-URL params, with a configured key winning over the
    /// same key in `base_url` (percent-encoded, no duplicates).
    WithQuery { prefix: String, suffix: String },
}

impl EndpointTemplate {
    fn new(base_url: &str, query_params: &IndexMap<String, String>) -> Self {
        let base = base_url.trim_end_matches('/').to_string();
        // The fast path is safe only when there is nothing to fold: no configured
        // params and no query already on the base (which would otherwise land
        // before the appended path).
        if query_params.is_empty() && !base.contains('?') {
            return Self::Plain(base);
        }
        let mut url = match reqwest::Url::parse(&base) {
            Ok(url) => url,
            Err(error) => {
                tracing::warn!(
                    url = %base,
                    %error,
                    "failed to parse base URL for endpoint; sending without folded query"
                );
                return Self::Plain(base);
            }
        };
        let overridden: std::collections::HashSet<&str> =
            query_params.keys().map(String::as_str).collect();
        let kept: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(k, _)| !overridden.contains(k.as_ref()))
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        let prefix = {
            let mut prefix_url = url.clone();
            prefix_url.set_query(None);
            prefix_url.as_str().trim_end_matches('/').to_string()
        };
        {
            let mut pairs = url.query_pairs_mut();
            pairs.clear();
            for (key, value) in &kept {
                pairs.append_pair(key, value);
            }
            for (key, value) in query_params {
                pairs.append_pair(key, value);
            }
        }
        let suffix = url.query().map(|q| format!("?{q}")).unwrap_or_default();
        Self::WithQuery { prefix, suffix }
    }

    fn url_for_path(&self, path: &str) -> String {
        let path = path.trim_start_matches('/');
        match self {
            Self::Plain(base) => format!("{base}/{path}"),
            Self::WithQuery { prefix, suffix } => format!("{prefix}/{path}{suffix}"),
        }
    }
}

// =============================================================================
// User-Agent helpers
// =============================================================================

#[derive(Clone, Debug, Eq, PartialEq)]
struct PlatformInfo {
    os: String,
    arch: String,
}

impl PlatformInfo {
    fn current() -> Self {
        let os = match std::env::consts::OS {
            "macos" => "macos",
            "windows" => "windows",
            other => other,
        }
        .to_string();

        let arch = match std::env::consts::ARCH {
            "arm64" => "aarch64",
            "x86_64" => "x86_64",
            other => other,
        }
        .to_string();

        Self { os, arch }
    }
}

fn agent_version() -> String {
    xai_grok_version::VERSION.to_string()
}

/// Render a User-Agent string for the given origin client.
///
/// Mirrors the shell's `user_agent_string_for` but uses sampler-local
/// constants. The session typically owns the canonical User-Agent
/// rendering for process-wide HTTP clients; this helper is for
/// per-session sampling clients that want to override it.
pub fn user_agent_string_for(origin: &OriginClientInfo) -> String {
    let agent_version = agent_version();
    let platform = PlatformInfo::current();

    if origin.product == AGENT_PRODUCT && origin.version.as_deref() == Some(agent_version.as_str())
    {
        return format!(
            "{}/{} ({}; {})",
            AGENT_PRODUCT, agent_version, platform.os, platform.arch
        );
    }

    match origin.version.as_deref() {
        Some(origin_version) => format!(
            "{}/{} {}/{} ({}; {})",
            origin.product,
            origin_version,
            AGENT_PRODUCT,
            agent_version,
            platform.os,
            platform.arch
        ),
        None => format!(
            "{} {}/{} ({}; {})",
            origin.product, AGENT_PRODUCT, agent_version, platform.os, platform.arch
        ),
    }
}

/// A request builder coupled to the credential state it was built with, so
/// a 401 arm cannot classify from anything but the build-time capture. The
/// wire default (`SentCredential::Unknown`, which charges the retry budget)
/// stays the fail-closed one; only an explicit `sent_bearer: None` — a send
/// the builder provably stamped no credential onto — reaches the uncharged
/// lane via [`auth_rejected`].
struct SentRequest {
    builder: reqwest::RequestBuilder,
    /// Tail fragment of the credential in the built headers (`None` = no
    /// credential header at all).
    sent_bearer: Option<String>,
}

/// The one way a 401 becomes a `SamplingError::Auth` with a wire-derived
/// credential classification: from the fragment its [`SentRequest`] captured.
fn auth_rejected(message: String, sent_bearer: Option<&str>) -> SamplingError {
    SamplingError::Auth {
        message,
        credential: SentCredential::from_sent_fragment(sent_bearer),
    }
}

// =============================================================================
// SamplingClient
// =============================================================================

impl SamplingClient {
    /// Construct a sampling client from a [`SamplerConfig`].
    ///
    /// Grabs the process-wide shared `reqwest::Client` (HTTP/2 by
    /// default, HTTP/1.1 when `config.force_http1` is set) and
    /// pre-computes the default request headers. This does not perform
    /// any network I/O.
    pub fn new(config: SamplerConfig) -> Result<Self> {
        // Arms the `GROK_AUTH_TRACE` linkscope gate before the first request
        // so auth traces cover the whole client lifetime. No-op when unset.
        crate::auth_trace::trace_enabled();
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Some(ref api_key) = config.api_key {
            match config.auth_scheme {
                AuthScheme::XApiKey => {
                    let header_value = HeaderValue::from_str(api_key).map_err(|_| {
                        tracing::debug!(
                            api_key = %api_key,
                            "Invalid api_key: cannot be converted to a valid HTTP header"
                        );
                        SamplingError::auth_unknown(
                            "Invalid api_key: cannot be converted to a valid HTTP header",
                        )
                    })?;
                    headers.insert(HeaderName::from_static("x-api-key"), header_value);
                }
                AuthScheme::Bearer => {
                    let bearer = format!("Bearer {}", api_key);
                    let header_value = HeaderValue::from_str(&bearer).map_err(|_| {
                        tracing::debug!(
                            api_key = %api_key,
                            "Invalid api_key: cannot be converted to a valid HTTP Authorization header"
                        );
                        SamplingError::auth_unknown(
                            "Invalid api_key: cannot be converted to a valid HTTP Authorization header",
                        )
                    })?;
                    headers.insert(AUTHORIZATION, header_value);
                }
            }
        }

        // Apply all extra headers verbatim. This is the single
        // injection point for proxy-auth headers and any other URL- or
        // environment-specific headers the session decides to set.
        for (key, value) in &config.extra_headers {
            let header_name = HeaderName::try_from(key.as_str())
                .map_err(|_| SamplingError::InvalidConfiguration("Invalid extra header name"))?;
            let header_value = HeaderValue::from_str(value)
                .map_err(|_| SamplingError::InvalidConfiguration("Invalid extra header value"))?;
            headers.insert(header_name, header_value);
        }

        // Resolve here, not into `extra_headers`, so an env-sourced secret stays
        // out of persisted state.
        apply_env_http_headers(
            &config.env_http_headers,
            |var| std::env::var(var).ok(),
            &mut headers,
        );

        // Add x-grok-client-version header for version gating at the proxy.
        if let Some(client_version) = config.client_version.as_ref()
            && let Ok(header_value) = HeaderValue::from_str(client_version)
        {
            headers.insert(
                HeaderName::from_static("x-grok-client-version"),
                header_value,
            );
        }

        if let Some(deployment_id) = config.deployment_id.as_ref()
            && let Ok(header_value) = HeaderValue::from_str(deployment_id)
        {
            headers.insert(
                HeaderName::from_static("x-grok-deployment-id"),
                header_value,
            );
        }

        if let Some(user_id) = config.user_id.as_ref()
            && let Ok(header_value) = HeaderValue::from_str(user_id)
        {
            headers.insert(HeaderName::from_static("x-grok-user-id"), header_value);
        }

        {
            let client_id = config
                .client_identifier
                .clone()
                .unwrap_or_else(|| DEFAULT_CLIENT_IDENTIFIER.to_string());
            if let Ok(header_value) = HeaderValue::from_str(&client_id) {
                headers.insert(
                    HeaderName::from_static("x-grok-client-identifier"),
                    header_value,
                );
            }
        }

        // Always set User-Agent: per-session origin if available, else fallback.
        {
            let ua_string = match config.origin_client.as_ref() {
                Some(origin) => user_agent_string_for(origin),
                None => user_agent_string_for(&OriginClientInfo {
                    product: AGENT_PRODUCT.to_string(),
                    version: Some(agent_version()),
                }),
            };
            if let Ok(v) = HeaderValue::from_str(&ua_string) {
                headers.insert(USER_AGENT, v);
            }
        }

        let http = if config.force_http1 {
            tracing::info!("Using HTTP/1.1 for sampling client (force_http1=true)");
            crate::shared_http::client_http1().map_err(SamplingError::Http)?
        } else {
            crate::shared_http::client().map_err(SamplingError::Http)?
        };

        tracing::info!(
            target: crate::sampling_log::TARGET,
            event = "client_new",
            base_url = %config.base_url,
            model = %config.model,
            api_backend = ?config.api_backend,
            auth_scheme = ?config.auth_scheme,
            // "unset" (not "none"): `ReasoningEffort::None` is a real wire value;
            // logging the absent Option as "none" looked like we were sending it.
            reasoning_effort = config.reasoning_effort.map_or("unset", |e| e.as_str()),
            has_api_key = config.api_key.is_some(),
            has_bearer_resolver = config.bearer_resolver.is_some(),
            has_authorization_header = headers.get(AUTHORIZATION).is_some(),
            has_x_api_key_header = headers.get(HeaderName::from_static("x-api-key")).is_some(),
        );

        let defaults = ClientDefaults {
            model: config.model,
            max_completion_tokens: config.max_completion_tokens,
            context_window: config.context_window,
            temperature: config.temperature,
            top_p: config.top_p,
            api_backend: config.api_backend,
            auth_scheme: config.auth_scheme,
            stream_tool_calls: config.stream_tool_calls,
            doom_loop_recovery: config.doom_loop_recovery,
            provider_request_adapter: config.provider_request_adapter,
            advisor_server_model: config.advisor_server_model,
        };

        let endpoint = EndpointTemplate::new(&config.base_url, &config.query_params);

        Ok(Self {
            http,
            default_headers: headers,
            base_url: config.base_url,
            defaults,
            attribution_callback: config.attribution_callback,
            bearer_resolver: config.bearer_resolver,
            header_injector: config.header_injector,
            endpoint,
        })
    }

    /// The configured API backend for this client.
    pub fn api_backend(&self) -> ApiBackend {
        self.defaults.api_backend.clone()
    }

    /// Default headers for a provider request. Overrides auth from resolver if wired.
    fn request_headers(&self) -> HeaderMap {
        let mut headers = self.default_headers.clone();
        if let Some(resolver) = &self.bearer_resolver {
            headers.remove(AUTHORIZATION);
            headers.remove(HeaderName::from_static("x-api-key"));
            if let Some(fresh) = resolver.current_bearer() {
                match self.defaults.auth_scheme {
                    AuthScheme::XApiKey => {
                        if let Ok(v) = HeaderValue::from_str(&fresh) {
                            headers.insert(HeaderName::from_static("x-api-key"), v);
                        }
                    }
                    AuthScheme::Bearer => {
                        if let Ok(v) = HeaderValue::from_str(&format!("Bearer {fresh}")) {
                            headers.insert(AUTHORIZATION, v);
                        }
                    }
                }
            }
        }
        {
            let auth_prefix = headers
                .get(AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.chars().take(20).collect::<String>());
            let x_api_key_prefix = headers
                .get(HeaderName::from_static("x-api-key"))
                .and_then(|v| v.to_str().ok())
                .map(|s| s.chars().take(12).collect::<String>());
            tracing::info!(
                target: crate::sampling_log::TARGET,
                event = "client_post",
                base_url = %self.base_url,
                model = %self.defaults.model,
                api_backend = ?self.defaults.api_backend,
                auth_scheme = ?self.defaults.auth_scheme,
                has_bearer_resolver = self.bearer_resolver.is_some(),
                has_authorization_header = headers.get(AUTHORIZATION).is_some(),
                has_x_api_key_header = headers.get(HeaderName::from_static("x-api-key")).is_some(),
                auth_header_prefix = auth_prefix.as_deref().unwrap_or("none"),
                x_api_key_prefix = x_api_key_prefix.as_deref().unwrap_or("none"),
            );
        }
        if let Some(injector) = &self.header_injector {
            injector.inject(&mut headers);
        }
        headers
    }

    /// POST with default headers and the credential fragment placed on the
    /// request. Capturing it at build time keeps 401 attribution stable even
    /// if a live credential resolver rotates immediately afterward.
    fn post(&self, url: impl reqwest::IntoUrl) -> SentRequest {
        let headers = self.request_headers();
        let sent_bearer = Self::sent_fragment_from_headers(&headers, &self.defaults.auth_scheme);
        SentRequest {
            builder: self.http.post(url).headers(headers),
            sent_bearer,
        }
    }

    fn apply_messages_request_adapter(&self, request: &mut messages::MessagesRequest) {
        let Some(ProviderRequestAdapter::Anthropic {
            tool_name_prefix, ..
        }) = self.defaults.provider_request_adapter.as_ref()
        else {
            return;
        };
        sanitize_thinking_blocks(request);
        ensure_claude_code_system_blocks(request);
        if let Some(tools) = request.tools.as_mut() {
            for tool in tools {
                tool.name = encode_provider_tool_name(&tool.name, tool_name_prefix);
            }
        }
    }

    /// Whether the server-side advisor tool (beta header + raw tool
    /// injection) is active for this client: only on the Anthropic
    /// Messages API, over OAuth (Bearer) auth, going through the
    /// Anthropic adapter, with an advisor model configured.
    fn advisor_gate_active(&self) -> bool {
        self.defaults.api_backend == ApiBackend::Messages
            && is_anthropic_adapter(self.defaults.provider_request_adapter.as_ref())
            && self.defaults.auth_scheme == AuthScheme::Bearer
            && self.defaults.advisor_server_model.is_some()
    }

    /// Injects the wrapper's `extra_raw_tools` (plus the server advisor
    /// tool, when [`Self::advisor_gate_active`]) into the serialized
    /// Messages request body's `tools` array. Mirrors the Responses API
    /// `extra_raw_tools` injection.
    fn inject_messages_extra_tools(
        &self,
        extra_raw_tools: &mut Vec<serde_json::Value>,
        body: &mut serde_json::Value,
    ) {
        if self.advisor_gate_active() {
            if let Some(model) = self.defaults.advisor_server_model.as_deref() {
                extra_raw_tools.push(serde_json::json!({
                    "type": "advisor_20260301",
                    "name": "advisor",
                    "model": model,
                }));
            }
        }
        if !extra_raw_tools.is_empty() {
            if let Some(tools) = body.get_mut("tools").and_then(|v| v.as_array_mut()) {
                tools.extend(extra_raw_tools.drain(..));
            } else {
                body["tools"] = serde_json::Value::Array(std::mem::take(extra_raw_tools));
            }
        }
    }

    fn apply_anthropic_cli_headers(&self, headers: &mut HeaderMap, _model: &str) {
        if !is_anthropic_adapter(self.defaults.provider_request_adapter.as_ref()) {
            return;
        }
        let existing = headers
            .get("anthropic-beta")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        let mut betas: Vec<&str> = CLAUDE_BETAS
            .iter()
            .copied()
            .filter(|beta| {
                (*beta != "context-1m-2025-08-07" || self.defaults.context_window > 200_000)
                    && (*beta != "advisor-tool-2026-03-01" || self.advisor_gate_active())
            })
            .collect();
        for beta in existing.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            if !betas.contains(&beta) {
                betas.push(beta);
            }
        }
        headers.insert(
            "anthropic-beta",
            HeaderValue::from_str(&betas.join(",")).expect("static beta header is valid"),
        );
        headers.insert(
            USER_AGENT,
            HeaderValue::from_str(&format!(
                "claude-cli/{} (external, cli)",
                claude_cli_version()
            ))
            .expect("static user-agent is valid"),
        );
        headers.insert("x-app", HeaderValue::from_static("cli"));
        headers.insert("anthropic-client-platform", HeaderValue::from_static("cli"));
        headers.insert(
            "anthropic-dangerous-direct-browser-access",
            HeaderValue::from_static("true"),
        );
        headers
            .entry("x-client-request-id")
            .or_insert_with(|| HeaderValue::from_str(&uuid::Uuid::new_v4().to_string()).unwrap());
        let session_id = std::env::var("CLAUDE_CODE_SESSION_ID").unwrap_or_default();
        headers
            .entry("x-claude-code-session-id")
            .or_insert_with(|| HeaderValue::from_str(&sanitize_header_value(&session_id)).unwrap());
    }

    fn adapt_messages_event_json(&self, data: &str) -> String {
        adapt_messages_event_json(self.defaults.provider_request_adapter.as_ref(), data)
    }

    fn adapt_messages_response_bytes(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        let Some(ProviderRequestAdapter::Anthropic { .. }) =
            self.defaults.provider_request_adapter.as_ref()
        else {
            return Ok(bytes.to_vec());
        };
        let body = std::str::from_utf8(bytes).map_err(SamplingError::serialization_message)?;
        Ok(self.adapt_messages_event_json(body).into_bytes())
    }

    fn command_adapter(&self) -> Option<ProviderCommandAdapter> {
        match self.defaults.provider_request_adapter.as_ref()? {
            ProviderRequestAdapter::Anthropic { command, .. } => command.clone(),
            ProviderRequestAdapter::Command { argv, timeout_ms } => Some(ProviderCommandAdapter {
                argv: argv.clone(),
                timeout_ms: *timeout_ms,
            }),
        }
    }

    async fn apply_command_adapter(
        &self,
        endpoint: &str,
        model: &str,
        headers: &mut HeaderMap,
        body: &mut serde_json::Value,
    ) -> Result<()> {
        let Some(adapter) = self.command_adapter() else {
            return Ok(());
        };
        if adapter.argv.is_empty() {
            return Err(SamplingError::InvalidConfiguration(
                "provider request adapter command argv is empty",
            ));
        }
        let request = ProviderCommandRequest {
            endpoint: endpoint.to_owned(),
            model: model.to_owned(),
            headers: serializable_headers(headers),
            body: body.clone(),
        };
        let response = run_provider_command_adapter(&adapter, request).await?;
        for (key, value) in response.headers {
            let name = HeaderName::try_from(key.as_str()).map_err(|e| {
                SamplingError::serialization_message(format!(
                    "provider request adapter returned invalid header name `{key}`: {e}"
                ))
            })?;
            let value = HeaderValue::from_str(&value).map_err(|e| {
                SamplingError::serialization_message(format!(
                    "provider request adapter returned invalid value for `{key}`: {e}"
                ))
            })?;
            headers.insert(name, value);
        }
        if let Some(new_body) = response.body {
            *body = new_body;
        }
        Ok(())
    }

    fn serialize_messages_body(&self, body: &serde_json::Value) -> Result<String> {
        let serialized = serde_json::to_string(body).map_err(SamplingError::Serialization)?;
        if is_anthropic_adapter(self.defaults.provider_request_adapter.as_ref()) {
            Ok(compute_body_attestation(serialized))
        } else {
            Ok(serialized)
        }
    }

    /// Tail fragment of the credential in `headers` — `x-api-key`
    /// (Messages-API scheme) or `Authorization` — per
    /// [`crate::attribution::SENT_BEARER_PREFIX_LEN`].
    fn sent_fragment_from_headers(headers: &HeaderMap, scheme: &AuthScheme) -> Option<String> {
        let raw = match scheme {
            AuthScheme::XApiKey => headers
                .get(HeaderName::from_static("x-api-key"))
                .and_then(|v| v.to_str().ok()),
            AuthScheme::Bearer => headers
                .get(AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.strip_prefix("Bearer ")),
        };
        raw.map(|s| bearer_tail_fragment(s).to_string())
    }

    /// Best-effort *build-time* view of what the next request would carry
    /// (resolver-authoritative). For request-start diagnostics
    /// ([`Self::auth_info`]) only — 401 attribution must use the fragment
    /// captured by [`Self::post`] instead, which cannot race a recovery.
    fn current_sent_bearer_prefix(&self) -> Option<String> {
        if self.bearer_resolver.is_some() {
            return self
                .bearer_resolver
                .as_ref()
                .and_then(|r| r.current_bearer())
                .map(|s| bearer_tail_fragment(&s).to_string());
        }
        Self::sent_fragment_from_headers(&self.default_headers, &self.defaults.auth_scheme)
    }

    /// Invoke the optional 401 attribution callback for one logical
    /// 401 response. Each of the six UNAUTHORIZED arms in this file
    /// calls this helper immediately before returning
    /// `SamplingError::Auth(...)`. Emit happens at the lowest layer
    /// that saw the status, so higher layers that react to a 401 must
    /// not emit a duplicate event.
    ///
    /// `sent_prefix` is the fragment [`Self::post`] captured for the
    /// rejected request (already tail-truncated; the full bearer never
    /// crosses this boundary).
    fn record_401_attribution(
        &self,
        consumer: crate::attribution::SamplingConsumer,
        sent_prefix: Option<&str>,
    ) {
        if let Some(cb) = self.attribution_callback.as_ref() {
            cb.record_401(consumer, sent_prefix);
        }
    }

    pub fn auth_info(&self) -> crate::sampling_log::AuthInfo {
        let auth_prefix = self.current_sent_bearer_prefix();
        let auth_type = match (&self.defaults.auth_scheme, &auth_prefix) {
            (AuthScheme::XApiKey, Some(_)) => "x-api-key",
            (AuthScheme::Bearer, Some(_)) => "bearer",
            (_, None) => "none",
        };
        crate::sampling_log::AuthInfo {
            auth_type,
            auth_prefix,
        }
    }

    /// Check if a header name contains sensitive information that should be redacted.
    fn is_sensitive_header(name: &str) -> bool {
        let lower = name.to_lowercase();
        lower.contains("authorization")
            || lower.contains("api-key")
            || lower.contains("apikey")
            || lower.contains("token")
            || lower.contains("secret")
    }

    /// Short lossy body snippet for error logs (never user-facing).
    fn body_preview(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).chars().take(500).collect()
    }

    /// Log all headers from a request at debug level (redacting sensitive values).
    fn log_request_headers(request: &reqwest::Request, endpoint_name: &str) {
        for (name, value) in request.headers().iter() {
            let value_str = if Self::is_sensitive_header(name.as_str()) {
                "[REDACTED]"
            } else {
                value.to_str().unwrap_or("[non-utf8]")
            };
            tracing::debug!(
                header_name = %name,
                header_value = %value_str,
                "Request header ({})",
                endpoint_name
            );
        }
    }

    fn endpoint(&self, path: &str) -> String {
        self.endpoint.url_for_path(path)
    }

    fn apply_defaults(&self, mut request: ChatCompletionRequest) -> Result<ChatCompletionRequest> {
        if request.model.is_none() {
            request.model = Some(self.defaults.model.clone());
        }

        if request.max_tokens.is_none() {
            request.max_tokens = self.defaults.max_completion_tokens;
        }

        if request.temperature.is_none() {
            request.temperature = self.defaults.temperature;
        }

        if request.top_p.is_none() {
            request.top_p = self.defaults.top_p;
        }

        Ok(request)
    }

    /// `sent_bearer` is the fragment [`Self::post`] captured for the
    /// request that produced `response` (401 attribution).
    async fn handle_response(
        &self,
        response: reqwest::Response,
        sent_bearer: Option<&str>,
    ) -> Result<ChatCompletionResponse> {
        let status = response.status();
        let model_metadata = extract_model_metadata(response.headers());
        let retry_after_secs = extract_retry_after(response.headers());
        let should_retry = extract_should_retry(response.headers());
        let bytes = response.bytes().await?;

        if !status.is_success() {
            if status == reqwest::StatusCode::UNAUTHORIZED {
                self.record_401_attribution(
                    crate::attribution::SamplingConsumer::ChatCompletions,
                    sent_bearer,
                );
                let server_message = user_facing_api_error_message(status, bytes.as_ref());
                return Err(auth_rejected(
                    format!("Unauthorized (401): {server_message}"),
                    sent_bearer,
                ));
            }
            let message = user_facing_api_error_message(status, bytes.as_ref());
            return Err(SamplingError::Api {
                status,
                message,
                model_metadata,
                retry_after_secs,
                should_retry,
            });
        }

        let completion = serde_json::from_slice::<ChatCompletionResponse>(&bytes).map_err(|e| {
            let raw_body = String::from_utf8_lossy(&bytes);
            tracing::error!(
                error = %e,
                raw_body = %raw_body,
                "Failed to deserialize ChatCompletionResponse"
            );
            SamplingError::Serialization(e)
        })?;
        Ok(completion)
    }

    // =========================================================================
    // Chat Completions API
    // =========================================================================

    pub async fn chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse> {
        let payload = self.apply_defaults(request)?;
        let x_grok_conv_id = &payload.x_grok_conv_id.clone().unwrap_or_default();
        let x_grok_req_id = &payload.x_grok_req_id.clone().unwrap_or_default();
        let model_id = payload.model.clone().unwrap_or_default();

        tracing::debug!(
            base_url = %self.base_url,
            model_id = %model_id,
            "Sending chat completion request"
        );

        let grok_headers = GrokRequestHeaders {
            conv_id: x_grok_conv_id,
            req_id: x_grok_req_id,
            model_id: &model_id,
            session_id: payload.x_grok_session_id.as_deref().unwrap_or_default(),
            turn_idx: payload.x_grok_turn_idx.as_deref(),
            agent_id: payload.x_grok_agent_id.as_deref().unwrap_or_default(),
            deployment_id: payload.x_grok_deployment_id.as_deref(),
            user_id: payload.x_grok_user_id.as_deref(),
        };
        let SentRequest {
            builder,
            sent_bearer,
        } = self.post(self.endpoint("chat/completions"));
        let http_request = grok_headers.apply(builder).json(&payload);

        let response = http_request.send().await.map_err(|e| {
            // Log at debug level; errors are surfaced to the caller.
            tracing::debug!("HTTP request failed: {}", e);
            e
        })?;

        self.handle_response(response, sent_bearer.as_deref()).await
    }

    /// Start a streaming chat completion request. Returns a stream of typed chunks.
    #[tracing::instrument(
        name = "http.chat_completion_stream",
        skip_all,
        fields(
            endpoint = %self.endpoint("chat/completions"),
            model_id = request.model.as_deref().unwrap_or(""),
            status_code = tracing::field::Empty,
            success = tracing::field::Empty,
            error = tracing::field::Empty,
        )
    )]
    pub async fn chat_completion_stream(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<(
        BoxStream<'static, Result<ChatCompletionChunk>>,
        Option<ResponseModelMetadata>,
    )> {
        let payload = self.apply_defaults(request)?;
        let x_grok_conv_id = &payload.x_grok_conv_id.clone().unwrap_or_default();
        let x_grok_req_id = &payload.x_grok_req_id.clone().unwrap_or_default();
        let model_id = payload.model.clone().unwrap_or_default();

        // Wrap the request with streaming fields and serialize once.
        // Previously this path serialized twice: first to serde_json::Value
        // (to inject `stream` and `stream_options`), then to HTTP body bytes.
        let streaming_request = StreamingChatRequest {
            inner: &payload,
            stream: true,
            stream_options: StreamOptions {
                include_usage: true,
            },
        };

        let grok_headers = GrokRequestHeaders {
            conv_id: x_grok_conv_id,
            req_id: x_grok_req_id,
            model_id: &model_id,
            session_id: payload.x_grok_session_id.as_deref().unwrap_or_default(),
            turn_idx: payload.x_grok_turn_idx.as_deref(),
            agent_id: payload.x_grok_agent_id.as_deref().unwrap_or_default(),
            deployment_id: payload.x_grok_deployment_id.as_deref(),
            user_id: payload.x_grok_user_id.as_deref(),
        };
        let mut request_body = serde_json::to_value(&streaming_request).map_err(|e| {
            tracing::error!("Failed to serialize chat/completions request: {}", e);
            SamplingError::Serialization(e)
        })?;
        let mut headers = self.request_headers();
        self.apply_command_adapter(
            "chat/completions",
            &model_id,
            &mut headers,
            &mut request_body,
        )
        .await?;
        let sent_bearer = Self::sent_fragment_from_headers(&headers, &self.defaults.auth_scheme);
        let http_request = grok_headers
            .apply(
                self.http
                    .post(self.endpoint("chat/completions"))
                    .headers(headers),
            )
            .header(ACCEPT, HeaderValue::from_static("text/event-stream"))
            .json(&request_body);

        let built_request = http_request.build().map_err(|e| {
            tracing::error!("Failed to build HTTP request: {}", e);
            SamplingError::Http(e)
        })?;

        tracing::debug!(
            url = %built_request.url(),
            method = %built_request.method(),
            "Sending chat/completions request"
        );
        Self::log_request_headers(&built_request, "chat/completions");

        let response = self.http.execute(built_request).await.map_err(|e| {
            tracing::debug!("HTTP request failed: {}", e);
            record_stream_request_failure(&e);
            e
        })?;

        let status = response.status();
        let span = tracing::Span::current();
        span.record("status_code", status.as_u16() as i64);
        span.record("success", status.is_success());
        let model_metadata = extract_model_metadata(response.headers());
        let retry_after_secs = extract_retry_after(response.headers());
        let should_retry = extract_should_retry(response.headers());
        if !status.is_success() {
            if status == reqwest::StatusCode::UNAUTHORIZED {
                span.record("error", "unauthorized (401)");
                self.record_401_attribution(
                    crate::attribution::SamplingConsumer::ChatCompletionsStream,
                    sent_bearer.as_deref(),
                );
                let endpoint = self.endpoint("chat/completions");
                let body = response.bytes().await.unwrap_or_default();
                let server_message = user_facing_api_error_message(status, body.as_ref());
                return Err(auth_rejected(
                    format!("Unauthorized (401) from {endpoint}: {server_message}"),
                    sent_bearer.as_deref(),
                ));
            }

            let bytes = response.bytes().await?;
            let message = user_facing_api_error_message(status, bytes.as_ref());
            span.record("error", message.as_str());
            tracing::error!(
                status = %status,
                error_message = %message,
                body_preview = %Self::body_preview(bytes.as_ref()),
                model_id = %model_id,
                "chat/completions API error"
            );
            return Err(SamplingError::Api {
                status,
                message,
                model_metadata,
                retry_after_secs,
                should_retry,
            });
        }

        // Strip UTF-8 BOM if present: eventsource-stream 0.2.3 incorrectly slices BOM at byte 1 instead of 3.
        const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];
        let mut is_first = true;
        let byte_stream = response.bytes_stream().map(move |result| {
            result.map(|bytes| {
                if is_first {
                    is_first = false;
                    if bytes.starts_with(UTF8_BOM) {
                        return bytes.slice(UTF8_BOM.len()..);
                    }
                }
                bytes
            })
        });

        // Turn raw bytes into SSE events
        let event_stream = byte_stream.eventsource();

        // Map SSE events into ChatCompletionChunk.
        // Uses `scan` so that `[DONE]` and transport errors both terminate the
        // stream (`None`). The first transport error is emitted to the consumer,
        // then subsequent polls return `None` -- preventing an infinite busy-loop
        // when the HTTP/2 connection drops and h2 keeps producing errors.
        let chunks = event_stream
            .scan(false, |had_transport_error, event_res| {
                if *had_transport_error {
                    return std::future::ready(None);
                }
                let item = match event_res {
                    Ok(event) => {
                        let data = &event.data;
                        if data == "[DONE]" {
                            return std::future::ready(None);
                        }

                        tracing::info!(
                            target: crate::sampling_log::TARGET,
                            event = "sse_chunk",
                            backend = "chat_completions",
                            data = %data,
                        );

                        if let Some(stream_error) = try_parse_stream_error(data) {
                            Some(Err(stream_error))
                        } else {
                            Some(
                                serde_json::from_str::<ChatCompletionChunk>(data).map_err(|e| {
                                    tracing::error!(
                                        error = %e,
                                        raw_data = %data,
                                        "Failed to deserialize ChatCompletionChunk from stream"
                                    );
                                    SamplingError::Serialization(e)
                                }),
                            )
                        }
                    }
                    Err(e) => {
                        *had_transport_error = true;
                        Some(Err(SamplingError::EventStreamError(e.to_string())))
                    }
                };
                std::future::ready(item)
            })
            .boxed();

        Ok((chunks, model_metadata))
    }

    // =========================================================================
    // Responses API
    // =========================================================================

    /// Apply default configuration to a Responses API request.
    fn apply_response_defaults(&self, request: &mut CreateResponseWrapper) -> Result<()> {
        // Apply model default if not specified
        if request.inner.model.is_none() {
            request.inner.model = Some(self.defaults.model.clone());
        }

        // Apply temperature default if not specified
        if request.inner.temperature.is_none() {
            request.inner.temperature = self.defaults.temperature;
        }

        // Apply top_p default if not specified
        if request.inner.top_p.is_none() {
            request.inner.top_p = self.defaults.top_p;
        }

        // Apply max_output_tokens default if not specified
        if request.inner.max_output_tokens.is_none() {
            request.inner.max_output_tokens = self.defaults.max_completion_tokens;
        }

        // Set store to false if not specified (default is true, but that breaks ZDR compliance)
        if request.inner.store.is_none() {
            request.inner.store = Some(false);
        }

        // Include encrypted reasoning content if not specified
        let includes = request.inner.include.get_or_insert_with(Vec::new);
        if !includes.contains(&rs::IncludeEnum::ReasoningEncryptedContent) {
            includes.push(rs::IncludeEnum::ReasoningEncryptedContent);
        }

        Ok(())
    }

    /// Create a response using the Responses API (non-streaming).
    ///
    /// This uses the Responses API format which provides a simpler interface
    /// for multi-turn conversations and tool calling.
    pub async fn create_response(
        &self,
        mut request: CreateResponseWrapper,
    ) -> Result<rs::Response> {
        self.apply_response_defaults(&mut request)?;

        let x_grok_conv_id = request.x_grok_conv_id.as_deref().unwrap_or_default();
        let x_grok_req_id = request.x_grok_req_id.as_deref().unwrap_or_default();
        let model_id = request.inner.model.clone().unwrap_or_default();

        // The trace field is process-local: it is consumed by upstream
        // session code (which may upload a payload artifact) and is not
        // forwarded by the sampler. Drop it before we send.
        request.trace.take();

        tracing::debug!("create_response: {:?}", &request);
        tracing::debug!("endpoint: {:?}", self.endpoint("responses"));

        let grok_headers = GrokRequestHeaders {
            conv_id: x_grok_conv_id,
            req_id: x_grok_req_id,
            model_id: &model_id,
            session_id: request.x_grok_session_id.as_deref().unwrap_or_default(),
            turn_idx: request.x_grok_turn_idx.as_deref(),
            agent_id: request.x_grok_agent_id.as_deref().unwrap_or_default(),
            deployment_id: request.x_grok_deployment_id.as_deref(),
            user_id: request.x_grok_user_id.as_deref(),
        };
        let mut request_body = serde_json::to_value(&request.inner).map_err(|e| {
            tracing::error!("Failed to serialize responses request: {}", e);
            SamplingError::Serialization(e)
        })?;
        // async-openai's ReasoningTextContent struct omits the `type`
        // discriminator that the Responses API requires on input. Patch
        // it in post-serialize. This is the last surviving piece of the
        // old raw_output machinery.
        xai_grok_sampling_types::patch_reasoning_text_types(&mut request_body);
        let SentRequest {
            builder,
            sent_bearer,
        } = self.post(self.endpoint("responses"));
        let http_request = grok_headers.apply(builder).json(&request_body);

        let response = http_request.send().await.map_err(|e| {
            tracing::debug!("HTTP request failed: {}", e);
            e
        })?;

        let status = response.status();
        let model_metadata = extract_model_metadata(response.headers());
        let retry_after_secs = extract_retry_after(response.headers());
        let should_retry = extract_should_retry(response.headers());
        let bytes = response.bytes().await?;

        if !status.is_success() {
            if status == reqwest::StatusCode::UNAUTHORIZED {
                self.record_401_attribution(
                    crate::attribution::SamplingConsumer::Responses,
                    sent_bearer.as_deref(),
                );
                let endpoint = self.endpoint("responses");
                let server_message = user_facing_api_error_message(status, bytes.as_ref());
                return Err(auth_rejected(
                    format!("Unauthorized (401) from {endpoint}: {server_message}"),
                    sent_bearer.as_deref(),
                ));
            }

            let message = user_facing_api_error_message(status, bytes.as_ref());
            tracing::warn!(
                status = %status,
                error_message = %message,
                body_preview = %Self::body_preview(bytes.as_ref()),
                model_id = %model_id,
                "responses API error"
            );
            return Err(SamplingError::Api {
                status,
                message,
                model_metadata,
                retry_after_secs,
                should_retry,
            });
        }

        let response_obj = serde_json::from_slice::<rs::Response>(&bytes).map_err(|e| {
            let raw_body = String::from_utf8_lossy(&bytes);
            tracing::error!(
                error = %e,
                raw_body = %raw_body,
                "Failed to deserialize rs::Response"
            );
            SamplingError::Serialization(e)
        })?;
        Ok(response_obj)
    }

    /// Create a streaming response using the Responses API.
    ///
    /// Returns a stream of `rs::ResponseStreamEvent` which includes events like:
    /// - `response.created` - Initial response object
    /// - `response.output_text.delta` - Text content deltas
    /// - `response.function_call_arguments.delta` - Function call argument deltas
    /// - `response.completed` - Final response with all output
    ///
    /// The third tuple element is a per-request doom-loop signal collector,
    /// `Some` only when `SamplerConfig::doom_loop_recovery` is set — the same
    /// gate that adds the opt-in `x-grok-doom-loop-check` request header, so
    /// header and parse protection cannot drift apart. It is filled by the
    /// SSE decoder as the server reports triggers and is meant to be handed
    /// to `stream_responses` so the signals land on the final
    /// `ConversationResponse`.
    #[tracing::instrument(
        name = "http.create_response_stream",
        skip_all,
        fields(
            endpoint = %self.endpoint("responses"),
            model_id = request.inner.model.as_deref().unwrap_or(""),
            status_code = tracing::field::Empty,
            success = tracing::field::Empty,
            error = tracing::field::Empty,
        )
    )]
    #[allow(clippy::type_complexity)]
    pub async fn create_response_stream(
        &self,
        mut request: CreateResponseWrapper,
    ) -> Result<(
        BoxStream<'static, Result<rs::ResponseStreamEvent>>,
        Option<ResponseModelMetadata>,
        Option<crate::doom_loop::DoomLoopSignalCollector>,
    )> {
        self.apply_response_defaults(&mut request)?;

        // Enable streaming
        request.inner.stream = Some(true);

        let x_grok_conv_id = request.x_grok_conv_id.as_deref().unwrap_or_default();
        let x_grok_req_id = request.x_grok_req_id.as_deref().unwrap_or_default();
        let model_id = request.inner.model.clone().unwrap_or_default();

        // Drop process-local trace data (see note in `create_response`).
        request.trace.take();

        tracing::debug!(
            base_url = %self.base_url,
            model_id = model_id.as_str(),
            "Sending responses API stream request"
        );

        let grok_headers = GrokRequestHeaders {
            conv_id: x_grok_conv_id,
            req_id: x_grok_req_id,
            model_id: &model_id,
            session_id: request.x_grok_session_id.as_deref().unwrap_or_default(),
            turn_idx: request.x_grok_turn_idx.as_deref(),
            agent_id: request.x_grok_agent_id.as_deref().unwrap_or_default(),
            deployment_id: request.x_grok_deployment_id.as_deref(),
            user_id: request.x_grok_user_id.as_deref(),
        };
        let extra_tool_entries = std::mem::take(&mut request.extra_tool_entries);
        let mut request_body = serde_json::to_value(&request.inner).map_err(|e| {
            tracing::error!("Failed to serialize responses request: {}", e);
            SamplingError::Serialization(e)
        })?;
        // Inject xAI-specific fields not in async-openai's CreateResponse type.
        if self.defaults.stream_tool_calls {
            request_body["stream_tool_calls"] = serde_json::json!(true);
        }
        // Inject xAI-specific tools (e.g., x_search) that can't be expressed
        // via async_openai's rs::Tool enum.
        if !extra_tool_entries.is_empty() {
            if let Some(tools) = request_body.get_mut("tools").and_then(|v| v.as_array_mut()) {
                tools.extend(extra_tool_entries);
            } else {
                request_body["tools"] = serde_json::Value::Array(extra_tool_entries);
            }
        }
        xai_grok_sampling_types::patch_reasoning_text_types(&mut request_body);
        // Fresh per attempt so signals never leak across retries; `None`
        // (check disabled) sends no header and does no peek work per event.
        let doom_loop = self
            .defaults
            .doom_loop_recovery
            .map(crate::doom_loop::DoomLoopSignalCollector::new);
        let mut headers = self.request_headers();
        self.apply_command_adapter("responses", &model_id, &mut headers, &mut request_body)
            .await?;
        let sent_bearer = Self::sent_fragment_from_headers(&headers, &self.defaults.auth_scheme);
        let mut http_request = grok_headers
            .apply(self.http.post(self.endpoint("responses")).headers(headers))
            .header(ACCEPT, HeaderValue::from_static("text/event-stream"));
        if doom_loop.is_some() {
            // Presence opts in; the server ignores the value.
            http_request = http_request.header(DOOM_LOOP_CHECK_HEADER, "true");
        }
        let http_request = http_request.json(&request_body);

        let built_request = http_request.build().map_err(|e| {
            tracing::error!("Failed to build HTTP request: {}", e);
            SamplingError::Http(e)
        })?;

        tracing::debug!(
            url = %built_request.url(),
            method = %built_request.method(),
            "Sending responses API stream request"
        );
        Self::log_request_headers(&built_request, "responses");

        let response = self.http.execute(built_request).await.map_err(|e| {
            tracing::debug!("HTTP request failed: {}", e);
            record_stream_request_failure(&e);
            e
        })?;

        let status = response.status();
        let span = tracing::Span::current();
        span.record("status_code", status.as_u16() as i64);
        span.record("success", status.is_success());
        if !status.is_success() {
            if status == reqwest::StatusCode::UNAUTHORIZED {
                span.record("error", "unauthorized (401)");
                self.record_401_attribution(
                    crate::attribution::SamplingConsumer::ResponsesStream,
                    sent_bearer.as_deref(),
                );
                let endpoint = self.endpoint("responses");
                let body = response.bytes().await.unwrap_or_default();
                let server_message = user_facing_api_error_message(status, body.as_ref());
                return Err(auth_rejected(
                    format!("Unauthorized (401) from {endpoint}: {server_message}"),
                    sent_bearer.as_deref(),
                ));
            }
            let model_metadata = extract_model_metadata(response.headers());
            let retry_after_secs = extract_retry_after(response.headers());
            let should_retry = extract_should_retry(response.headers());
            let bytes = response.bytes().await?;
            let message = user_facing_api_error_message(status, bytes.as_ref());
            span.record("error", message.as_str());
            tracing::error!(
                status = %status,
                error_message = %message,
                body_preview = %Self::body_preview(bytes.as_ref()),
                model_id = %model_id,
                "responses API error"
            );
            return Err(SamplingError::Api {
                status,
                message,
                model_metadata,
                retry_after_secs,
                should_retry,
            });
        }

        let model_metadata = extract_model_metadata(response.headers());

        // Strip UTF-8 BOM if present
        const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];
        let mut is_first = true;
        let byte_stream = response.bytes_stream().map(move |result| {
            result.map(|bytes| {
                if is_first {
                    is_first = false;
                    if bytes.starts_with(UTF8_BOM) {
                        return bytes.slice(UTF8_BOM.len()..);
                    }
                }
                bytes
            })
        });

        // Turn raw bytes into SSE events
        let event_stream = byte_stream.eventsource();

        let doom_loop_for_stream = doom_loop.clone();

        // The scan item is an `Option`: `Some(None)` skips an absorbed
        // doom-loop event without terminating the stream (`filter_map`
        // below), while an outer `None` still ends it.
        let events = event_stream
            .scan(false, move |had_transport_error, event_res| {
                if *had_transport_error {
                    return std::future::ready(None);
                }
                let item = match event_res {
                    Ok(event) => {
                        let data = &event.data;
                        if data == "[DONE]" {
                            return std::future::ready(None);
                        }

                        tracing::info!(
                            target: crate::sampling_log::TARGET,
                            event = "sse_chunk",
                            backend = "responses",
                            data = %data,
                        );

                        // Intercept liveness-only and non-standard events before
                        // typed deserialization; async-openai's event enum does
                        // not know them and would fail to parse them. With the
                        // doom-loop check disabled, the shared name-or-payload-type
                        // predicate still guards against a server emitting it
                        // despite no opt-in (rollout skew), named or not.
                        let swallow = is_responses_keepalive_event(&event.event, data)
                            || match &doom_loop_for_stream {
                                Some(collector) => collector.absorb(&event.event, data),
                                None => is_check_event(&event.event, data),
                            };
                        if swallow {
                            Some(None)
                        } else if let Some(stream_error) = try_parse_stream_error(data) {
                            Some(Some(Err(stream_error)))
                        } else {
                            Some(Some(deserialize_response_event(data)))
                        }
                    }
                    Err(e) => {
                        *had_transport_error = true;
                        Some(Some(Err(SamplingError::EventStreamError(e.to_string()))))
                    }
                };
                std::future::ready(item)
            })
            .filter_map(std::future::ready)
            .boxed();

        Ok((events, model_metadata, doom_loop))
    }

    // =========================================================================
    // Anthropic Messages API
    // =========================================================================

    /// Apply default configuration to a Messages API request.
    fn apply_message_defaults(&self, request: &mut MessagesRequestWrapper) -> Result<()> {
        // Apply model default if not specified
        if request.inner.model.is_empty() {
            request.inner.model = self.defaults.model.clone();
        }

        if request.inner.max_tokens == 0 {
            request.inner.max_tokens = self
                .defaults
                .max_completion_tokens
                .unwrap_or(ANTHROPIC_DEFAULT_MAX_TOKENS);
        }

        // Apply temperature default if not specified
        if request.inner.temperature.is_none() {
            request.inner.temperature = self.defaults.temperature;
        }

        // Apply top_p default if not specified
        if request.inner.top_p.is_none() {
            request.inner.top_p = self.defaults.top_p;
        }

        Ok(())
    }

    /// Create a message using the Anthropic Messages API (non-streaming).
    pub async fn create_message(
        &self,
        mut request: MessagesRequestWrapper,
    ) -> Result<messages::MessagesResponse> {
        self.apply_message_defaults(&mut request)?;
        self.apply_messages_request_adapter(&mut request.inner);

        let x_grok_conv_id = request.x_grok_conv_id.as_deref().unwrap_or_default();
        let x_grok_req_id = request.x_grok_req_id.as_deref().unwrap_or_default();
        let model_id = request.inner.model.clone();

        // Drop process-local trace data.
        request.trace.take();

        tracing::debug!("create_message: {:?}", &request.inner);
        tracing::debug!("endpoint: {:?}", self.endpoint("messages"));

        let grok_headers = GrokRequestHeaders {
            conv_id: x_grok_conv_id,
            req_id: x_grok_req_id,
            model_id: &model_id,
            session_id: request.x_grok_session_id.as_deref().unwrap_or_default(),
            turn_idx: request.x_grok_turn_idx.as_deref(),
            agent_id: request.x_grok_agent_id.as_deref().unwrap_or_default(),
            deployment_id: request.x_grok_deployment_id.as_deref(),
            user_id: request.x_grok_user_id.as_deref(),
        };
        let mut request_body = serde_json::to_value(&request.inner).map_err(|e| {
            tracing::error!("Failed to serialize messages request: {}", e);
            SamplingError::Serialization(e)
        })?;
        self.inject_messages_extra_tools(&mut request.extra_raw_tools, &mut request_body);
        let mut headers = self.request_headers();
        self.apply_anthropic_cli_headers(&mut headers, &model_id);
        self.apply_command_adapter("messages", &model_id, &mut headers, &mut request_body)
            .await?;
        let sent_bearer = Self::sent_fragment_from_headers(&headers, &self.defaults.auth_scheme);
        // Same provider-rejection retry as the streaming path: strip the field
        // the provider named in a 400 and send once more without it. Fields
        // this model already rejected in this process are dropped up front.
        let mut stripped = remembered_rejected_fields(&model_id);
        for field in &stripped {
            strip_rejected_request_field(&mut request_body, field);
        }
        let bytes = loop {
            let serialized_body = self.serialize_messages_body(&request_body)?;
            let payload_stats = crate::auth_trace::log_request(
                "messages",
                x_grok_req_id,
                &model_id,
                &self.base_url,
                &headers,
                serialized_body.as_bytes(),
            );
            let http_request = grok_headers
                .apply(
                    self.http
                        .post(self.endpoint("messages"))
                        .headers(headers.clone()),
                )
                .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
                .body(serialized_body);

            let response = http_request.send().await.map_err(|e| {
                tracing::debug!("HTTP request failed: {}", e);
                e
            })?;

            let status = response.status();
            let model_metadata = extract_model_metadata(response.headers());
            let retry_after_secs = extract_retry_after(response.headers());
            let should_retry = extract_should_retry(response.headers());
            if status.is_success() {
                crate::auth_trace::log_success(
                    "messages",
                    x_grok_req_id,
                    &model_id,
                    status.as_u16(),
                    Some(&payload_stats),
                    response.headers(),
                );
            } else {
                crate::auth_trace::log_rejection(
                    "messages",
                    x_grok_req_id,
                    &model_id,
                    status.as_u16(),
                    Some(&payload_stats),
                    response.headers(),
                );
            }
            let bytes = response.bytes().await?;

            if status.is_success() {
                break bytes;
            }

            if status == reqwest::StatusCode::UNAUTHORIZED {
                self.record_401_attribution(
                    crate::attribution::SamplingConsumer::Messages,
                    sent_bearer.as_deref(),
                );
                let endpoint = self.endpoint("messages");
                let server_message = user_facing_api_error_message(status, bytes.as_ref());
                return Err(auth_rejected(
                    format!("Unauthorized (401) from {endpoint}: {server_message}"),
                    sent_bearer.as_deref(),
                ));
            }

            let server_message = parse_error_bytes(bytes.as_ref());

            if status == reqwest::StatusCode::BAD_REQUEST
                && let Some(field) = parse_rejected_request_field(&server_message)
                && !stripped.contains(&field)
                && strip_rejected_request_field(&mut request_body, &field)
            {
                tracing::warn!(
                    model_id = %model_id,
                    field = %field,
                    server_message = %server_message,
                    "provider rejected a request field; retrying once without it"
                );
                remember_rejected_field(&model_id, &field);
                stripped.push(field);
                continue;
            }

            let message = user_facing_api_error_message(status, bytes.as_ref());
            tracing::warn!(
                status = %status,
                error_message = %message,
                body_preview = %Self::body_preview(bytes.as_ref()),
                model_id = %model_id,
                "messages API error"
            );
            return Err(SamplingError::Api {
                status,
                message,
                model_metadata,
                retry_after_secs,
                should_retry,
            });
        };

        let bytes = self.adapt_messages_response_bytes(&bytes)?;
        let response_obj =
            serde_json::from_slice::<messages::MessagesResponse>(&bytes).map_err(|e| {
                let raw_body = String::from_utf8_lossy(&bytes);
                tracing::error!(
                    error = %e,
                    raw_body = %raw_body,
                    "Failed to deserialize MessagesResponse"
                );
                SamplingError::Serialization(e)
            })?;
        Ok(response_obj)
    }

    /// Create a streaming message using the Anthropic Messages API.
    ///
    /// Returns a stream of `MessageStreamEvent` which includes events like:
    /// - `message_start` - Initial message object
    /// - `content_block_start` / `content_block_delta` / `content_block_stop` - Content blocks
    /// - `message_delta` / `message_stop` - Final message with stop reason
    #[tracing::instrument(
        name = "http.create_message_stream",
        skip_all,
        fields(
            endpoint = %self.endpoint("messages"),
            model_id = request.inner.model.as_str(),
            status_code = tracing::field::Empty,
            success = tracing::field::Empty,
            error = tracing::field::Empty,
        )
    )]
    pub async fn create_message_stream(
        &self,
        mut request: MessagesRequestWrapper,
    ) -> Result<(
        BoxStream<'static, Result<messages::MessageStreamEvent>>,
        Option<ResponseModelMetadata>,
    )> {
        self.apply_message_defaults(&mut request)?;
        self.apply_messages_request_adapter(&mut request.inner);

        // Enable streaming
        request.inner.stream = Some(true);

        let x_grok_conv_id = request.x_grok_conv_id.as_deref().unwrap_or_default();
        let x_grok_req_id = request.x_grok_req_id.as_deref().unwrap_or_default();
        let model_id = request.inner.model.clone();

        // Drop process-local trace data.
        request.trace.take();

        tracing::debug!(
            base_url = %self.base_url,
            model_id = model_id.as_str(),
            "Sending Messages API stream request"
        );

        let grok_headers = GrokRequestHeaders {
            conv_id: x_grok_conv_id,
            req_id: x_grok_req_id,
            model_id: &model_id,
            session_id: request.x_grok_session_id.as_deref().unwrap_or_default(),
            turn_idx: request.x_grok_turn_idx.as_deref(),
            agent_id: request.x_grok_agent_id.as_deref().unwrap_or_default(),
            deployment_id: request.x_grok_deployment_id.as_deref(),
            user_id: request.x_grok_user_id.as_deref(),
        };
        let mut request_body = serde_json::to_value(&request.inner).map_err(|e| {
            tracing::error!("Failed to serialize messages stream request: {}", e);
            SamplingError::Serialization(e)
        })?;
        self.inject_messages_extra_tools(&mut request.extra_raw_tools, &mut request_body);
        let mut headers = self.request_headers();
        self.apply_anthropic_cli_headers(&mut headers, &model_id);
        self.apply_command_adapter("messages", &model_id, &mut headers, &mut request_body)
            .await?;
        let sent_bearer = Self::sent_fragment_from_headers(&headers, &self.defaults.auth_scheme);
        // Providers reject some fields deterministically: Anthropic 400s on an
        // explicit `temperature` for newer models, and on JSON Schema keywords
        // its structured-output validator does not implement. Those are
        // body-only failures, so strip the field the provider named and retry
        // once rather than failing every caller that sets it. Fields this model
        // already rejected in this process are dropped before the first send.
        let mut stripped = remembered_rejected_fields(&model_id);
        for field in &stripped {
            strip_rejected_request_field(&mut request_body, field);
        }
        let (response, status, payload_stats) = loop {
            let serialized_body = self.serialize_messages_body(&request_body)?;
            let payload_stats = crate::auth_trace::log_request(
                "messages_stream",
                x_grok_req_id,
                &model_id,
                &self.base_url,
                &headers,
                serialized_body.as_bytes(),
            );
            let http_request = grok_headers
                .apply(
                    self.http
                        .post(self.endpoint("messages"))
                        .headers(headers.clone()),
                )
                .header(ACCEPT, HeaderValue::from_static("text/event-stream"))
                .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
                .body(serialized_body);

            let built_request = http_request.build().map_err(|e| {
                tracing::error!("Failed to build HTTP request: {}", e);
                SamplingError::Http(e)
            })?;

            tracing::debug!(
                url = %built_request.url(),
                method = %built_request.method(),
                "Sending messages API stream request"
            );
            Self::log_request_headers(&built_request, "messages");

            let response = self.http.execute(built_request).await.map_err(|e| {
                tracing::debug!("HTTP request failed: {}", e);
                record_stream_request_failure(&e);
                e
            })?;

            let status = response.status();
            let span = tracing::Span::current();
            span.record("status_code", status.as_u16() as i64);
            span.record("success", status.is_success());
            if status.is_success() {
                break (response, status, payload_stats);
            }
            crate::auth_trace::log_rejection(
                "messages_stream",
                x_grok_req_id,
                &model_id,
                status.as_u16(),
                Some(&payload_stats),
                response.headers(),
            );
            if status == reqwest::StatusCode::UNAUTHORIZED {
                span.record("error", "unauthorized (401)");
                self.record_401_attribution(
                    crate::attribution::SamplingConsumer::MessagesStream,
                    sent_bearer.as_deref(),
                );
                let endpoint = self.endpoint("messages");
                let body = response.bytes().await.unwrap_or_default();
                let server_message = user_facing_api_error_message(status, body.as_ref());
                return Err(auth_rejected(
                    format!("Unauthorized (401) from {endpoint}: {server_message}"),
                    sent_bearer.as_deref(),
                ));
            }
            let model_metadata = extract_model_metadata(response.headers());
            let retry_after_secs = extract_retry_after(response.headers());
            let should_retry = extract_should_retry(response.headers());
            let bytes = response.bytes().await?;
            let server_message = parse_error_bytes(bytes.as_ref());

            if status == reqwest::StatusCode::BAD_REQUEST
                && let Some(field) = parse_rejected_request_field(&server_message)
                && !stripped.contains(&field)
                && strip_rejected_request_field(&mut request_body, &field)
            {
                tracing::warn!(
                    model_id = %model_id,
                    field = %field,
                    server_message = %server_message,
                    "provider rejected a request field; retrying once without it"
                );
                remember_rejected_field(&model_id, &field);
                stripped.push(field);
                continue;
            }

            let message = user_facing_api_error_message(status, bytes.as_ref());

            span.record("error", message.as_str());
            tracing::error!(
                status = %status,
                error_message = %message,
                body_preview = %Self::body_preview(bytes.as_ref()),
                model_id = %model_id,
                "messages API error"
            );
            return Err(SamplingError::Api {
                status,
                message,
                model_metadata,
                retry_after_secs,
                should_retry,
            });
        };

        crate::auth_trace::log_success(
            "messages_stream",
            x_grok_req_id,
            &model_id,
            status.as_u16(),
            Some(&payload_stats),
            response.headers(),
        );

        let model_metadata = extract_model_metadata(response.headers());

        // Strip UTF-8 BOM if present
        const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];
        let mut is_first = true;
        let byte_stream = response.bytes_stream().map(move |result| {
            result.map(|bytes| {
                if is_first {
                    is_first = false;
                    if bytes.starts_with(UTF8_BOM) {
                        return bytes.slice(UTF8_BOM.len()..);
                    }
                }
                bytes
            })
        });

        // Turn raw bytes into SSE events
        let event_stream = byte_stream.eventsource();

        // Map SSE events into MessageStreamEvent.
        // Uses `scan` so transport errors terminate the stream after the first
        // error (same pattern as `chat_completion_stream`).
        let provider_request_adapter = self.defaults.provider_request_adapter.clone();
        let events = event_stream
            .scan(false, move |had_transport_error, event_res| {
                if *had_transport_error {
                    return std::future::ready(None);
                }
                let item = match event_res {
                    Ok(event) => {
                        let data = adapt_messages_event_json(
                            provider_request_adapter.as_ref(),
                            &event.data,
                        );
                        if data == "[DONE]" {
                            return std::future::ready(None);
                        }

                        tracing::info!(
                            target: crate::sampling_log::TARGET,
                            event = "sse_chunk",
                            backend = "messages",
                            data = %data,
                        );

                        if let Some(stream_error) = try_parse_stream_error(&data) {
                            Some(Err(stream_error))
                        } else {
                            Some(
                                serde_json::from_str::<messages::MessageStreamEvent>(&data)
                                    .map_err(|e| {
                                        tracing::error!(
                                            error = %e,
                                            raw_data = %data,
                                            "Failed to deserialize MessageStreamEvent from stream"
                                        );
                                        SamplingError::Serialization(e)
                                    }),
                            )
                        }
                    }
                    Err(e) => {
                        *had_transport_error = true;
                        Some(Err(SamplingError::EventStreamError(e.to_string())))
                    }
                };
                std::future::ready(item)
            })
            .boxed();

        Ok((events, model_metadata))
    }

    // =========================================================================
    // Unified Conversation API
    // =========================================================================

    /// Apply default configuration to a ConversationRequest.
    fn apply_conversation_defaults(&self, request: &mut ConversationRequest) -> Result<()> {
        if request.model.is_none() {
            request.model = Some(self.defaults.model.clone());
        }

        if request.temperature.is_none() {
            request.temperature = self.defaults.temperature;
        }

        if request.top_p.is_none() {
            request.top_p = self.defaults.top_p;
        }

        if request.max_output_tokens.is_none() {
            request.max_output_tokens = self.defaults.max_completion_tokens;
        }

        Ok(())
    }

    /// Send a conversation request using the Chat Completions API (streaming).
    ///
    /// Converts the `ConversationRequest` to `ChatCompletionRequest` internally.
    /// Returns the stream and any model metadata extracted from response headers.
    pub async fn conversation_stream(
        &self,
        mut request: ConversationRequest,
    ) -> Result<(
        BoxStream<'static, Result<ChatCompletionChunk>>,
        Option<ResponseModelMetadata>,
    )> {
        self.apply_conversation_defaults(&mut request)?;

        let trace = request.trace.take();
        let mut chat_request: ChatCompletionRequest = request.into();
        if let Some(trace) = trace {
            chat_request.trace = Some(trace);
        }

        self.chat_completion_stream(chat_request).await
    }

    /// Send a conversation request using the Chat Completions API (non-streaming).
    ///
    /// Converts the `ConversationRequest` to `ChatCompletionRequest` internally.
    pub async fn conversation(
        &self,
        mut request: ConversationRequest,
    ) -> Result<ChatCompletionResponse> {
        self.apply_conversation_defaults(&mut request)?;

        let trace = request.trace.take();
        let mut chat_request: ChatCompletionRequest = request.into();
        if let Some(trace) = trace {
            chat_request.trace = Some(trace);
        }

        self.chat_completion(chat_request).await
    }

    /// Send a conversation request using the Responses API (streaming).
    ///
    /// Converts the `ConversationRequest` to Responses API format internally.
    /// The third tuple element is the per-request doom-loop signal collector
    /// (see [`Self::create_response_stream`]); callers that don't consume the
    /// signals can ignore it.
    #[allow(clippy::type_complexity)]
    pub async fn conversation_stream_responses(
        &self,
        mut request: ConversationRequest,
    ) -> Result<(
        BoxStream<'static, Result<rs::ResponseStreamEvent>>,
        Option<ResponseModelMetadata>,
        Option<crate::doom_loop::DoomLoopSignalCollector>,
    )> {
        self.apply_conversation_defaults(&mut request)?;

        let trace = request.trace.take();
        let x_grok_conv_id = request.x_grok_conv_id.clone();
        let x_grok_req_id = request.x_grok_req_id.clone();
        let x_grok_session_id = request.x_grok_session_id.clone();
        let x_grok_turn_idx = request.x_grok_turn_idx.clone();
        let x_grok_agent_id = request.x_grok_agent_id.clone();

        // Collect xAI-specific tools that can't be expressed via rs::Tool
        // (e.g., x_search). These are injected as raw JSON after serialization.
        let extra_tools = xai_grok_sampling_types::extra_tool_entries(&request.hosted_tools);

        let responses_request: rs::CreateResponse = (&request).into();

        let mut wrapper = CreateResponseWrapper::new(responses_request);
        wrapper.x_grok_conv_id = x_grok_conv_id;
        wrapper.x_grok_req_id = x_grok_req_id;
        wrapper.x_grok_session_id = x_grok_session_id;
        wrapper.x_grok_turn_idx = x_grok_turn_idx;
        wrapper.x_grok_agent_id = x_grok_agent_id;
        wrapper.extra_tool_entries = extra_tools;

        if let Some(trace) = trace {
            wrapper.trace = Some(trace);
        }

        self.create_response_stream(wrapper).await
    }

    /// Send a conversation request using the Responses API (non-streaming).
    ///
    /// Converts the `ConversationRequest` to Responses API format internally.
    pub async fn conversation_responses(
        &self,
        mut request: ConversationRequest,
    ) -> Result<rs::Response> {
        self.apply_conversation_defaults(&mut request)?;

        let trace = request.trace.take();
        let x_grok_conv_id = request.x_grok_conv_id.clone();
        let x_grok_req_id = request.x_grok_req_id.clone();
        let x_grok_session_id = request.x_grok_session_id.clone();
        let x_grok_turn_idx = request.x_grok_turn_idx.clone();
        let x_grok_agent_id = request.x_grok_agent_id.clone();

        let responses_request: rs::CreateResponse = (&request).into();

        let mut wrapper = CreateResponseWrapper::new(responses_request);
        wrapper.x_grok_conv_id = x_grok_conv_id;
        wrapper.x_grok_req_id = x_grok_req_id;
        wrapper.x_grok_session_id = x_grok_session_id;
        wrapper.x_grok_turn_idx = x_grok_turn_idx;
        wrapper.x_grok_agent_id = x_grok_agent_id;

        if let Some(trace) = trace {
            wrapper.trace = Some(trace);
        }

        self.create_response(wrapper).await
    }

    /// Send a conversation request using the Anthropic Messages API (streaming).
    ///
    /// Converts the `ConversationRequest` to Messages API format internally.
    pub async fn conversation_stream_messages(
        &self,
        mut request: ConversationRequest,
    ) -> Result<(
        BoxStream<'static, Result<messages::MessageStreamEvent>>,
        Option<ResponseModelMetadata>,
    )> {
        self.apply_conversation_defaults(&mut request)?;

        let trace = request.trace.take();
        let x_grok_conv_id = request.x_grok_conv_id.clone();
        let x_grok_req_id = request.x_grok_req_id.clone();
        let x_grok_session_id = request.x_grok_session_id.clone();
        let x_grok_turn_idx = request.x_grok_turn_idx.clone();
        let x_grok_agent_id = request.x_grok_agent_id.clone();

        let messages_request = build_messages_request(&request);

        let mut wrapper = MessagesRequestWrapper::new(messages_request);
        wrapper.x_grok_conv_id = x_grok_conv_id;
        wrapper.x_grok_req_id = x_grok_req_id;
        wrapper.x_grok_session_id = x_grok_session_id;
        wrapper.x_grok_turn_idx = x_grok_turn_idx;
        wrapper.x_grok_agent_id = x_grok_agent_id;

        if let Some(trace) = trace {
            wrapper.trace = Some(trace);
        }

        self.create_message_stream(wrapper).await
    }

    /// Send a conversation request using the Anthropic Messages API (non-streaming).
    ///
    /// Converts the `ConversationRequest` to Messages API format internally.
    pub async fn conversation_messages(
        &self,
        mut request: ConversationRequest,
    ) -> Result<messages::MessagesResponse> {
        self.apply_conversation_defaults(&mut request)?;

        let trace = request.trace.take();
        let x_grok_conv_id = request.x_grok_conv_id.clone();
        let x_grok_req_id = request.x_grok_req_id.clone();
        let x_grok_session_id = request.x_grok_session_id.clone();
        let x_grok_turn_idx = request.x_grok_turn_idx.clone();
        let x_grok_agent_id = request.x_grok_agent_id.clone();

        let messages_request = build_messages_request(&request);

        let mut wrapper = MessagesRequestWrapper::new(messages_request);
        wrapper.x_grok_conv_id = x_grok_conv_id;
        wrapper.x_grok_req_id = x_grok_req_id;
        wrapper.x_grok_session_id = x_grok_session_id;
        wrapper.x_grok_turn_idx = x_grok_turn_idx;
        wrapper.x_grok_agent_id = x_grok_agent_id;

        if let Some(trace) = trace {
            wrapper.trace = Some(trace);
        }

        self.create_message(wrapper).await
    }

    /// Backend-aware streaming call that collects the full response.
    pub async fn conversation_collect(
        &self,
        request: ConversationRequest,
    ) -> Result<ConversationResponse> {
        let request_id = crate::types::RequestId::random();
        let idle_timeout = std::time::Duration::from_secs(300);
        let result = match self.api_backend() {
            ApiBackend::ChatCompletions => {
                let (raw, meta) = self.conversation_stream(request).await?;
                let events =
                    crate::stream::stream_chat_completions(raw, meta, request_id, idle_timeout);
                crate::stream::collect_response(events).await
            }
            ApiBackend::Responses => {
                let (raw, meta, doom_loop) = self.conversation_stream_responses(request).await?;
                let events =
                    crate::stream::stream_responses(raw, meta, request_id, idle_timeout, doom_loop);
                crate::stream::collect_response(events).await
            }
            ApiBackend::Messages => {
                let (raw, meta) = self.conversation_stream_messages(request).await?;
                let events = crate::stream::stream_messages(raw, meta, request_id, idle_timeout);
                crate::stream::collect_response(events).await
            }
        };
        result
            .map(|(response, _metrics)| response)
            .map_err(|info| SamplingError::Api {
                status: info
                    .status_code
                    .and_then(|c| reqwest::StatusCode::from_u16(c).ok())
                    .unwrap_or(reqwest::StatusCode::INTERNAL_SERVER_ERROR),
                message: info.message,
                model_metadata: info.model_metadata,
                retry_after_secs: info.retry_after_secs,
                should_retry: None,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;
    use xai_grok_sampling_types::messages;
    use xai_grok_sampling_types::types::ChatRequestMessage;

    /// Anthropic 400s on an explicit `temperature` for newer models. The root
    /// parameter it names must be recognized and removed so the retry differs.
    #[test]
    fn deprecated_root_parameter_is_parsed_and_stripped() {
        let field = parse_rejected_request_field("`temperature` is deprecated for this model.")
            .expect("deprecated parameter should be recognized");
        assert_eq!(field.path, "");
        assert_eq!(field.key, "temperature");
        assert_eq!(field.to_string(), "temperature");

        let mut body = serde_json::json!({
            "model": "claude-opus-4-8",
            "temperature": 0.0,
            "max_tokens": 2048
        });
        assert!(strip_rejected_request_field(&mut body, &field));
        assert!(body.get("temperature").is_none());
        assert_eq!(body["max_tokens"], 2048);
        // Second pass changes nothing, so the caller must not retry forever.
        assert!(!strip_rejected_request_field(&mut body, &field));
    }

    /// The structured-output validator rejects schema keywords by path. The
    /// keyword must be removed at every depth under that path, and only there.
    #[test]
    fn unsupported_schema_keyword_is_stripped_under_its_path_only() {
        let field = parse_rejected_request_field(
            "output_config.format.schema: For 'array' type, property 'maxItems' is not supported",
        )
        .expect("unsupported schema keyword should be recognized");
        assert_eq!(field.path, "output_config.format.schema");
        assert_eq!(field.key, "maxItems");
        assert_eq!(field.to_string(), "output_config.format.schema.maxItems");

        let mut body = serde_json::json!({
            "maxItems": 3,
            "output_config": {
                "format": {
                    "schema": {
                        "type": "object",
                        "properties": {
                            "pages": {
                                "type": "array",
                                "maxItems": 12,
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "sources": { "type": "array", "maxItems": 4 }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });
        assert!(strip_rejected_request_field(&mut body, &field));
        let schema = &body["output_config"]["format"]["schema"];
        assert!(schema["properties"]["pages"].get("maxItems").is_none());
        assert!(
            schema["properties"]["pages"]["items"]["properties"]["sources"]
                .get("maxItems")
                .is_none(),
            "nested keyword must be stripped too: {schema}"
        );
        assert_eq!(schema["properties"]["pages"]["type"], "array");
        // A same-named key outside the named path is untouched.
        assert_eq!(body["maxItems"], 3);
    }

    /// Everything else must fail closed: a real model error must surface as an
    /// error instead of silently mutating the request and retrying.
    #[test]
    fn unrelated_errors_do_not_trigger_a_retry() {
        for message in [
            "invalid_request_error: credit balance is too low",
            "overloaded_error: server is overloaded",
            "messages.0.content: expected an array",
        ] {
            assert_eq!(
                parse_rejected_request_field(message),
                None,
                "must not strip anything for: {message}"
            );
        }
    }

    /// A named field that is not in the body must not trigger a retry: the
    /// resent request would be byte-identical and loop.
    #[test]
    fn absent_field_reports_no_change() {
        let field = parse_rejected_request_field("`top_k` is deprecated for this model.")
            .expect("deprecated parameter should be recognized");
        let mut body = serde_json::json!({ "model": "claude-opus-4-8" });
        assert!(!strip_rejected_request_field(&mut body, &field));
    }

    /// The rejection memo makes the fix cost one round-trip per model per
    /// process: a later request for the same model drops the field up front.
    #[test]
    fn rejected_field_is_remembered_per_model() {
        let field = parse_rejected_request_field("`temperature` is deprecated for this model.")
            .expect("deprecated parameter should be recognized");
        let model = "test-model-remembers-temperature";
        assert!(remembered_rejected_fields(model).is_empty());

        remember_rejected_field(model, &field);
        remember_rejected_field(model, &field);
        assert_eq!(remembered_rejected_fields(model), vec![field.clone()]);
        assert!(
            remembered_rejected_fields("test-model-unaffected").is_empty(),
            "the memo must be per model"
        );

        let mut body = serde_json::json!({ "temperature": 0.0, "max_tokens": 8 });
        for remembered in remembered_rejected_fields(model) {
            strip_rejected_request_field(&mut body, &remembered);
        }
        assert!(body.get("temperature").is_none());
        assert_eq!(body["max_tokens"], 8);
    }

    fn minimal_config() -> SamplerConfig {
        SamplerConfig {
            api_key: Some("test-key".to_string()),
            base_url: "https://example.test".to_string(),
            model: "test-model".to_string(),
            max_completion_tokens: None,
            temperature: None,
            top_p: None,
            api_backend: ApiBackend::ChatCompletions,
            auth_scheme: AuthScheme::Bearer,
            extra_headers: IndexMap::new(),
            query_params: IndexMap::new(),
            env_http_headers: IndexMap::new(),
            context_window: 8192,
            force_http1: false,
            max_retries: None,
            stream_tool_calls: false,
            idle_timeout_secs: None,
            reasoning_effort: None,
            origin_client: None,
            client_identifier: None,
            deployment_id: None,
            user_id: None,
            client_version: None,
            attribution_callback: None,
            bearer_resolver: None,
            supports_backend_search: false,
            compactions_remaining: None,
            compaction_at_tokens: None,
            doom_loop_recovery: None,
            provider_request_adapter: None,
            advisor_server_model: None,
            header_injector: None,
        }
    }

    #[test]
    fn anthropic_adapter_prefixes_request_tools_and_strips_response_names() {
        let mut cfg = minimal_config();
        cfg.api_backend = ApiBackend::Messages;
        cfg.provider_request_adapter = Some(ProviderRequestAdapter::Anthropic {
            tool_name_prefix: "mcp__".to_string(),
            command: None,
        });
        let client = SamplingClient::new(cfg).expect("client");
        let mut request = messages::MessagesRequest {
            tools: Some(vec![
                messages::ToolParam {
                    name: "bash".to_string(),
                    description: None,
                    input_schema: serde_json::json!({"type":"object"}),
                },
                messages::ToolParam {
                    name: "ide:execute".to_string(),
                    description: None,
                    input_schema: serde_json::json!({"type":"object"}),
                },
            ]),
            ..Default::default()
        };

        client.apply_messages_request_adapter(&mut request);
        let names: Vec<_> = request
            .tools
            .as_ref()
            .unwrap()
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        assert_eq!(names, vec!["mcp__grok__bash", "mcp__ide__execute"]);

        let data = serde_json::json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {
                "type": "tool_use",
                "id": "toolu_1",
                "name": "mcp__grok__bash",
                "input": {}
            }
        })
        .to_string();
        let decoded = client.adapt_messages_event_json(&data);
        let event: messages::MessageStreamEvent =
            serde_json::from_str(&decoded).expect("decoded event");
        match event {
            messages::MessageStreamEvent::ContentBlockStart {
                content_block: messages::ContentBlock::ToolUse { name, .. },
                ..
            } => assert_eq!(name, "bash"),
            other => panic!("unexpected event: {other:?}"),
        }
    }

    fn thinking_block(thinking: &str, signature: &str) -> messages::ContentBlock {
        messages::ContentBlock::Thinking {
            thinking: thinking.to_owned(),
            signature: signature.to_owned(),
        }
    }

    fn tool_use_block(id: &str) -> messages::ContentBlock {
        messages::ContentBlock::ToolUse {
            id: id.to_owned(),
            name: "bash".to_owned(),
            input: serde_json::json!({}),
            cache_control: None,
        }
    }

    fn assistant_blocks(blocks: Vec<messages::ContentBlock>) -> messages::Message {
        messages::Message {
            role: messages::MessageRole::Assistant,
            content: messages::MessageContent::Blocks(blocks),
        }
    }

    fn user_text(text: &str) -> messages::Message {
        messages::Message {
            role: messages::MessageRole::User,
            content: messages::MessageContent::Text(text.to_owned()),
        }
    }

    fn block_types(message: &messages::Message) -> Vec<&'static str> {
        match &message.content {
            messages::MessageContent::Text(_) => vec!["text"],
            messages::MessageContent::Blocks(blocks) => blocks
                .iter()
                .map(|block| match block {
                    messages::ContentBlock::Text { .. } => "text",
                    messages::ContentBlock::Image { .. } => "image",
                    messages::ContentBlock::ToolUse { .. } => "tool_use",
                    messages::ContentBlock::ToolResult { .. } => "tool_result",
                    messages::ContentBlock::Thinking { .. } => "thinking",
                    messages::ContentBlock::RedactedThinking { .. } => "redacted_thinking",
                })
                .collect(),
        }
    }

    /// Regression: api.anthropic.com validates the signature of every replayed
    /// `thinking` block and rejects the whole request with
    /// `messages.N.content.0: Invalid signature in thinking block` (seen after
    /// a failed compaction retry). Prior-turn thinking must be stripped; only
    /// the final assistant message of an in-flight tool loop keeps its
    /// (plausibly valid) thinking.
    #[test]
    fn anthropic_adapter_strips_prior_turn_thinking_keeps_final_tool_loop_thinking() {
        let mut cfg = minimal_config();
        cfg.api_backend = ApiBackend::Messages;
        cfg.provider_request_adapter = Some(ProviderRequestAdapter::Anthropic {
            tool_name_prefix: "mcp__".to_string(),
            command: None,
        });
        let client = SamplingClient::new(cfg).expect("client");

        let mut request = messages::MessagesRequest {
            thinking: Some(messages::ThinkingConfig::Adaptive {
                display: Some(messages::ThinkingDisplay::Summarized),
            }),
            messages: vec![
                user_text("do the thing"),
                // Prior turn: thinking must be stripped even though signed.
                assistant_blocks(vec![
                    thinking_block("old reasoning", "sig_old"),
                    tool_use_block("toolu_1"),
                ]),
                user_text("tool result 1"),
                // Final assistant turn of the tool loop: valid thinking kept.
                assistant_blocks(vec![
                    thinking_block("fresh reasoning", "sig_fresh"),
                    tool_use_block("toolu_2"),
                ]),
                user_text("tool result 2"),
            ],
            ..Default::default()
        };

        client.apply_messages_request_adapter(&mut request);

        assert_eq!(block_types(&request.messages[1]), vec!["tool_use"]);
        assert_eq!(
            block_types(&request.messages[3]),
            vec!["thinking", "tool_use"],
            "final assistant turn must keep its signed thinking for tool-loop continuation",
        );
    }

    /// Signature-only blobs (`tco_*` cross-provider reasoning) and unsigned
    /// thinking can never validate at api.anthropic.com — drop them even on
    /// the final assistant message.
    #[test]
    fn anthropic_adapter_drops_implausible_final_thinking() {
        let mut cfg = minimal_config();
        cfg.api_backend = ApiBackend::Messages;
        cfg.provider_request_adapter = Some(ProviderRequestAdapter::Anthropic {
            tool_name_prefix: "mcp__".to_string(),
            command: None,
        });
        let client = SamplingClient::new(cfg).expect("client");

        let mut request = messages::MessagesRequest {
            thinking: Some(messages::ThinkingConfig::Adaptive { display: None }),
            messages: vec![
                user_text("go"),
                assistant_blocks(vec![
                    // Signature-only blob (e.g. cross-provider encrypted reasoning).
                    thinking_block("", "tco_blob"),
                    // Unsigned thinking text (e.g. non-Anthropic model output).
                    thinking_block("unsigned", ""),
                    tool_use_block("toolu_1"),
                ]),
                user_text("tool result"),
            ],
            ..Default::default()
        };

        client.apply_messages_request_adapter(&mut request);

        assert_eq!(block_types(&request.messages[1]), vec!["tool_use"]);
    }

    /// Thinking on a final assistant message *without* tool use is not needed
    /// for continuation; replaying it is pure signature-validation risk.
    #[test]
    fn anthropic_adapter_strips_final_thinking_without_tool_use() {
        let mut cfg = minimal_config();
        cfg.api_backend = ApiBackend::Messages;
        cfg.provider_request_adapter = Some(ProviderRequestAdapter::Anthropic {
            tool_name_prefix: "mcp__".to_string(),
            command: None,
        });
        let client = SamplingClient::new(cfg).expect("client");

        let mut request = messages::MessagesRequest {
            thinking: Some(messages::ThinkingConfig::Adaptive { display: None }),
            messages: vec![
                user_text("hi"),
                assistant_blocks(vec![
                    thinking_block("pondering", "sig"),
                    messages::ContentBlock::Text {
                        text: "answer".to_owned(),
                        cache_control: None,
                    },
                ]),
                user_text("follow-up"),
            ],
            ..Default::default()
        };

        client.apply_messages_request_adapter(&mut request);

        assert_eq!(block_types(&request.messages[1]), vec!["text"]);
    }

    /// A thinking-only assistant message (aborted turn) must be removed
    /// entirely once its thinking is stripped: the API rejects messages with
    /// empty content arrays.
    #[test]
    fn anthropic_adapter_removes_thinking_only_assistant_messages() {
        let mut cfg = minimal_config();
        cfg.api_backend = ApiBackend::Messages;
        cfg.provider_request_adapter = Some(ProviderRequestAdapter::Anthropic {
            tool_name_prefix: "mcp__".to_string(),
            command: None,
        });
        let client = SamplingClient::new(cfg).expect("client");

        let mut request = messages::MessagesRequest {
            thinking: Some(messages::ThinkingConfig::Adaptive { display: None }),
            messages: vec![
                user_text("hi"),
                assistant_blocks(vec![thinking_block("aborted turn", "sig")]),
                user_text("continue"),
            ],
            ..Default::default()
        };

        client.apply_messages_request_adapter(&mut request);

        assert_eq!(request.messages.len(), 2);
        assert!(matches!(
            request.messages[0].role,
            messages::MessageRole::User
        ));
        assert!(matches!(
            request.messages[1].role,
            messages::MessageRole::User
        ));
    }

    /// When the request has no `thinking` config, replayed thinking blocks are
    /// rejected outright by the Messages API — strip them everywhere.
    #[test]
    fn anthropic_adapter_strips_all_thinking_when_disabled() {
        let mut cfg = minimal_config();
        cfg.api_backend = ApiBackend::Messages;
        cfg.provider_request_adapter = Some(ProviderRequestAdapter::Anthropic {
            tool_name_prefix: "mcp__".to_string(),
            command: None,
        });
        let client = SamplingClient::new(cfg).expect("client");

        let mut request = messages::MessagesRequest {
            thinking: None,
            messages: vec![
                user_text("go"),
                assistant_blocks(vec![
                    thinking_block("reasoning", "sig"),
                    tool_use_block("toolu_1"),
                ]),
                user_text("tool result"),
            ],
            ..Default::default()
        };

        client.apply_messages_request_adapter(&mut request);

        assert_eq!(block_types(&request.messages[1]), vec!["tool_use"]);
    }

    /// The xAI Messages backend (no Anthropic adapter) must keep replaying
    /// reasoning verbatim — `tco_*` blobs and prior-turn thinking included.
    #[test]
    fn non_anthropic_backend_keeps_thinking_blocks_verbatim() {
        let mut cfg = minimal_config();
        cfg.api_backend = ApiBackend::Messages;
        let client = SamplingClient::new(cfg).expect("client");

        let mut request = messages::MessagesRequest {
            thinking: Some(messages::ThinkingConfig::Adaptive { display: None }),
            messages: vec![
                user_text("go"),
                assistant_blocks(vec![thinking_block("", "tco_blob"), tool_use_block("t1")]),
                user_text("tool result"),
                assistant_blocks(vec![thinking_block("later", "sig")]),
            ],
            ..Default::default()
        };

        client.apply_messages_request_adapter(&mut request);

        assert_eq!(
            block_types(&request.messages[1]),
            vec!["thinking", "tool_use"]
        );
        assert_eq!(block_types(&request.messages[3]), vec!["thinking"]);
    }

    #[test]
    fn anthropic_adapter_injects_billing_identity_and_cch() {
        let mut cfg = minimal_config();
        cfg.api_backend = ApiBackend::Messages;
        cfg.provider_request_adapter = Some(ProviderRequestAdapter::Anthropic {
            tool_name_prefix: "mcp__".to_string(),
            command: None,
        });
        let client = SamplingClient::new(cfg).expect("client");
        let mut request = messages::MessagesRequest {
            model: "claude-sonnet-5".to_owned(),
            messages: vec![messages::Message {
                role: messages::MessageRole::User,
                content: messages::MessageContent::Text("hello from test".to_owned()),
            }],
            max_tokens: 32,
            ..Default::default()
        };

        client.apply_messages_request_adapter(&mut request);
        let body = client
            .serialize_messages_body(&serde_json::to_value(&request).unwrap())
            .unwrap();

        assert!(body.contains("x-anthropic-billing-header:"));
        assert!(body.contains(CLAUDE_CODE_IDENTITY));
        assert!(!body.contains(CLAUDE_CCH_PLACEHOLDER));
        assert!(body.contains("\"system\":["));
        assert!(body.contains("cch="));
        let cch = body
            .split("cch=")
            .nth(1)
            .unwrap()
            .chars()
            .take(5)
            .collect::<String>();
        assert_eq!(cch.len(), 5);
        assert!(cch.chars().all(|ch| ch.is_ascii_hexdigit()));
    }

    #[test]
    fn anthropic_headers_skip_context_1m_for_200k_context() {
        let mut cfg = minimal_config();
        cfg.api_backend = ApiBackend::Messages;
        cfg.context_window = 200_000;
        cfg.provider_request_adapter = Some(ProviderRequestAdapter::Anthropic {
            tool_name_prefix: "mcp__".to_string(),
            command: None,
        });
        let client = SamplingClient::new(cfg).expect("client");
        let mut headers = HeaderMap::new();

        client.apply_anthropic_cli_headers(&mut headers, "claude-haiku-4-5");

        let betas = headers
            .get("anthropic-beta")
            .and_then(|value| value.to_str().ok())
            .unwrap();
        assert!(betas.contains("claude-code-20250219"));
        assert!(!betas.contains("context-1m-2025-08-07"));
    }

    #[test]
    fn anthropic_headers_include_context_1m_for_1m_context() {
        let mut cfg = minimal_config();
        cfg.api_backend = ApiBackend::Messages;
        cfg.context_window = 1_000_000;
        cfg.provider_request_adapter = Some(ProviderRequestAdapter::Anthropic {
            tool_name_prefix: "mcp__".to_string(),
            command: None,
        });
        let client = SamplingClient::new(cfg).expect("client");
        let mut headers = HeaderMap::new();

        client.apply_anthropic_cli_headers(&mut headers, "claude-sonnet-5");

        let betas = headers
            .get("anthropic-beta")
            .and_then(|value| value.to_str().ok())
            .unwrap();
        assert!(betas.contains("context-1m-2025-08-07"));
    }
    /// Builds a client for the advisor matrix with the given axis values and
    /// returns whether the beta header and the raw `advisor_20260301` tool
    /// are both present (they are wired off the same gate, so either both
    /// appear or neither does).
    fn advisor_probe(
        api_backend: ApiBackend,
        auth_scheme: AuthScheme,
        anthropic_adapter: bool,
        advisor_server_model: Option<&str>,
    ) -> (bool, bool) {
        let mut cfg = minimal_config();
        cfg.api_backend = api_backend;
        cfg.auth_scheme = auth_scheme;
        cfg.provider_request_adapter = if anthropic_adapter {
            Some(ProviderRequestAdapter::Anthropic {
                tool_name_prefix: "mcp__".to_string(),
                command: None,
            })
        } else {
            None
        };
        cfg.advisor_server_model = advisor_server_model.map(str::to_string);
        let client = SamplingClient::new(cfg).expect("client should build");

        let mut headers = HeaderMap::new();
        client.apply_anthropic_cli_headers(&mut headers, "claude-sonnet-5");
        let beta_present = headers
            .get("anthropic-beta")
            .and_then(|v| v.to_str().ok())
            .map(|betas| betas.contains("advisor-tool-2026-03-01"))
            .unwrap_or(false);

        let mut extra_raw_tools = Vec::new();
        let mut body = serde_json::json!({});
        client.inject_messages_extra_tools(&mut extra_raw_tools, &mut body);
        let tool_present = body
            .get("tools")
            .and_then(|t| t.as_array())
            .map(|tools| {
                tools.iter().any(|t| {
                    t.get("type").and_then(|v| v.as_str()) == Some("advisor_20260301")
                        && t.get("name").and_then(|v| v.as_str()) == Some("advisor")
                })
            })
            .unwrap_or(false);

        (beta_present, tool_present)
    }

    #[test]
    fn advisor_present_when_messages_anthropic_bearer_and_model_set() {
        let (beta, tool) = advisor_probe(
            ApiBackend::Messages,
            AuthScheme::Bearer,
            true,
            Some("claude-opus-5"),
        );
        assert!(beta, "expected advisor beta present in the positive case");
        assert!(tool, "expected advisor tool present in the positive case");
    }

    #[test]
    fn advisor_absent_for_responses_backend() {
        let (beta, tool) = advisor_probe(
            ApiBackend::Responses,
            AuthScheme::Bearer,
            true,
            Some("claude-opus-5"),
        );
        assert!(!beta, "Responses backend must not get the advisor beta");
        assert!(!tool, "Responses backend must not get the advisor tool");
    }

    #[test]
    fn advisor_absent_for_chat_completions_backend() {
        let (beta, tool) = advisor_probe(
            ApiBackend::ChatCompletions,
            AuthScheme::Bearer,
            true,
            Some("claude-opus-5"),
        );
        assert!(
            !beta,
            "ChatCompletions backend must not get the advisor beta"
        );
        assert!(
            !tool,
            "ChatCompletions backend must not get the advisor tool"
        );
    }

    #[test]
    fn advisor_absent_for_x_api_key_scheme() {
        let (beta, tool) = advisor_probe(
            ApiBackend::Messages,
            AuthScheme::XApiKey,
            true,
            Some("claude-opus-5"),
        );
        assert!(!beta, "XApiKey scheme must not get the advisor beta");
        assert!(!tool, "XApiKey scheme must not get the advisor tool");
    }

    #[test]
    fn advisor_absent_for_non_anthropic_adapter() {
        let (beta, tool) = advisor_probe(
            ApiBackend::Messages,
            AuthScheme::Bearer,
            false,
            Some("claude-opus-5"),
        );
        assert!(!beta, "non-Anthropic adapter must not get the advisor beta");
        assert!(!tool, "non-Anthropic adapter must not get the advisor tool");
    }

    #[test]
    fn advisor_absent_when_model_none() {
        let (beta, tool) = advisor_probe(ApiBackend::Messages, AuthScheme::Bearer, true, None);
        assert!(!beta, "advisor_server_model=None must not get the beta");
        assert!(!tool, "advisor_server_model=None must not get the tool");
    }

    #[test]
    fn advisor_matrix_cartesian_sweep() {
        let backends = [
            ApiBackend::Messages,
            ApiBackend::Responses,
            ApiBackend::ChatCompletions,
        ];
        let auth_schemes = [AuthScheme::Bearer, AuthScheme::XApiKey];
        let adapters = [true, false];
        let models: [Option<&str>; 2] = [Some("claude-opus-5"), None];

        for backend in backends {
            for auth in auth_schemes {
                for adapter in adapters {
                    for model in models {
                        let expected = backend == ApiBackend::Messages
                            && auth == AuthScheme::Bearer
                            && adapter
                            && model.is_some();
                        let (beta, tool) = advisor_probe(backend.clone(), auth, adapter, model);
                        assert_eq!(
                            beta, expected,
                            "beta mismatch for backend={backend:?} auth={auth:?} adapter={adapter} model={model:?}"
                        );
                        assert_eq!(
                            tool, expected,
                            "tool mismatch for backend={backend:?} auth={auth:?} adapter={adapter} model={model:?}"
                        );
                    }
                }
            }
        }
    }
    /// Dedicated coupling invariant (independent of whether the gate formula
    /// itself is correct): across the full axis matrix, the advisor beta and
    /// the advisor tool must never diverge -- either both present or both
    /// absent, on every combination. A regression that wires one without the
    /// other (e.g. a beta-filter edit that forgets the tool injection, or
    /// vice versa) would pass `advisor_matrix_cartesian_sweep`'s per-field
    /// `expected` comparison only if it broke both identically; this test
    /// catches an asymmetric break directly.
    #[test]
    fn advisor_beta_tool_never_diverge_across_matrix() {
        let backends = [
            ApiBackend::Messages,
            ApiBackend::Responses,
            ApiBackend::ChatCompletions,
        ];
        let auth_schemes = [AuthScheme::Bearer, AuthScheme::XApiKey];
        let adapters = [true, false];
        let models: [Option<&str>; 2] = [Some("claude-opus-5"), None];

        for backend in backends {
            for auth in auth_schemes {
                for adapter in adapters {
                    for model in models {
                        let (beta, tool) = advisor_probe(backend.clone(), auth, adapter, model);
                        assert_eq!(
                            beta, tool,
                            "beta/tool diverged for backend={backend:?} auth={auth:?} adapter={adapter} model={model:?}: beta={beta} tool={tool}"
                        );
                    }
                }
            }
        }
    }

    /// The injected tool's exact shape: `{"type":"advisor_20260301","name":"advisor","model":<configured model>}`.
    /// Two distinct models must produce two distinct `model` fields -- the
    /// value must be threaded from `advisor_server_model`, never hardcoded.
    #[test]
    fn advisor_tool_shape_matches_configured_model_exactly() {
        for model in ["claude-opus-5", "claude-sonnet-5-preview"] {
            let mut cfg = minimal_config();
            cfg.api_backend = ApiBackend::Messages;
            cfg.auth_scheme = AuthScheme::Bearer;
            cfg.provider_request_adapter = Some(ProviderRequestAdapter::Anthropic {
                tool_name_prefix: "mcp__".to_string(),
                command: None,
            });
            cfg.advisor_server_model = Some(model.to_string());
            let client = SamplingClient::new(cfg).expect("client should build");

            let mut extra_raw_tools = Vec::new();
            let mut body = serde_json::json!({});
            client.inject_messages_extra_tools(&mut extra_raw_tools, &mut body);

            let tools = body
                .get("tools")
                .and_then(|t| t.as_array())
                .expect("gate-active injection must populate body.tools");
            assert_eq!(tools.len(), 1, "exactly one advisor tool must be injected");
            assert_eq!(
                tools[0],
                serde_json::json!({
                    "type": "advisor_20260301",
                    "name": "advisor",
                    "model": model,
                }),
                "injected tool must be exactly {{type, name, model}} with the configured model, not hardcoded"
            );
        }
    }

    /// When the gate is active AND the request already carries client-side
    /// tools (body["tools"] already populated before injection), the advisor
    /// tool must be APPENDED -- existing tools must never be dropped or
    /// overwritten.
    #[test]
    fn advisor_tool_appended_without_dropping_existing_tools() {
        let mut cfg = minimal_config();
        cfg.api_backend = ApiBackend::Messages;
        cfg.auth_scheme = AuthScheme::Bearer;
        cfg.provider_request_adapter = Some(ProviderRequestAdapter::Anthropic {
            tool_name_prefix: "mcp__".to_string(),
            command: None,
        });
        cfg.advisor_server_model = Some("claude-opus-5".to_string());
        let client = SamplingClient::new(cfg).expect("client should build");

        let existing_tool = serde_json::json!({
            "name": "read_file",
            "description": "Reads a file",
            "input_schema": {"type": "object"},
        });
        let mut extra_raw_tools = Vec::new();
        let mut body = serde_json::json!({ "tools": [existing_tool.clone()] });
        client.inject_messages_extra_tools(&mut extra_raw_tools, &mut body);

        let tools = body
            .get("tools")
            .and_then(|t| t.as_array())
            .expect("tools array must remain present");
        assert_eq!(
            tools.len(),
            2,
            "the pre-existing client tool and the injected advisor tool must both be present"
        );
        assert_eq!(
            tools[0], existing_tool,
            "the pre-existing tool must be preserved in place, not dropped"
        );
        assert_eq!(
            tools[1].get("type").and_then(|v| v.as_str()),
            Some("advisor_20260301"),
            "the advisor tool must be appended after the existing tool"
        );
    }

    #[tokio::test]
    async fn command_adapter_mutates_headers_and_body() {
        let mut cfg = minimal_config();
        cfg.provider_request_adapter = Some(ProviderRequestAdapter::Command {
            argv: vec![
                "python3".to_string(),
                "-c".to_string(),
                "import json,sys; req=json.load(sys.stdin); req['body']['model']='rewritten'; print(json.dumps({'headers': {'x-adapter': 'yes'}, 'body': req['body']}))".to_string(),
            ],
            timeout_ms: Some(1_000),
        });
        let client = SamplingClient::new(cfg).expect("client");
        let mut headers = HeaderMap::new();
        let mut body = serde_json::json!({"model":"original"});

        client
            .apply_command_adapter("messages", "original", &mut headers, &mut body)
            .await
            .expect("adapter applies");

        assert_eq!(
            headers
                .get("x-adapter")
                .and_then(|value| value.to_str().ok()),
            Some("yes")
        );
        assert_eq!(body["model"], "rewritten");
    }

    /// Verify the serialized shape of StreamingChatRequest matches the
    /// expected wire format: all ChatCompletionRequest fields flattened at
    /// top level, plus `stream: true` and `stream_options.include_usage: true`.
    #[test]
    fn streaming_chat_request_serializes_correctly() {
        let request = ChatCompletionRequest {
            model: Some("test-model".into()),
            messages: vec![ChatRequestMessage::user("hello")],
            temperature: Some(0.7),
            max_tokens: None,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            user: None,
            tools: None,
            tool_choice: None,
            search_parameters: None,
            response_format: None,
            reasoning_effort: None,
            x_grok_conv_id: None,
            x_grok_req_id: None,
            x_grok_session_id: None,
            x_grok_turn_idx: None,
            x_grok_agent_id: None,
            x_grok_deployment_id: None,
            x_grok_user_id: None,
            trace: None,
        };

        let wrapper = StreamingChatRequest {
            inner: &request,
            stream: true,
            stream_options: StreamOptions {
                include_usage: true,
            },
        };

        let json: serde_json::Value = serde_json::to_value(&wrapper).unwrap();
        let obj = json.as_object().unwrap();

        assert_eq!(obj.get("stream").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(
            obj.get("stream_options")
                .and_then(|v| v.get("include_usage"))
                .and_then(|v| v.as_bool()),
            Some(true)
        );

        assert!(
            obj.get("inner").is_none(),
            "inner field should be flattened"
        );
        assert_eq!(
            obj.get("model").and_then(|v| v.as_str()),
            Some("test-model")
        );
        assert!(obj.get("messages").is_some());
        let temp = obj.get("temperature").and_then(|v| v.as_f64()).unwrap();
        assert!((temp - 0.7).abs() < 0.001, "temperature should be ~0.7");

        assert!(obj.get("max_tokens").is_none());
        assert!(obj.get("tools").is_none());
    }

    #[test]
    fn extract_retry_after_parses_seconds() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "30".parse().unwrap());
        assert_eq!(extract_retry_after(&headers), Some(30));
    }

    #[test]
    fn extract_retry_after_caps_at_120() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "3600".parse().unwrap());
        assert_eq!(extract_retry_after(&headers), Some(120));
    }

    #[test]
    fn extract_retry_after_zero_is_valid() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "0".parse().unwrap());
        assert_eq!(extract_retry_after(&headers), Some(0));
    }

    #[test]
    fn extract_retry_after_ms_is_preferred_and_rounded_up() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "60".parse().unwrap());
        headers.insert("retry-after-ms", "1500".parse().unwrap());
        assert_eq!(extract_retry_after(&headers), Some(2));
    }

    #[test]
    fn extract_retry_after_ms_is_capped() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("retry-after-ms", "3600000".parse().unwrap());
        assert_eq!(extract_retry_after(&headers), Some(120));
    }

    #[test]
    fn extract_retry_after_invalid_ms_falls_back_to_seconds() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("retry-after-ms", "nope".parse().unwrap());
        headers.insert(reqwest::header::RETRY_AFTER, "30".parse().unwrap());
        assert_eq!(extract_retry_after(&headers), Some(30));
    }

    #[test]
    fn extract_retry_after_parses_http_date() {
        let mut headers = reqwest::header::HeaderMap::new();
        let future = Utc::now() + chrono::Duration::hours(1);
        headers.insert(
            reqwest::header::RETRY_AFTER,
            future.to_rfc2822().parse().unwrap(),
        );
        assert_eq!(extract_retry_after(&headers), Some(120));
    }

    #[test]
    fn extract_retry_after_past_http_date_is_zero() {
        let mut headers = reqwest::header::HeaderMap::new();
        let past = Utc::now() - chrono::Duration::hours(1);
        headers.insert(
            reqwest::header::RETRY_AFTER,
            past.to_rfc2822().parse().unwrap(),
        );
        assert_eq!(extract_retry_after(&headers), Some(0));
    }

    #[test]
    fn extract_retry_after_none_when_missing() {
        let headers = reqwest::header::HeaderMap::new();
        assert_eq!(extract_retry_after(&headers), None);
    }

    #[test]
    fn extract_should_retry_true() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-should-retry", "true".parse().unwrap());
        assert_eq!(extract_should_retry(&headers), Some(true));
    }

    #[test]
    fn extract_should_retry_true_case_insensitive() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-should-retry", "TRUE".parse().unwrap());
        assert_eq!(extract_should_retry(&headers), Some(true));
    }

    #[test]
    fn extract_should_retry_false() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-should-retry", "false".parse().unwrap());
        assert_eq!(extract_should_retry(&headers), Some(false));
    }

    #[test]
    fn extract_should_retry_unknown_value_is_none() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-should-retry", "banana".parse().unwrap());
        assert_eq!(extract_should_retry(&headers), None);
    }

    #[test]
    fn extract_should_retry_absent_is_none() {
        let headers = reqwest::header::HeaderMap::new();
        assert_eq!(extract_should_retry(&headers), None);
    }

    #[test]
    fn new_with_minimal_config_succeeds() {
        let client = SamplingClient::new(minimal_config()).expect("client should construct");
        assert_eq!(client.api_backend(), ApiBackend::ChatCompletions);
    }

    #[test]
    fn new_applies_extra_headers() {
        let mut cfg = minimal_config();
        cfg.extra_headers
            .insert("x-test-header".to_string(), "test-value".to_string());
        cfg.extra_headers
            .insert("x-XAI-token-auth".to_string(), "xai-grok-cli".to_string());
        let _client = SamplingClient::new(cfg).expect("client with extra headers should construct");
    }

    #[test]
    fn apply_env_http_headers_resolves_trims_skips_and_overrides() {
        let mut map = IndexMap::new();
        map.insert("x-tenant-token".to_string(), "TENANT".to_string());
        map.insert("x-blank".to_string(), "BLANK".to_string());
        map.insert("x-missing".to_string(), "MISSING".to_string());
        map.insert("x-override".to_string(), "OVERRIDE".to_string());
        map.insert("x invalid".to_string(), "INVALID".to_string());

        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-override"),
            HeaderValue::from_static("static"),
        );

        apply_env_http_headers(
            &map,
            |var| match var {
                // Leading space + trailing newline exercises trimming.
                "TENANT" => Some(" tenant-secret\n".to_string()),
                "BLANK" => Some("   ".to_string()),
                "OVERRIDE" => Some("from-env".to_string()),
                "INVALID" => Some("value".to_string()),
                _ => None,
            },
            &mut headers,
        );

        assert_eq!(headers.get("x-tenant-token").unwrap(), "tenant-secret");
        assert!(headers.get("x-blank").is_none());
        assert!(headers.get("x-missing").is_none());
        // A resolved env value overrides an existing header of the same name.
        assert_eq!(headers.get("x-override").unwrap(), "from-env");
        // An invalid header name is skipped rather than panicking.
        assert!(headers.get("x invalid").is_none());
    }

    #[test]
    fn endpoint_appends_path_before_a_base_url_query_without_configured_params() {
        let template =
            EndpointTemplate::new("https://gateway.example/v1?api-version=x", &IndexMap::new());
        let url = template.url_for_path("responses");
        assert!(
            url.starts_with("https://gateway.example/v1/responses?"),
            "url: {url}"
        );
        assert!(url.contains("api-version=x"), "url: {url}");
        assert!(!url.contains("x/responses"), "url: {url}");
    }

    #[test]
    fn messages_plus_anthropic_api_key_uses_x_api_key_and_not_authorization() {
        let cfg = SamplerConfig {
            api_key: Some("anthropic-key-abc123".to_string()),
            api_backend: ApiBackend::Messages,
            auth_scheme: AuthScheme::XApiKey,
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        assert!(
            client
                .default_headers
                .get(HeaderName::from_static("x-api-key"))
                .is_some()
        );
        assert!(client.default_headers.get(AUTHORIZATION).is_none());
    }

    #[test]
    fn messages_plus_bearer_uses_authorization_and_not_x_api_key() {
        let cfg = SamplerConfig {
            api_key: Some("bearer-key-abc123".to_string()),
            api_backend: ApiBackend::Messages,
            auth_scheme: AuthScheme::Bearer,
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        assert!(client.default_headers.get(AUTHORIZATION).is_some());
        assert!(
            client
                .default_headers
                .get(HeaderName::from_static("x-api-key"))
                .is_none()
        );
    }

    // Regression: a past change dropped User-Agent from sampling requests.
    #[test]
    fn sampling_client_always_has_user_agent() {
        let client = SamplingClient::new(minimal_config()).expect("build");
        assert!(client.default_headers.contains_key(USER_AGENT));
    }

    // Regression: a past change dropped HeaderInjector (traceparent) from sampling requests.
    #[test]
    fn header_injector_is_called_in_post() {
        #[derive(Debug)]
        struct TestInjector;
        impl crate::config::HeaderInjector for TestInjector {
            fn inject(&self, headers: &mut HeaderMap) {
                headers.insert(
                    HeaderName::from_static("traceparent"),
                    HeaderValue::from_static("00-test-trace-id-00"),
                );
            }
        }

        let mut config = minimal_config();
        config.header_injector = Some(std::sync::Arc::new(TestInjector));
        let client = SamplingClient::new(config).expect("build");
        let SentRequest { builder, .. } = client.post("http://localhost/test");
        let req = builder.build().expect("build request");
        assert!(
            req.headers().contains_key("traceparent"),
            "HeaderInjector should inject traceparent into post() requests"
        );
    }

    #[test]
    fn user_agent_includes_origin_and_agent_product() {
        let origin = OriginClientInfo {
            product: "my-client".to_string(),
            version: Some("1.2.3".to_string()),
        };
        let ua = user_agent_string_for(&origin);
        assert!(ua.contains("my-client/1.2.3"));
        assert!(ua.contains(AGENT_PRODUCT));
    }

    #[test]
    fn user_agent_omits_origin_version_when_absent() {
        let origin = OriginClientInfo {
            product: "my-client".to_string(),
            version: None,
        };
        let ua = user_agent_string_for(&origin);
        // No slash between product and the grok-shell agent product.
        assert!(ua.starts_with("my-client grok-shell/"));
    }

    #[test]
    fn user_agent_collapses_when_origin_matches_agent() {
        let agent_version = xai_grok_version::VERSION.to_string();
        let origin = OriginClientInfo {
            product: AGENT_PRODUCT.to_string(),
            version: Some(agent_version.clone()),
        };
        let ua = user_agent_string_for(&origin);
        // Single product/version slot when the origin and agent match.
        assert!(ua.starts_with(&format!("{}/{}", AGENT_PRODUCT, agent_version)));
    }

    /// Counts callbacks for assertions in the tests below.
    #[derive(Default, Debug)]
    struct CountingCallback {
        invocations: std::sync::Mutex<Vec<(crate::attribution::SamplingConsumer, Option<String>)>>,
    }

    #[derive(Debug)]
    struct StaticBearerResolver(&'static str);

    impl crate::config::BearerResolver for StaticBearerResolver {
        fn current_bearer(&self) -> Option<String> {
            Some(self.0.to_string())
        }
    }

    impl crate::attribution::Auth401AttributionCallback for CountingCallback {
        fn record_401(
            &self,
            consumer: crate::attribution::SamplingConsumer,
            sent_bearer: Option<&str>,
        ) {
            self.invocations
                .lock()
                .unwrap()
                .push((consumer, sent_bearer.map(|s| s.to_string())));
        }
    }

    /// `post()` strips the `"Bearer "` scheme prefix off `Authorization`
    /// and captures the tail fragment (see `SENT_BEARER_PREFIX_LEN`).
    #[test]
    fn post_captures_bearer_tail_for_openai_compat() {
        let cfg = SamplerConfig {
            api_key: Some("test-bearer-1234567890".to_string()),
            api_backend: ApiBackend::ChatCompletions,
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        let SentRequest {
            sent_bearer: bearer,
            ..
        } = client.post("https://example.test/v1/chat/completions");
        assert_eq!(bearer.as_deref(), Some("r-1234567890"));
        assert_eq!(
            bearer.as_deref().map(str::len),
            Some(crate::attribution::SENT_BEARER_PREFIX_LEN),
        );
    }

    /// `post()` captures `x-api-key` for Messages-API backends and keeps
    /// the value's tail fragment.
    #[test]
    fn post_captures_x_api_key_tail_for_messages() {
        let cfg = SamplerConfig {
            api_key: Some("anthropic-key-abc123".to_string()),
            api_backend: ApiBackend::Messages,
            auth_scheme: AuthScheme::XApiKey,
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        let SentRequest {
            sent_bearer: bearer,
            ..
        } = client.post("https://example.test/v1/messages");
        assert_eq!(bearer.as_deref(), Some("c-key-abc123"));
        assert_eq!(
            bearer.as_deref().map(str::len),
            Some(crate::attribution::SENT_BEARER_PREFIX_LEN),
        );
    }

    /// `post()` captures `None` when the request carries no auth header.
    #[test]
    fn post_captures_none_when_no_header() {
        let cfg = SamplerConfig {
            api_key: None,
            api_backend: ApiBackend::ChatCompletions,
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        let SentRequest {
            sent_bearer: bearer,
            ..
        } = client.post("https://example.test/v1/chat/completions");
        assert!(bearer.is_none());
    }

    /// The race this design closes: a 401 triggers a recovery that rotates
    /// the resolver, so a record-time re-read attributes a bearer the
    /// rejected request never carried. The attributed fragment must be the
    /// one captured when the request was built.
    #[test]
    fn post_capture_is_immune_to_resolver_rotation_after_build() {
        #[derive(Debug)]
        struct RotatingResolver(std::sync::Mutex<String>);
        impl crate::config::BearerResolver for RotatingResolver {
            fn current_bearer(&self) -> Option<String> {
                Some(self.0.lock().unwrap().clone())
            }
        }

        let resolver = std::sync::Arc::new(RotatingResolver(std::sync::Mutex::new(
            "rejected-token-oldtail1".to_string(),
        )));
        let cfg = SamplerConfig {
            api_key: None,
            api_backend: ApiBackend::Responses,
            bearer_resolver: Some(resolver.clone()),
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");

        let SentRequest {
            sent_bearer: sent_at_build,
            ..
        } = client.post("https://example.test/v1/responses");
        // The 401 kicks recovery; the resolver rotates before the callback runs.
        *resolver.0.lock().unwrap() = "fresh-token-newtail99".to_string();

        assert_eq!(
            sent_at_build.as_deref(),
            Some("ken-oldtail1"),
            "attribution must describe the bearer the rejected request carried"
        );
        // A record-time re-read (the pre-fix behavior) would report the
        // rotated token instead:
        assert_eq!(
            client.current_sent_bearer_prefix().as_deref(),
            Some("en-newtail99"),
            "sanity: the build-time capture and a live re-read now differ"
        );
    }

    #[test]
    fn live_bearer_resolver_uses_authorization_for_messages_plus_bearer() {
        let cfg = SamplerConfig {
            api_key: Some("stale-bearer".to_string()),
            api_backend: ApiBackend::Messages,
            auth_scheme: AuthScheme::Bearer,
            bearer_resolver: Some(std::sync::Arc::new(StaticBearerResolver("fresh-bearer"))),
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        let SentRequest { builder, .. } = client.post("https://example.test/v1/messages");
        let request = builder.build().expect("request should build");
        let auth = request
            .headers()
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok());
        assert_eq!(auth, Some("Bearer fresh-bearer"));
        assert!(request.headers().get("x-api-key").is_none());
    }

    /// Regression: when `api_key` (which seeds `default_headers` with an
    /// `Authorization: Bearer ...`) AND a `bearer_resolver` are both set,
    /// `post()` must produce **exactly one** `Authorization` header on the
    /// wire. The pre-fix code used `RequestBuilder::header(AUTHORIZATION, ...)`
    /// which appends rather than replaces, causing two identical
    /// `Authorization` headers and a 400 from cli-chat-proxy.
    #[test]
    fn post_emits_single_authorization_with_api_key_and_bearer_resolver() {
        let cfg = SamplerConfig {
            api_key: Some("stale-bearer".to_string()),
            api_backend: ApiBackend::Responses,
            auth_scheme: AuthScheme::Bearer,
            bearer_resolver: Some(std::sync::Arc::new(StaticBearerResolver("fresh-bearer"))),
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        let SentRequest { builder, .. } = client.post("https://example.test/v1/responses");
        let request = builder.build().expect("request should build");
        let auth_count = request.headers().get_all(AUTHORIZATION).iter().count();
        assert_eq!(
            auth_count, 1,
            "expected exactly one Authorization header, got {auth_count}"
        );
        assert_eq!(
            request
                .headers()
                .get(AUTHORIZATION)
                .and_then(|v| v.to_str().ok()),
            Some("Bearer fresh-bearer"),
        );
    }

    #[test]
    fn live_bearer_resolver_uses_x_api_key_for_messages_plus_anthropic_api_key() {
        let cfg = SamplerConfig {
            api_key: Some("stale-anthropic".to_string()),
            api_backend: ApiBackend::Messages,
            auth_scheme: AuthScheme::XApiKey,
            bearer_resolver: Some(std::sync::Arc::new(StaticBearerResolver("fresh-anthropic"))),
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        let SentRequest { builder, .. } = client.post("https://example.test/v1/messages");
        let request = builder.build().expect("request should build");
        let api_key = request
            .headers()
            .get("x-api-key")
            .and_then(|v| v.to_str().ok());
        assert_eq!(api_key, Some("fresh-anthropic"));
        assert!(request.headers().get(AUTHORIZATION).is_none());
    }

    /// The callback receives the `post()`-captured fragment only — the
    /// full bearer never crosses the crate boundary.
    #[test]
    fn record_401_attribution_invokes_callback_with_captured_bearer() {
        let cb = std::sync::Arc::new(CountingCallback::default());
        let cb_dyn: crate::attribution::SharedAttributionCallback = cb.clone();
        let cfg = SamplerConfig {
            api_key: Some("the-bearer-1234567890-extra-tail".to_string()),
            api_backend: ApiBackend::ChatCompletions,
            attribution_callback: Some(cb_dyn),
            bearer_resolver: None,
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        let SentRequest { sent_bearer, .. } =
            client.post("https://example.test/v1/chat/completions");
        client.record_401_attribution(
            crate::attribution::SamplingConsumer::ChatCompletionsStream,
            sent_bearer.as_deref(),
        );
        let calls = cb.invocations.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].0,
            crate::attribution::SamplingConsumer::ChatCompletionsStream
        );
        assert_eq!(calls[0].1.as_deref(), Some("0-extra-tail"));
        assert_eq!(
            calls[0].1.as_deref().map(str::len),
            Some(crate::attribution::SENT_BEARER_PREFIX_LEN),
        );
    }

    /// When a bearer_resolver is wired but returns `None`, attribution must
    /// report no sent bearer (not the construction-time default header seed).
    #[test]
    fn bearer_resolver_none_attribution_ignores_default_headers() {
        #[derive(Debug)]
        struct EmptyResolver;
        impl crate::config::BearerResolver for EmptyResolver {
            fn current_bearer(&self) -> Option<String> {
                None
            }
        }

        let cfg = SamplerConfig {
            api_key: Some("stale-seed-token".to_string()),
            api_backend: ApiBackend::Responses,
            bearer_resolver: Some(std::sync::Arc::new(EmptyResolver)),
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        assert_eq!(
            client.current_sent_bearer_prefix(),
            None,
            "resolver None must not attribute a stripped default seed"
        );
    }

    /// When a bearer_resolver is wired but returns `None` (hard-expired
    /// session with no live AT), default Authorization / x-api-key must be
    /// stripped so a stale seed key cannot ride the wire.
    #[test]
    fn bearer_resolver_none_strips_default_authorization() {
        #[derive(Debug)]
        struct EmptyResolver;
        impl crate::config::BearerResolver for EmptyResolver {
            fn current_bearer(&self) -> Option<String> {
                None
            }
        }

        let cfg = SamplerConfig {
            api_key: Some("stale-token".to_string()),
            api_backend: ApiBackend::Responses,
            bearer_resolver: Some(std::sync::Arc::new(EmptyResolver)),
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        let SentRequest {
            builder,
            sent_bearer: sent,
        } = client.post("https://example.test/v1/responses");
        let request = builder.body("").build().expect("request should build");
        assert_eq!(sent, None, "capture must agree: nothing was sent");
        assert!(
            request.headers().get(AUTHORIZATION).is_none(),
            "stale default Authorization must not be sent when resolver is empty"
        );
    }

    /// Regression test: when a bearer_resolver is wired, `post()` must
    /// *replace* the Authorization header from `default_headers`, not
    /// append a second one. Duplicate Authorization headers cause
    /// Cloudflare to return 400 Bad Request.
    #[test]
    fn bearer_resolver_replaces_authorization_header() {
        #[derive(Debug)]
        struct StaticResolver(String);
        impl crate::config::BearerResolver for StaticResolver {
            fn current_bearer(&self) -> Option<String> {
                Some(self.0.clone())
            }
        }

        let resolver: crate::config::SharedBearerResolver =
            std::sync::Arc::new(StaticResolver("fresh-token".to_string()));
        let cfg = SamplerConfig {
            api_key: Some("stale-token".to_string()),
            api_backend: ApiBackend::Responses,
            bearer_resolver: Some(resolver),
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");

        // Build a request to inspect the final headers.
        let SentRequest { builder, .. } = client.post("https://example.test/v1/responses");
        let request = builder.body("").build().expect("request should build");

        let auth_values: Vec<_> = request.headers().get_all(AUTHORIZATION).iter().collect();
        assert_eq!(
            auth_values.len(),
            1,
            "expected exactly one Authorization header, got {}: {:?}",
            auth_values.len(),
            auth_values
        );
        assert_eq!(
            auth_values[0].to_str().unwrap(),
            "Bearer fresh-token",
            "Authorization header should contain the resolver's fresh token"
        );
    }

    /// `record_401_attribution` is a no-op when `attribution_callback`
    /// is `None` (the BYOK / sampler-only path). The previous tests
    /// in this module construct clients without a callback and rely
    /// on this property holding.
    #[test]
    fn record_401_attribution_is_noop_without_callback() {
        let cfg = SamplerConfig {
            api_key: Some("bearer".to_string()),
            api_backend: ApiBackend::ChatCompletions,
            attribution_callback: None,
            bearer_resolver: None,
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        // Must not panic.
        client.record_401_attribution(
            crate::attribution::SamplingConsumer::ChatCompletions,
            Some("bearer-tail-12"),
        );
    }

    /// `response.completed` carrying
    /// `usage.context_details.{input_tokens, output_tokens}` rewrites
    /// `usage.total_tokens` in place to the live context length
    /// (`ctx.input + ctx.output`). Billing fields stay on the wire's
    /// cumulative values.
    #[test]
    fn deserialize_response_event_overrides_total_tokens_from_context_details() {
        let sse = r#"{
            "type": "response.completed",
            "sequence_number": 0,
            "response": {
                "id": "resp_1",
                "object": "response",
                "created_at": 0,
                "model": "grok-build",
                "status": "completed",
                "output": [],
                "usage": {
                    "input_tokens": 6003,
                    "input_tokens_details": { "cached_tokens": 1984 },
                    "output_tokens": 711,
                    "output_tokens_details": { "reasoning_tokens": 388 },
                    "total_tokens": 6714,
                    "context_details": {
                        "input_tokens": 5022,
                        "output_tokens": 571
                    }
                }
            }
        }"#;
        let event = deserialize_response_event(sse).expect("parse");
        let rs::ResponseStreamEvent::ResponseCompleted(e) = event else {
            panic!("expected ResponseCompleted");
        };
        let usage = e.response.usage.expect("usage present");
        // Billing fields stay cumulative — unchanged by context_details.
        assert_eq!(usage.input_tokens, 6003);
        assert_eq!(usage.output_tokens, 711);
        assert_eq!(usage.input_tokens_details.cached_tokens, 1984);
        assert_eq!(usage.output_tokens_details.reasoning_tokens, 388);
        // total_tokens rewritten to ctx.input + ctx.output (5022 + 571).
        // NOT the wire's cumulative total (6714).
        assert_eq!(usage.total_tokens, 5_593);
    }

    #[test]
    fn deserialize_response_event_stashes_cost_in_metadata() {
        let make = |ticks: i64| {
            format!(
                r#"{{
                "type": "response.completed",
                "sequence_number": 0,
                "response": {{
                    "id": "resp_1", "object": "response", "created_at": 0,
                    "model": "grok-build", "status": "completed", "output": [],
                    "usage": {{
                        "input_tokens": 10,
                        "input_tokens_details": {{ "cached_tokens": 0 }},
                        "output_tokens": 5,
                        "output_tokens_details": {{ "reasoning_tokens": 0 }},
                        "total_tokens": 15,
                        "cost_in_usd_ticks": {ticks}
                    }}
                }}
            }}"#
            )
        };

        let event = deserialize_response_event(&make(78)).expect("parse");
        let rs::ResponseStreamEvent::ResponseCompleted(e) = event else {
            panic!("expected ResponseCompleted");
        };
        assert_eq!(
            e.response
                .metadata
                .as_ref()
                .and_then(|m| m.get(COST_USD_TICKS_METADATA_KEY))
                .map(String::as_str),
            Some("78")
        );

        // The REST mapper backfills 0 for unbilled requests: no stash.
        let event = deserialize_response_event(&make(0)).expect("parse");
        let rs::ResponseStreamEvent::ResponseCompleted(e) = event else {
            panic!("expected ResponseCompleted");
        };
        assert!(e.response.metadata.is_none());
    }

    #[test]
    fn deserialize_response_event_total_tokens_unchanged_when_context_details_absent() {
        // Older / non-Responses backends omit `context_details`.
        // `total_tokens` passes through from the wire unchanged.
        let sse = r#"{
            "type": "response.completed",
            "sequence_number": 0,
            "response": {
                "id": "resp_1",
                "object": "response",
                "created_at": 0,
                "model": "grok-build",
                "status": "completed",
                "output": [],
                "usage": {
                    "input_tokens": 10000,
                    "input_tokens_details": { "cached_tokens": 0 },
                    "output_tokens": 100,
                    "output_tokens_details": { "reasoning_tokens": 0 },
                    "total_tokens": 10100
                }
            }
        }"#;
        let event = deserialize_response_event(sse).expect("parse");
        let rs::ResponseStreamEvent::ResponseCompleted(e) = event else {
            panic!("expected ResponseCompleted");
        };
        let usage = e.response.usage.expect("usage present");
        assert_eq!(usage.total_tokens, 10_100);
    }

    #[test]
    fn deserialize_response_event_total_tokens_unchanged_when_context_details_partial() {
        // Defensive: if the backend ever ships only one of the two
        // context_details fields, we don't have a complete picture of
        // the live context size, so leave `total_tokens` on the wire's
        // cumulative value instead of guessing (treating the missing
        // half as 0 would silently under-report).
        let sse = r#"{
            "type": "response.completed",
            "sequence_number": 0,
            "response": {
                "id": "resp_1",
                "object": "response",
                "created_at": 0,
                "model": "grok-build",
                "status": "completed",
                "output": [],
                "usage": {
                    "input_tokens": 6003,
                    "input_tokens_details": { "cached_tokens": 1984 },
                    "output_tokens": 711,
                    "output_tokens_details": { "reasoning_tokens": 388 },
                    "total_tokens": 6714,
                    "context_details": {
                        "input_tokens": 5022
                    }
                }
            }
        }"#;
        let event = deserialize_response_event(sse).expect("parse");
        let rs::ResponseStreamEvent::ResponseCompleted(e) = event else {
            panic!("expected ResponseCompleted");
        };
        let usage = e.response.usage.expect("usage present");
        assert_eq!(usage.total_tokens, 6_714);
    }

    #[test]
    fn deserialize_response_event_ignores_context_details_on_non_terminal_events() {
        // Non-terminal events don't carry final usage; even if the backend ever
        // echoed `context_details` on one, we don't touch it.
        let sse = r#"{
            "type": "response.output_text.delta",
            "sequence_number": 0,
            "item_id": "item-1",
            "output_index": 0,
            "content_index": 0,
            "delta": "hello",
            "logprobs": []
        }"#;
        let event = deserialize_response_event(sse).expect("non-terminal event parses");
        assert!(matches!(
            event,
            rs::ResponseStreamEvent::ResponseOutputTextDelta(_)
        ));
    }
}
