//! Shared advisor policy and transcript snapshot helpers.
//!
//! The advisor is exposed through the normal `task`/subagent flow, but its
//! operational rules need to be enforced at more than one layer: the
//! model-facing task tool gates requests before background spawns, and the
//! shell coordinator keeps a defense-in-depth check for non-tool internal
//! callers.  This module owns the shared configuration, bounded snapshot, and
//! per-session budget state so those layers do not drift.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Default per-session advisor budget. Mirrors JFC's conservative shape:
/// enough for a few side reviews, not enough for a runaway loop.
pub const DEFAULT_ADVISOR_BUDGET: u64 = 50_000;
/// Default maximum advisor snapshot size in characters.
pub const DEFAULT_ADVISOR_SNAPSHOT_CHARS: usize = 40_000;
/// Per-tool/result preview cap inside an advisor transcript snapshot.
const TOOL_PREVIEW_CHARS: usize = 2_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvisorConfig {
    pub enabled: bool,
    pub token_budget: u64,
    pub max_snapshot_chars: usize,
}

impl Default for AdvisorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            token_budget: DEFAULT_ADVISOR_BUDGET,
            max_snapshot_chars: DEFAULT_ADVISOR_SNAPSHOT_CHARS,
        }
    }
}

impl AdvisorConfig {
    /// Resolve advisor config from environment. Config-file plumbing can layer
    /// on top later; this gives an immediately testable runtime gate.
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if env_truthy("GROK_ADVISOR_DISABLED") || env_truthy("GROK_DISABLE_ADVISOR") {
            cfg.enabled = false;
        }
        if let Some(enabled) = env_bool("GROK_ADVISOR_ENABLED") {
            cfg.enabled = enabled;
        }
        if let Ok(raw) = std::env::var("GROK_ADVISOR_BUDGET")
            && let Ok(value) = raw.trim().parse::<u64>()
        {
            cfg.token_budget = value;
        }
        if let Ok(raw) = std::env::var("GROK_ADVISOR_SNAPSHOT_CHARS")
            && let Ok(value) = raw.trim().parse::<usize>()
        {
            cfg.max_snapshot_chars = value.max(1);
        }
        cfg
    }
}

fn env_truthy(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .and_then(|value| env_bool_value(&value))
        .unwrap_or(false)
}

fn env_bool(name: &str) -> Option<bool> {
    std::env::var(name)
        .ok()
        .and_then(|value| env_bool_value(&value))
}

fn env_bool_value(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" | "enabled" | "enable" => Some(true),
        "0" | "false" | "no" | "off" | "disabled" | "disable" => Some(false),
        _ => None,
    }
}

/// Advisor budget state for one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvisorBudget {
    limit: u64,
    used: u64,
}

impl AdvisorBudget {
    pub fn new(limit: u64) -> Self {
        Self { limit, used: 0 }
    }

    pub fn used(&self) -> u64 {
        self.used
    }

    pub fn remaining(&self) -> u64 {
        self.limit.saturating_sub(self.used)
    }

    pub fn try_reserve(&mut self, estimated_tokens: u64) -> Result<(), AdvisorError> {
        if self.remaining() < estimated_tokens {
            return Err(AdvisorError::BudgetExhausted {
                requested: estimated_tokens,
                remaining: self.remaining(),
            });
        }
        self.used = self.used.saturating_add(estimated_tokens);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdvisorError {
    Disabled,
    EmptyQuestion,
    BudgetExhausted { requested: u64, remaining: u64 },
    ModelUnavailable(String),
}

impl std::fmt::Display for AdvisorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disabled => f.write_str("advisor is disabled"),
            Self::EmptyQuestion => f.write_str("advisor question is empty"),
            Self::BudgetExhausted {
                requested,
                remaining,
            } => write!(
                f,
                "advisor token budget exhausted: requested {requested}, remaining {remaining}",
            ),
            Self::ModelUnavailable(model) => write!(f, "advisor model unavailable: {model}"),
        }
    }
}

impl std::error::Error for AdvisorError {}

/// Minimal transcript item used for advisor snapshot tests and future adapters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvisorTranscriptItem {
    pub role: &'static str,
    pub text: String,
    pub tool_name: Option<String>,
    pub tool_output: Option<String>,
}

impl AdvisorTranscriptItem {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: "User",
            text: text.into(),
            tool_name: None,
            tool_output: None,
        }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: "Assistant",
            text: text.into(),
            tool_name: None,
            tool_output: None,
        }
    }

    pub fn tool(name: impl Into<String>, output: impl Into<String>) -> Self {
        Self {
            role: "Assistant",
            text: String::new(),
            tool_name: Some(name.into()),
            tool_output: Some(output.into()),
        }
    }
}

