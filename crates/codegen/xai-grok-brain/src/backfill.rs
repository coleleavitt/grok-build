//! Bounded Grok session-history backfill for Brain.
//!
//! This ports Onyx `brain/tasks.py` run semantics to Grok Build's local JSONL
//! session model: recent-session/last-run cutoff, session/message/transcript/doc
//! caps, `[S#]`/`[D#]` source-map construction, connector/document toggling via
//! settings, provider extraction, page/source/relation application, and
//! last-run stamping.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::engine::{DocumentSource, RunContext, SessionSource};
use crate::{BrainSettings, Result};

/// Onyx cap: at most 25 recent sessions per run.
pub const BRAIN_MAX_SESSIONS_PER_RUN: usize = 25;
/// Onyx cap: at most 20 messages per session in prompt material.
pub const BRAIN_MAX_MESSAGES_PER_SESSION: usize = 20;
/// Onyx cap: at most 800 chars from one message.
pub const BRAIN_MAX_CHARS_PER_MESSAGE: usize = 800;
/// Onyx cap: at most 24k transcript chars total.
pub const BRAIN_MAX_TRANSCRIPT_CHARS: usize = 24_000;
/// Onyx cap: at most 20 cited documents.
pub const BRAIN_MAX_DOCS: usize = 20;
/// Onyx recent-window for first run: 14 days.
pub const BRAIN_LOOKBACK_DAYS: i64 = 14;
/// Grok-specific cap: extra artifact lines mined per session.
pub const BRAIN_MAX_ARTIFACT_LINES_PER_SESSION: usize = 16;
/// Grok-specific cap: characters from one artifact excerpt.
pub const BRAIN_MAX_CHARS_PER_ARTIFACT: usize = 1_000;

/// A parsed persisted Grok session, before Onyx-style bounds are applied.
#[derive(Debug, Clone)]
pub struct PersistedBrainSession {
    /// Stable session id.
    pub id: String,
    /// Display label.
    pub label: String,
    /// Last update time used for cutoff and sorting.
    pub updated_at: DateTime<Utc>,
    /// Chronological user/assistant lines, already role-labelled.
    pub lines: Vec<String>,
    /// File/document-like citations discovered in the session text.
    pub documents: Vec<DocumentSource>,
    /// Workspace scope decoded from the persisted session directory.
    pub workspace_scope: Option<String>,
}

/// What the bounded backfill reader selected.
#[derive(Debug, Clone)]
pub struct BackfillSelection {
    /// Sessions selected after sort/limit/cutoff.
    pub sessions: Vec<PersistedBrainSession>,
    /// Run context passed to the Brain extraction provider.
    pub context: RunContext,
    /// Cutoff used for this run.
    pub cutoff: DateTime<Utc>,
}

/// Build a bounded [`RunContext`] from already parsed sessions. This is the
/// pure parity port of Onyx `_recent_sessions` + `_build_context` selection.
pub fn build_bounded_run_context(
    mut sessions: Vec<PersistedBrainSession>,
    settings: &BrainSettings,
    _now: DateTime<Utc>,
) -> BackfillSelection {
    // Do not time-window Brain backfill. The prompt budget still caps how much
    // source text reaches the extractor, but selection may draw from any
    // persisted session instead of silently ignoring older durable work.
    let cutoff = DateTime::<Utc>::MIN_UTC;

    sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    let sessions: Vec<_> = sessions.into_iter().collect();

    let mut total = 0usize;
    let mut docs = Vec::new();
    let mut context_sessions = Vec::new();
    for session in &sessions {
        let mut lines = Vec::new();
        for line in session.lines.iter().take(BRAIN_MAX_MESSAGES_PER_SESSION) {
            let clipped = truncate_chars(line.trim(), BRAIN_MAX_CHARS_PER_MESSAGE);
            if clipped.is_empty() {
                continue;
            }
            if total + clipped.len() + 1 > BRAIN_MAX_TRANSCRIPT_CHARS {
                break;
            }
            total += clipped.len() + 1;
            lines.push(clipped);
        }
        if !lines.is_empty() {
            context_sessions.push(SessionSource {
                id: session.id.clone(),
                label: Some(session.label.clone()),
                url: Some(format!("grok://session/{}", session.id)),
                workspace_scope: session.workspace_scope.clone(),
                lines,
            });
        }
        if settings.use_connectors {
            for doc in &session.documents {
                if docs.len() >= BRAIN_MAX_DOCS {
                    break;
                }
                if docs
                    .iter()
                    .any(|existing: &DocumentSource| existing.id == doc.id)
                {
                    continue;
                }
                docs.push(doc.clone());
            }
        }
    }

    BackfillSelection {
        sessions,
        context: RunContext {
            sessions: context_sessions,
            documents: docs,
        },
        cutoff,
    }
}

