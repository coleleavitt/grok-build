//! Bounded Grok session-history backfill for Brain.
//!
//! This ports Onyx `brain/tasks.py` run semantics to Grok Build's local JSONL
//! session model: recent-session/last-run cutoff, session/message/transcript/doc
//! caps, `[S#]`/`[D#]` source-map construction, connector/document toggling via
//! settings, provider extraction, page/source/relation application, and
//! last-run stamping.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
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
    now: DateTime<Utc>,
) -> BackfillSelection {
    let default_cutoff = now - Duration::days(BRAIN_LOOKBACK_DAYS);
    let cutoff = settings
        .last_run_at
        .filter(|last_run| *last_run > default_cutoff)
        .unwrap_or(default_cutoff);

    sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    let sessions: Vec<_> = sessions
        .into_iter()
        .take(BRAIN_MAX_SESSIONS_PER_RUN)
        .filter(|session| session.updated_at >= cutoff)
        .collect();

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
        context_sessions.push(SessionSource {
            id: session.id.clone(),
            label: Some(session.label.clone()),
            url: Some(format!("grok://session/{}", session.id)),
            lines,
        });
        if settings.use_connectors {
            for doc in &session.documents {
                if docs.iter().any(|existing: &DocumentSource| existing.id == doc.id) {
                    continue;
                }
                docs.push(doc.clone());
                if docs.len() >= BRAIN_MAX_DOCS {
                    break;
                }
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
        for session_entry in fs::read_dir(cwd_entry.path())? {
            let session_entry = session_entry?;
            if !session_entry.file_type()?.is_dir() {
                continue;
            }
            if let Some(session) = read_session_dir(&session_entry.path())? {
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
        .or_else(|| dir.file_name().map(|name| name.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "unknown-session".to_owned());
    let updated_at = summary
        .updated_at
        .or(summary.created_at)
        .unwrap_or_else(Utc::now);
    let label = summary
        .session_summary
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("Grok session {id}"));
    let (lines, documents) = read_chat_history(&chat_path)?;
    if lines.is_empty() {
        return Ok(None);
    }
    Ok(Some(PersistedBrainSession {
        id,
        label,
        updated_at,
        lines,
        documents,
    }))
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
            let end = raw
                .find([')', ']', '>', '"', '\''])
                .unwrap_or(raw.len());
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
    Assistant { content: String },
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
    use chrono::TimeZone;

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
        }
    }

    #[test]
    fn bounded_context_applies_recent_window_last_run_and_caps() {
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
        assert_eq!(selection.sessions.len(), 6, "days 0..5 survive cutoff");
        assert_eq!(selection.context.sessions.len(), 6);
        assert!(selection
            .context
            .sessions
            .iter()
            .all(|s| s.lines.len() == BRAIN_MAX_MESSAGES_PER_SESSION));
        assert_eq!(selection.context.documents.len(), 6);
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
        assert!(sessions[0].documents.iter().any(|d| d.id == "file://docs/brain.md"));
        assert!(sessions[0].documents.iter().any(|d| d.id == "/repo/notes.txt"));
    }
}
