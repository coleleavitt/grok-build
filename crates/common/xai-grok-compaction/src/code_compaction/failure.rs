//! Deterministic-vs-transient failure classification for compaction
//! LLM calls.
//!
//! The *policy* lives here (shared across harnesses); the per-harness error
//! types and their wrapping (e.g. grok-build's `SamplingError` →
//! `CompactFailure(acp::Error)`) stay in thin host wrappers that delegate the
//! status/message decisions to these functions.

/// Whether a compaction-call failure is worth retrying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// Retrying the same payload will hit the same failure — the retry loop
    /// should bail without sleeping or re-issuing.
    Deterministic,
    /// Failure may resolve on retry (network blips, 5xx, rate limits).
    Transient,
}

impl FailureKind {
    /// `true` for [`FailureKind::Deterministic`].
    pub fn is_deterministic(self) -> bool {
        matches!(self, Self::Deterministic)
    }
}

/// True when an error message indicates a context-window overflow. Backends report
/// this inconsistently with no stable error code, so we match the message text; it's
/// deterministic (re-sending the same payload always fails), so callers must not retry.
pub fn is_context_length_error(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.contains("too long for this model")
        || m.contains("prompt is too long")
        || m.contains("maximum prompt length")
        || m.contains("maximum context length")
        || m.contains("context_length_exceeded")
        || (m.contains("current message") && m.contains("exceeds budget"))
}

/// Provider-reported sizes parsed out of a context-overflow error message,
/// e.g. Anthropic's `prompt is too long: 1285075 tokens > 1000000 maximum`.
///
/// These are the provider's *true* token counts. Clients that shrink their
/// payload with a byte-heuristic estimator need them to calibrate: when the
/// estimator undercounts (heavy base64 / non-ASCII histories), an
/// estimator-budget fit can conclude the payload already fits and re-send the
/// exact bytes the API just rejected — a deterministic retry loop that ends in
/// "Compaction failed.".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextOverflowSizes {
    /// How many tokens the provider counted in the rejected prompt.
    pub prompt_tokens: u64,
    /// The provider's hard maximum, when the message states one.
    pub max_tokens: Option<u64>,
}

/// Parse provider token counts from a context-overflow message. Returns
/// `None` when the message carries no usable numbers (e.g. OpenAI's
/// "maximum context length" phrasing without counts, or non-overflow errors).
///
/// Strategy: scan for `<number> tokens` as the prompt size and
/// `<number> maximum` / `maximum ... <number>` as the limit — covering the
/// known phrasings without pinning one provider's exact format.
pub fn parse_context_overflow_sizes(message: &str) -> Option<ContextOverflowSizes> {
    if !is_context_length_error(message) {
        return None;
    }
    let lower = message.to_ascii_lowercase();
    let tokens: Vec<(usize, u64)> = number_spans(&lower);
    if tokens.is_empty() {
        return None;
    }
    // `<n> tokens` → prompt size (first match wins; Anthropic writes
    // "prompt is too long: <n> tokens > <m> maximum").
    let prompt_tokens = tokens
        .iter()
        .find(|(end, _)| lower[*end..].trim_start().starts_with("token"))
        .map(|(_, n)| *n)?;
    let max_tokens = tokens
        .iter()
        .find(|(end, n)| *n != prompt_tokens && lower[*end..].trim_start().starts_with("maximum"))
        .map(|(_, n)| *n);
    Some(ContextOverflowSizes {
        prompt_tokens,
        max_tokens,
    })
}