/// Read persisted Grok JSONL sessions from a `~/.grok`-style root and apply
/// Onyx-equivalent bounds.
pub fn read_bounded_run_context(
    grok_home: &Path,
    settings: &BrainSettings,
    now: DateTime<Utc>,
) -> Result<BackfillSelection> {
    let sessions = read_persisted_sessions(grok_home)?;
    Ok(build_bounded_run_context(sessions, settings, now))
}

/// Read every parseable session below `{grok_home}/sessions/*/*`.
pub fn read_persisted_sessions(grok_home: &Path) -> Result<Vec<PersistedBrainSession>> {
    let root = grok_home.join("sessions");
    let mut out = Vec::new();
    if !root.exists() {
        return Ok(out);
    }
    for cwd_entry in fs::read_dir(root)? {
        let cwd_entry = cwd_entry?;
        if !cwd_entry.file_type()?.is_dir() {
            continue;
        }
        let workspace_scope = cwd_entry
            .file_name()
            .to_str()
            .and_then(decode_workspace_scope);
        for session_entry in fs::read_dir(cwd_entry.path())? {
            let session_entry = session_entry?;
            if !session_entry.file_type()?.is_dir() {
                continue;
            }
            if let Some(mut session) = read_session_dir(&session_entry.path())? {
                session.workspace_scope = workspace_scope.clone();
                out.push(session);
            }
        }
    }
    Ok(out)
}