/// Build a bounded read-only snapshot for an advisor call. Recent context is
/// preserved by tail truncation; tool outputs are preview-capped before the
/// global snapshot cap is applied.
pub fn build_advisor_snapshot(items: &[AdvisorTranscriptItem], max_chars: usize) -> String {
    let mut out = String::new();
    for item in items {
        if !item.text.trim().is_empty() {
            out.push_str(item.role);
            out.push_str(": ");
            out.push_str(item.text.trim());
            out.push('\n');
        }
        if let Some(tool) = &item.tool_name {
            out.push_str(item.role);
            out.push_str(": [Tool: ");
            out.push_str(tool);
            out.push_str("]\n");
            if let Some(output) = &item.tool_output {
                out.push_str("Tool result: ");
                push_preview(&mut out, output.trim(), TOOL_PREVIEW_CHARS);
                out.push('\n');
            }
        }
    }
    tail_cap(out, max_chars)
}

fn push_preview(out: &mut String, text: &str, limit: usize) {
    if text.chars().count() <= limit {
        out.push_str(text);
        return;
    }
    out.extend(text.chars().take(limit));
    out.push_str(&format!(
        "... [truncated, {} chars total]",
        text.chars().count()
    ));
}

fn tail_cap(mut text: String, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text;
    }
    let marker = "[…earlier transcript elided…]\n";
    let keep = max_chars.saturating_sub(marker.chars().count());
    let tail = text
        .chars()
        .rev()
        .take(keep)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    text.clear();
    text.push_str(marker);
    text.push_str(&tail);
    text
}

/// Estimate advisor budget cost for a snapshot + question. Cheap and stable;
/// production can replace with provider usage when available.
pub fn estimate_advisor_tokens(snapshot: &str, question: &str) -> u64 {
    ((snapshot.len() + question.len()) / 4).max(1) as u64
}

pub fn validate_advisor_call(
    cfg: &AdvisorConfig,
    budget: &mut AdvisorBudget,
    snapshot: &str,
    question: &str,
) -> Result<(), AdvisorError> {
    if !cfg.enabled {
        return Err(AdvisorError::Disabled);
    }
    if question.trim().is_empty() {
        return Err(AdvisorError::EmptyQuestion);
    }
    budget.try_reserve(estimate_advisor_tokens(snapshot, question))
}

static ADVISOR_BUDGETS: OnceLock<Mutex<HashMap<String, AdvisorBudget>>> = OnceLock::new();

fn advisor_budgets() -> &'static Mutex<HashMap<String, AdvisorBudget>> {
    ADVISOR_BUDGETS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Validate and reserve budget for a real `advisor` subagent spawn using an
/// already-computed estimate for the exact context the child will receive.
///
/// The model-facing task tool calls into the coordinator to compute this from
/// the real forked parent transcript before any background spawn is started.
pub fn validate_advisor_subagent_spawn_with_estimate(
    parent_session_id: &str,
    request_prompt: &str,
    estimated_tokens: u64,
) -> Result<(), AdvisorError> {
    let cfg = AdvisorConfig::from_env();
    if !cfg.enabled {
        return Err(AdvisorError::Disabled);
    }
    if request_prompt.trim().is_empty() {
        return Err(AdvisorError::EmptyQuestion);
    }

    let mut budgets = advisor_budgets()
        .lock()
        .expect("advisor budget mutex poisoned");
    let budget = budgets
        .entry(parent_session_id.to_owned())
        .or_insert_with(|| AdvisorBudget::new(cfg.token_budget));
    budget.try_reserve(estimated_tokens.max(1))
}

/// Validate and reserve budget when only a prompt-level snapshot is available.
///
/// This is a fallback for non-forking callers. Advisor TaskTool/forked-subagent
/// launches should prefer [`validate_advisor_subagent_spawn_with_estimate`] with
/// the coordinator-computed fork estimate.
pub fn validate_advisor_subagent_spawn(
    parent_session_id: &str,
    request_prompt: &str,
) -> Result<(), AdvisorError> {
    let cfg = AdvisorConfig::from_env();
    let snapshot = tail_cap(request_prompt.trim().to_owned(), cfg.max_snapshot_chars);
    let estimated_tokens = estimate_advisor_tokens(&snapshot, request_prompt);
    validate_advisor_subagent_spawn_with_estimate(
        parent_session_id,
        request_prompt,
        estimated_tokens,
    )
}