/// All decimal-number runs in `s` as `(end_byte_index, value)` pairs.
/// Values that overflow `u64` are skipped.
fn number_spans(s: &str) -> Vec<(usize, u64)> {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if let Ok(n) = s[start..i].parse::<u64>() {
                out.push((i, n));
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Classify an HTTP API failure (status + message) for the compaction retry
/// loop.
///
/// 4xx responses other than 408 (timeout) and 429 (rate limit) are
/// deterministic; a context-length overflow message is deterministic
/// regardless of status (backends sometimes dress it as a synthesized 500).
/// Everything else (5xx, 408, 429) is transient.
pub fn classify_http_status(status: u16, message: &str) -> FailureKind {
    if is_context_length_error(message)
        || ((400..500).contains(&status) && status != 408 && status != 429)
    {
        FailureKind::Deterministic
    } else {
        FailureKind::Transient
    }
}

/// Classify a provider-style stream error event (`ResponseError` /
/// `ResponseFailed.error`) for the compaction retry loop.
///
/// `code` is the structured `code` field on the event (typically a numeric
/// HTTP status as a string, but some providers also use error-type strings like
/// `"invalid_request_error"`). `message` is the human-readable detail.
///
/// Numeric codes are classified by HTTP-status range. The
/// `invalid_request_error` marker, which can appear in either field, always
/// maps to `Deterministic` (schema violations cannot be fixed by re-sending
/// the same payload). The check order is semantic — marker, then numeric
/// code, then context-length message, then default-to-transient.
pub fn classify_stream_event_error(code: Option<&str>, message: &str) -> FailureKind {
    if matches!(code, Some("invalid_request_error")) || message.contains("invalid_request_error") {
        return FailureKind::Deterministic;
    }

    if let Some(status_code) = code.and_then(|c| c.parse::<u16>().ok())
        && (400..500).contains(&status_code)
        && status_code != 408
        && status_code != 429
    {
        return FailureKind::Deterministic;
    }

    // Size overflow arrives here with no parseable code (`code="none"`); the
    // message is the only signal that re-sending cannot help.
    if is_context_length_error(message) {
        return FailureKind::Deterministic;
    }

    FailureKind::Transient
}

#[cfg(test)]
mod tests {
    use super::*;

    fn det_status(status: u16) -> bool {
        classify_http_status(status, "test").is_deterministic()
    }

    #[test]
    fn http_4xx_is_deterministic_except_408_and_429() {
        assert!(det_status(400));
        assert!(det_status(401));
        assert!(det_status(403));
        assert!(det_status(404));
        assert!(det_status(413));
        assert!(!det_status(408));
        assert!(!det_status(429));
        assert!(!det_status(500));
        assert!(!det_status(502));
        assert!(!det_status(503));
    }

    #[test]
    fn http_500_with_context_length_message_is_deterministic() {
        // The sampler synthesizes status=500 from a streamed size overflow, so
        // status alone reads transient; the message must still short-circuit.
        assert!(
            classify_http_status(
                500,
                "API error (status 500 Internal Server Error): \
                 The prompt is too long for this model's context window."
            )
            .is_deterministic()
        );
    }

    #[test]
    fn stream_event_invalid_request_error_marker_is_deterministic() {
        assert!(
            classify_stream_event_error(
                Some("invalid_request_error"),
                "messages.27.content.1: ..."
            )
            .is_deterministic()
        );
        assert!(
            classify_stream_event_error(
                Some("400"),
                "Provider returned invalid_request_error: messages.X..."
            )
            .is_deterministic()
        );
        assert!(
            classify_stream_event_error(None, "messages.X.content.Y: invalid_request_error: ...")
                .is_deterministic()
        );
    }

    #[test]
    fn parse_overflow_sizes_reads_anthropic_message() {
        // Exact production message that broke the input ladder.
        let sizes = parse_context_overflow_sizes(
            "API error (status 400 Bad Request): invalid_request_error: \
             prompt is too long: 1285075 tokens > 1000000 maximum\n\n\
             Request URL: https://api.anthropic.com/v1/messages",
        )
        .expect("counts must parse");
        assert_eq!(sizes.prompt_tokens, 1_285_075);
        assert_eq!(sizes.max_tokens, Some(1_000_000));
    }

    #[test]
    fn parse_overflow_sizes_ignores_leading_status_number() {
        // The "(status 400)" number must not be mistaken for a token count.
        let sizes = parse_context_overflow_sizes("(status 400): prompt is too long: 205000 tokens")
            .expect("counts must parse");
        assert_eq!(sizes.prompt_tokens, 205_000);
        assert_eq!(sizes.max_tokens, None);
    }

    #[test]
    fn parse_overflow_sizes_none_for_countless_or_non_overflow_messages() {
        assert_eq!(
            parse_context_overflow_sizes("maximum context length exceeded"),
            None,
            "no numbers to parse"
        );
        assert_eq!(
            parse_context_overflow_sizes("rate limited, retry after 30 seconds"),
            None,
            "not a context-length error at all"
        );
    }

    #[test]
    fn stream_event_numeric_codes_match_http_classification() {
        let det = |c: &str| classify_stream_event_error(Some(c), "msg").is_deterministic();
        assert!(det("400"));
        assert!(det("401"));
        assert!(det("403"));
        assert!(det("404"));
        assert!(!det("408"));
        assert!(!det("429"));
        assert!(!det("500"));
        assert!(!det("503"));
    }

    #[test]
    fn stream_event_unknown_code_defaults_to_transient() {
        assert!(!classify_stream_event_error(None, "msg").is_deterministic());
        assert!(!classify_stream_event_error(Some("error"), "msg").is_deterministic());
        assert!(!classify_stream_event_error(Some("overloaded_error"), "msg").is_deterministic());
    }

    #[test]
    fn stream_event_context_length_message_is_deterministic() {
        assert!(
            classify_stream_event_error(
                None,
                "The prompt is too long for this model's context window."
            )
            .is_deterministic()
        );
    }

    #[test]
    fn context_length_error_matches_known_messages() {
        for msg in [
            "The prompt is too long for this model's context window.",
            "prompt is too long: 250000 tokens > 200000 maximum",
            "exceeds the maximum prompt length",
            "This model's maximum context length is 128000 tokens",
            "error code: context_length_exceeded",
            "Failed to start sampling: [conversation] Current message (1000000 tokens) exceeds budget (500000 tokens)",
            "compact failed: API error (status 400 Bad Request): invalid-argument: Failed to start sampling: [conversation] Current message (1000000 tokens) exceeds budget (500000 tokens)",
            "Current message (600000) exceeds budget (500000)",
        ] {
            assert!(is_context_length_error(msg), "should match: {msg}");
        }
        for msg in [
            "internal server error",
            "rate limited",
            "connection reset by peer",
            "Attached file content (300000 tokens) causes message to exceed budget",
            "compact index estimate 2.0 GB exceeds budget 1.0 GB",
        ] {
            assert!(!is_context_length_error(msg), "should not match: {msg}");
        }
    }
}