fn read_session_dir(dir: &Path) -> Result<Option<PersistedBrainSession>> {
    let summary_path = dir.join("summary.json");
    let chat_path = dir.join("chat_history.jsonl");
    if !summary_path.exists() || !chat_path.exists() {
        return Ok(None);
    }
    let summary: SummaryJson = serde_json::from_slice(&fs::read(&summary_path)?)?;
    let id = summary
        .info
        .and_then(|info| info.id)
        .or_else(|| {
            dir.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "unknown-session".to_owned());
    let updated_at = summary
        .updated_at
        .or(summary.created_at)
        .unwrap_or_else(Utc::now);
    let label = summary
        .session_summary
        .as_deref()
        .and_then(clean_session_label)
        .unwrap_or_else(|| format!("Grok session {}", short_session_id(&id)));
    let (mut lines, mut documents) = read_chat_history(&chat_path)?;
    let (artifact_lines, artifact_documents) = read_session_artifacts(dir)?;
    lines.extend(artifact_lines);
    documents.extend(artifact_documents);
    dedup_documents(&mut documents);
    if lines.is_empty() {
        return Ok(None);
    }
    Ok(Some(PersistedBrainSession {
        id,
        label,
        updated_at,
        lines,
        documents,
        workspace_scope: None,
    }))
}

fn short_session_id(id: &str) -> &str {
    &id[..id.len().min(8)]
}

fn clean_session_label(label: &str) -> Option<String> {
    let collapsed = label.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim();
    if trimmed.is_empty() {
        return None;
    }
    let noisy = trimmed.starts_with('─')
        || trimmed.starts_with('{')
        || trimmed.starts_with('<')
        || trimmed.starts_with("# jfc")
        || trimmed.contains("Press Ctrl-C")
        || trimmed.contains("[Tool Call]")
        || trimmed.len() > 120;
    if noisy {
        None
    } else {
        Some(truncate_chars(trimmed, 96))
    }
}

fn decode_workspace_scope(raw: &str) -> Option<String> {
    let decoded = percent_decode(raw)?;
    let trimmed = decoded.trim_end_matches('/');
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn percent_decode(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = hex_value(bytes[i + 1])?;
            let lo = hex_value(bytes[i + 2])?;
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn read_chat_history(path: &Path) -> Result<(Vec<String>, Vec<DocumentSource>)> {
    let content = fs::read_to_string(path)?;
    let mut lines = Vec::new();
    let mut docs = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(item) = serde_json::from_str::<ChatItemJson>(line) else {
            continue;
        };
        match item {
            ChatItemJson::User {
                content,
                synthetic_reason,
            } => {
                if synthetic_reason.is_some() {
                    continue;
                }
                for text in content.into_texts() {
                    collect_document_refs(&text, &mut docs);
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        lines.push(format!("User: {trimmed}"));
                    }
                }
            }
            ChatItemJson::Assistant { content } => {
                let trimmed = content.trim();
                if !trimmed.is_empty() {
                    collect_document_refs(trimmed, &mut docs);
                    lines.push(format!("Assistant: {trimmed}"));
                }
            }
            ChatItemJson::Other => {}
        }
    }
    Ok((lines, docs))
}

fn read_session_artifacts(path: &Path) -> Result<(Vec<String>, Vec<DocumentSource>)> {
    let mut lines = Vec::new();
    let mut docs = Vec::new();
    read_jsonl_artifact_file(path, "events.jsonl", "Event", &mut lines, &mut docs)?;
    read_jsonl_artifact_file(path, "updates.jsonl", "Update", &mut lines, &mut docs)?;
    read_text_artifact_dir(path, "terminal", "Terminal log", &mut lines, &mut docs)?;
    read_text_artifact_dir(path, "mcp", "Tool output", &mut lines, &mut docs)?;
    read_text_artifact_dir(path, "subagents", "Subagent output", &mut lines, &mut docs)?;
    read_text_artifact_dir(path, "goal", "Goal artifact", &mut lines, &mut docs)?;
    read_text_artifact_dir(path, "test_reports", "Test report", &mut lines, &mut docs)?;
    read_text_artifact_dir(path, "reports", "Test report", &mut lines, &mut docs)?;
    read_named_text_artifact_file(path, "plan.md", "Plan artifact", &mut lines, &mut docs)?;
    read_named_text_artifact_file(path, "plan.json", "Plan artifact", &mut lines, &mut docs)?;
    read_named_text_artifact_file(
        path,
        "plan_mode.json",
        "Plan artifact",
        &mut lines,
        &mut docs,
    )?;
    lines.truncate(BRAIN_MAX_ARTIFACT_LINES_PER_SESSION);
    Ok((lines, docs))
}

fn read_jsonl_artifact_file(
    session_dir: &Path,
    file_name: &str,
    label: &str,
    lines: &mut Vec<String>,
    docs: &mut Vec<DocumentSource>,
) -> Result<()> {
    if lines.len() >= BRAIN_MAX_ARTIFACT_LINES_PER_SESSION {
        return Ok(());
    }
    let path = session_dir.join(file_name);
    if !path.exists() {
        return Ok(());
    }
    let content = fs::read_to_string(&path)?;
    let mut kept = 0usize;
    for line in content.lines().rev() {
        if kept >= 4 || lines.len() >= BRAIN_MAX_ARTIFACT_LINES_PER_SESSION {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parsed = serde_json::from_str::<serde_json::Value>(line).ok();
        if parsed.as_ref().is_some_and(is_measurement_only_event) {
            // Instrumentation carries no content for memory extraction, and
            // this window keeps only the last few lines — so a high-frequency
            // measurement event silently evicts the session events that DO
            // carry meaning, and contributes a content-free excerpt in their
            // place.
            continue;
        }
        let excerpt = parsed
            .and_then(|value| summarize_json_artifact(&value))
            .unwrap_or_else(|| truncate_chars(line, BRAIN_MAX_CHARS_PER_ARTIFACT));
        if excerpt.trim().is_empty() {
            continue;
        }
        collect_document_refs(&excerpt, docs);
        lines.push(format!("{label} {file_name}: {excerpt}"));
        docs.push(artifact_document(&path, label, &excerpt));
        kept += 1;
    }
    Ok(())
}

/// Wire tags of events that exist purely to be counted later.
///
/// MIRRORS `xai_file_utils::events::types::MEASUREMENT_ONLY_EVENT_TYPES`, which
/// is canonical because it sits beside the enum that owns the tags. Mirrored
/// rather than imported on purpose: this crate has no workspace dependencies,
/// and taking one on the whole event/tracker/log machinery to share a single
/// string would be a worse trade than restating it.
///
/// The cost is honest — nothing enforces agreement across the crate boundary.
/// Both sites carry a note and a test pinning the same literal set, so a
/// divergence is one grep away rather than invisible.
const MEASUREMENT_ONLY_EVENT_TYPES: &[&str] = &["goal_role_assignment"];

/// Whether a parsed event line is instrumentation rather than content.
///
/// Excluded from the self-improvement extraction context: these describe the
/// harness measuring itself, never anything about the user's project, and they
/// are emitted often enough to crowd out the events that do.
fn is_measurement_only_event(value: &serde_json::Value) -> bool {
    value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|tag| MEASUREMENT_ONLY_EVENT_TYPES.contains(&tag))
}

fn read_text_artifact_dir(
    session_dir: &Path,
    dir_name: &str,
    label: &str,
    lines: &mut Vec<String>,
    docs: &mut Vec<DocumentSource>,
) -> Result<()> {
    if lines.len() >= BRAIN_MAX_ARTIFACT_LINES_PER_SESSION {
        return Ok(());
    }
    let root = session_dir.join(dir_name);
    if !root.exists() {
        return Ok(());
    }
    let mut files = Vec::new();
    collect_artifact_files(&root, &mut files)?;
    files.sort_by_key(|path| {
        std::cmp::Reverse(
            path.metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |duration| duration.as_secs()),
        )
    });
    for path in files.into_iter().take(6) {
        if lines.len() >= BRAIN_MAX_ARTIFACT_LINES_PER_SESSION {
            break;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        let excerpt = summarize_artifact_file(&path, &content);
        if excerpt.is_empty() {
            continue;
        }
        collect_document_refs(&excerpt, docs);
        let rel = path
            .strip_prefix(session_dir)
            .unwrap_or(&path)
            .to_string_lossy();
        lines.push(format!("{label} {rel}: {excerpt}"));
        docs.push(artifact_document(&path, label, &excerpt));
    }
    Ok(())
}

fn read_named_text_artifact_file(
    session_dir: &Path,
    file_name: &str,
    label: &str,
    lines: &mut Vec<String>,
    docs: &mut Vec<DocumentSource>,
) -> Result<()> {
    if lines.len() >= BRAIN_MAX_ARTIFACT_LINES_PER_SESSION {
        return Ok(());
    }
    let path = session_dir.join(file_name);
    if !path.exists() {
        return Ok(());
    }
    let content = fs::read_to_string(&path)?;
    let excerpt = summarize_artifact_file(&path, &content);
    if excerpt.is_empty() {
        return Ok(());
    }
    collect_document_refs(&excerpt, docs);
    lines.push(format!("{label} {file_name}: {excerpt}"));
    docs.push(artifact_document(&path, label, &excerpt));
    Ok(())
}

fn collect_artifact_files(root: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_artifact_files(&path, out)?;
        } else if file_type.is_file()
            && !path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext == "lock")
        {
            out.push(path);
        }
    }
    Ok(())
}

fn summarize_artifact_file(path: &Path, content: &str) -> String {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default();
    if matches!(extension, "json" | "jsonl") {
        let mut pieces = Vec::new();
        for line in content.lines().rev().take(8) {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed)
                && let Some(summary) = summarize_json_artifact(&value)
            {
                pieces.push(summary);
            }
            if pieces.len() >= 4 {
                break;
            }
        }
        if !pieces.is_empty() {
            return truncate_chars(&pieces.join(" | "), BRAIN_MAX_CHARS_PER_ARTIFACT);
        }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(content)
            && let Some(summary) = summarize_json_artifact(&value)
        {
            return summary;
        }
    }
    summarize_text_artifact(content)
}

fn summarize_json_artifact(value: &serde_json::Value) -> Option<String> {
    let mut parts = Vec::new();
    collect_json_strings(value, "", &mut parts);
    parts.dedup();
    let joined = parts
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .take(12)
        .collect::<Vec<_>>()
        .join("; ");
    (!joined.trim().is_empty()).then(|| truncate_chars(&joined, BRAIN_MAX_CHARS_PER_ARTIFACT))
}

fn collect_json_strings(value: &serde_json::Value, key: &str, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => {
            if keep_json_key(key) || text.contains("error") || text.contains("failed") {
                out.push(format_json_piece(key, text));
            }
        }
        serde_json::Value::Number(number) => {
            if keep_json_key(key) {
                out.push(format!("{key}={number}"));
            }
        }
        serde_json::Value::Bool(flag) => {
            if keep_json_key(key) {
                out.push(format!("{key}={flag}"));
            }
        }
        serde_json::Value::Array(items) => {
            for item in items.iter().take(6) {
                collect_json_strings(item, key, out);
            }
        }
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                collect_json_strings(value, key, out);
            }
        }
        serde_json::Value::Null => {}
    }
}

fn keep_json_key(key: &str) -> bool {
    matches!(
        key,
        "type"
            | "method"
            | "sessionUpdate"
            | "name"
            | "description"
            | "message"
            | "error"
            | "status"
            | "command"
            | "summary"
            | "title"
            | "text"
            | "output"
            | "stdout"
            | "stderr"
            | "result"
            | "exit_code"
            | "exitCode"
            | "task_id"
            | "taskId"
            | "server_name"
            | "tool_name"
            | "toolName"
            | "model_id"
            | "path"
    )
}

fn format_json_piece(key: &str, text: &str) -> String {
    let text = text.replace(['\n', '\r', '\t'], " ");
    let text = truncate_chars(text.trim(), 240);
    if key.is_empty() {
        text
    } else {
        format!("{key}: {text}")
    }
}

fn summarize_text_artifact(content: &str) -> String {
    let mut selected = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if selected.len() < 3
            || line.contains("error")
            || line.contains("failed")
            || line.contains("passed")
            || line.contains("test result")
        {
            selected.push(line.to_owned());
        }
        if selected.len() >= 10 {
            break;
        }
    }
    truncate_chars(&selected.join(" | "), BRAIN_MAX_CHARS_PER_ARTIFACT)
}

fn artifact_document(path: &Path, label: &str, excerpt: &str) -> DocumentSource {
    let id = format!("file://{}", path.display());
    let file_label = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| label.to_owned());
    DocumentSource {
        id: id.clone(),
        label: format!("{label}: {file_label}"),
        url: Some(id),
        blurb: Some(truncate_chars(excerpt, 300)),
    }
}

fn dedup_documents(docs: &mut Vec<DocumentSource>) {
    let mut seen = std::collections::HashSet::new();
    docs.retain(|doc| seen.insert(doc.id.clone()));
}

fn collect_document_refs(text: &str, docs: &mut Vec<DocumentSource>) {
    for uri in extract_file_uris(text) {
        if docs.iter().any(|doc| doc.id == uri) || docs.len() >= BRAIN_MAX_DOCS {
            continue;
        }
        let label = PathBuf::from(uri.trim_start_matches("file://"))
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| uri.clone());
        docs.push(DocumentSource {
            id: uri.clone(),
            label,
            url: Some(uri),
            blurb: Some(truncate_chars(text.trim(), 300)),
        });
    }
    for path in extract_file_contents_paths(text) {
        if docs.iter().any(|doc| doc.id == path) || docs.len() >= BRAIN_MAX_DOCS {
            continue;
        }
        let label = PathBuf::from(&path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone());
        docs.push(DocumentSource {
            id: path.clone(),
            label,
            url: Some(format!("file://{path}")),
            blurb: Some(truncate_chars(text.trim(), 300)),
        });
    }
}

fn extract_file_uris(text: &str) -> Vec<String> {
    text.split_whitespace()
        .filter_map(|token| {
            let idx = token.find("file://")?;
            let raw = &token[idx..];
            let end = raw.find([')', ']', '>', '"', '\'']).unwrap_or(raw.len());
            Some(raw[..end].trim_end_matches([',', '.', ';']).to_owned())
        })
        .filter(|s| s.len() > "file://".len())
        .collect()
}

fn extract_file_contents_paths(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(idx) = rest.find("path=\"") {
        let after = &rest[idx + 6..];
        let Some(end) = after.find('"') else {
            break;
        };
        let path = after[..end].to_owned();
        if !path.is_empty() {
            out.push(path);
        }
        rest = &after[end + 1..];
    }
    out
}

fn truncate_chars(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

#[derive(Debug, Deserialize)]
struct SummaryJson {
    info: Option<SummaryInfo>,
    session_summary: Option<String>,
    created_at: Option<DateTime<Utc>>,
    updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
struct SummaryInfo {
    id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ChatItemJson {
    User {
        content: ChatContent,
        #[serde(default)]
        synthetic_reason: Option<String>,
    },
    Assistant {
        content: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ChatContent {
    Text(String),
    Parts(Vec<ChatContentPart>),
}

impl ChatContent {
    fn into_texts(self) -> Vec<String> {
        match self {
            Self::Text(text) => vec![text],
            Self::Parts(parts) => parts.into_iter().filter_map(|part| part.text).collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ChatContentPart {
    #[serde(default)]
    text: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    fn session(id: &str, days_ago: i64, lines: usize) -> PersistedBrainSession {
        PersistedBrainSession {
            id: id.to_owned(),
            label: format!("Session {id}"),
            updated_at: Utc.with_ymd_and_hms(2026, 7, 21, 12, 0, 0).unwrap()
                - Duration::days(days_ago),
            lines: (0..lines)
                .map(|i| format!("User: session {id} line {i}"))
                .collect(),
            documents: vec![DocumentSource {
                id: format!("file:///{id}.md"),
                label: format!("{id}.md"),
                url: Some(format!("file:///{id}.md")),
                blurb: Some("doc".to_owned()),
            }],
            workspace_scope: None,
        }
    }

    #[test]
    fn bounded_context_ignores_time_windows_but_keeps_prompt_caps() {
        let now = Utc.with_ymd_and_hms(2026, 7, 21, 12, 0, 0).unwrap();
        let sessions = (0..30)
            .map(|i| session(&format!("s{i}"), i as i64, 30))
            .collect::<Vec<_>>();
        let settings = BrainSettings {
            enabled: true,
            use_connectors: true,
            focus_instructions: None,
            last_run_at: Some(now - Duration::days(5)),
        };
        let selection = build_bounded_run_context(sessions, &settings, now);
        assert_eq!(
            selection.sessions.len(),
            30,
            "all persisted sessions remain eligible; prompt caps bound extracted text"
        );
        assert_eq!(selection.context.sessions.len(), 30);
        assert!(
            selection
                .context
                .sessions
                .iter()
                .all(|s| s.lines.len() == BRAIN_MAX_MESSAGES_PER_SESSION)
        );
        assert_eq!(selection.context.documents.len(), BRAIN_MAX_DOCS);
    }

    #[test]
    fn connector_toggle_controls_documents() {
        let now = Utc.with_ymd_and_hms(2026, 7, 21, 12, 0, 0).unwrap();
        let mut settings = BrainSettings {
            enabled: true,
            use_connectors: false,
            focus_instructions: None,
            last_run_at: None,
        };
        let without = build_bounded_run_context(vec![session("s", 0, 1)], &settings, now);
        assert!(without.context.documents.is_empty());
        settings.use_connectors = true;
        let with = build_bounded_run_context(vec![session("s", 0, 1)], &settings, now);
        assert_eq!(with.context.documents.len(), 1);
    }

    #[test]
    fn connector_documents_are_capped_across_sessions() {
        let now = Utc.with_ymd_and_hms(2026, 7, 21, 12, 0, 0).unwrap();
        let settings = BrainSettings {
            enabled: true,
            use_connectors: true,
            focus_instructions: None,
            last_run_at: None,
        };
        let sessions = (0..30)
            .map(|i| session(&format!("doc-cap-{i}"), 0, 1))
            .collect::<Vec<_>>();

        let selection = build_bounded_run_context(sessions, &settings, now);

        assert_eq!(selection.context.documents.len(), BRAIN_MAX_DOCS);
    }

    #[test]
    fn reads_jsonl_session_and_discovers_file_sources() {
        let tmp = tempfile::TempDir::new().unwrap();
        let session_dir = tmp.path().join("sessions/cwd/session-1");
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(
            session_dir.join("summary.json"),
            r#"{"info":{"id":"session-1"},"session_summary":"Demo","updated_at":"2026-07-21T12:00:00Z"}"#,
        )
        .unwrap();
        std::fs::write(
            session_dir.join("chat_history.jsonl"),
            r#"{"type":"user","content":[{"type":"text","text":"Remember file://docs/brain.md"}]}
{"type":"user","content":[{"type":"text","text":"<system-reminder>skip</system-reminder>"}],"synthetic_reason":"system_reminder"}
{"type":"assistant","content":"Read <file_contents path=\"/repo/notes.txt\">x</file_contents>"}
"#,
        )
        .unwrap();
        let sessions = read_persisted_sessions(tmp.path()).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].lines.len(), 2);
        assert_eq!(sessions[0].documents.len(), 2);
        assert!(
            sessions[0]
                .documents
                .iter()
                .any(|d| d.id == "file://docs/brain.md")
        );
        assert!(
            sessions[0]
                .documents
                .iter()
                .any(|d| d.id == "/repo/notes.txt")
        );
    }
    /// The mirror must stay in step with the canonical list in
    /// `xai_file_utils::events::types::MEASUREMENT_ONLY_EVENT_TYPES`. Nothing
    /// enforces that across the crate boundary — this crate takes no workspace
    /// dependencies — so both sides pin the same literal set and a divergence
    /// is one grep away instead of invisible.
    #[test]
    fn the_measurement_only_mirror_is_pinned() {
        assert_eq!(
            super::MEASUREMENT_ONLY_EVENT_TYPES,
            ["goal_role_assignment"],
            "adding an entry here needs a matching entry in xai-file-utils",
        );
    }

    /// Generation-1 red-team finding: the events window keeps only the last
    /// few lines, so appending high-frequency instrumentation evicted real
    /// session events AND replaced them with a content-free excerpt.
    #[test]
    fn measurement_events_do_not_evict_real_session_events() {
        let tmp = tempfile::TempDir::new().unwrap();
        let session_dir = tmp.path().join("sessions/cwd/session-evict");
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(
            session_dir.join("summary.json"),
            r#"{"info":{"id":"session-evict"},"session_summary":"Eviction check","updated_at":"2026-07-21T12:00:00Z"}"#,
        )
        .unwrap();
        std::fs::write(
            session_dir.join("chat_history.jsonl"),
            "{\"type\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"do the thing\"}]}\n",
        )
        .unwrap();
        std::fs::write(
            session_dir.join("events.jsonl"),
            concat!(
                r#"{"ts":"1","type":"goal_planner_fired"}"#,
                "\n",
                r#"{"ts":"2","type":"goal_planner_completed"}"#,
                "\n",
                r#"{"ts":"3","type":"turn_completed"}"#,
                "\n",
                r#"{"ts":"4","type":"turn_ended"}"#,
                "\n",
                r#"{"ts":"5","type":"goal_role_assignment","role":"planner","succeeded":true}"#,
                "\n",
                r#"{"ts":"6","type":"goal_role_assignment","role":"skeptic","succeeded":true}"#,
                "\n",
            ),
        )
        .unwrap();

        let sessions = read_persisted_sessions(tmp.path()).unwrap();
        let joined = sessions[0].lines.join("\n");
        assert!(
            !joined.contains("goal_role_assignment"),
            "instrumentation must not enter the extraction context: {joined}",
        );
        for kept in [
            "goal_planner_fired",
            "goal_planner_completed",
            "turn_completed",
            "turn_ended",
        ] {
            assert!(
                joined.contains(kept),
                "{kept} must survive; it was evicted before the filter: {joined}",
            );
        }
    }

    #[test]
    fn reads_grok_artifacts_into_bounded_session_context() {
        let tmp = tempfile::TempDir::new().unwrap();
        let session_dir = tmp.path().join("sessions/cwd/session-artifacts");
        std::fs::create_dir_all(session_dir.join("terminal")).unwrap();
        std::fs::create_dir_all(session_dir.join("mcp")).unwrap();
        std::fs::create_dir_all(session_dir.join("subagents/child")).unwrap();
        std::fs::create_dir_all(session_dir.join("goal")).unwrap();
        std::fs::create_dir_all(session_dir.join("test_reports")).unwrap();
        std::fs::write(
            session_dir.join("summary.json"),
            r#"{"info":{"id":"session-artifacts"},"session_summary":"Artifact Demo","updated_at":"2026-07-21T12:00:00Z"}"#,
        )
        .unwrap();
        std::fs::write(
            session_dir.join("chat_history.jsonl"),
            r#"{"type":"user","content":[{"type":"text","text":"Work on Zephyr"}]}
"#,
        )
        .unwrap();
        std::fs::write(
            session_dir.join("events.jsonl"),
            r#"{"type":"turn_completed","message":"unit tests passed","model_id":"gpt-test"}
"#,
        )
        .unwrap();
        std::fs::write(
            session_dir.join("updates.jsonl"),
            r#"{"method":"session/update","params":{"update":{"sessionUpdate":"tool_call","content":{"text":"cargo test failed then passed"}}}}
"#,
        )
        .unwrap();
        std::fs::write(
            session_dir.join("terminal/call_1.log"),
            "cargo test -p demo\ntest result: ok. 3 passed\n",
        )
        .unwrap();
        std::fs::write(
            session_dir.join("mcp/call_1.json"),
            r#"{"tool_name":"codegraph_explore","result":"found Brain source citation detail"}"#,
        )
        .unwrap();
        std::fs::write(
            session_dir.join("subagents/child/output.json"),
            r#"{"summary":"subagent found the Brain graph issue"}"#,
        )
        .unwrap();
        std::fs::write(
            session_dir.join("goal/plan.md"),
            "# Plan\n## Verification plan\nRun Brain tests\n",
        )
        .unwrap();
        std::fs::write(
            session_dir.join("test_reports/brain.junit.xml"),
            "<testsuite tests=\"1\" failures=\"0\"><testcase name=\"brain\"/></testsuite>",
        )
        .unwrap();
        std::fs::write(
            session_dir.join("plan.json"),
            r#"{"title":"Brain command plan","status":"in_progress"}"#,
        )
        .unwrap();

        let sessions = read_persisted_sessions(tmp.path()).unwrap();
        assert_eq!(sessions.len(), 1);
        let joined = sessions[0].lines.join("\n");
        assert!(joined.contains("Event events.jsonl"));
        assert!(joined.contains("Update updates.jsonl"));
        assert!(joined.contains("Terminal log terminal/call_1.log"));
        assert!(joined.contains("Tool output mcp/call_1.json"));
        assert!(joined.contains("Subagent output subagents/child/output.json"));
        assert!(joined.contains("Goal artifact goal/plan.md"));
        assert!(joined.contains("Test report test_reports/brain.junit.xml"));
        assert!(joined.contains("Plan artifact plan.json"));
        assert!(
            sessions[0]
                .documents
                .iter()
                .any(|doc| doc.label.contains("Terminal log"))
        );
    }
}
