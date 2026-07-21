//! The self-improvement run engine, ported from Onyx
//! `backend/onyx/background/celery/tasks/brain/tasks.py`.
//!
//! The pipeline is the shipped code; only the LLM call is abstracted behind
//! [`ExtractionProvider`] so callers wire a real model and tests inject a
//! deterministic provider.

use std::collections::HashMap;

use chrono::Utc;

use crate::types::{
    MemoryCategory, MemorySourceType, NewPage, PageUpdate, memory_title_for_content,
};
use crate::{BrainError, BrainStore, Result};

/// Cap on pages applied per run (Onyx `BRAIN_MAX_PAGES_PER_RUN`).
pub const BRAIN_MAX_PAGES_PER_RUN: usize = 12;
/// Cap on citations attached per page (Onyx `BRAIN_MAX_SOURCES_PER_PAGE`).
pub const BRAIN_MAX_SOURCES_PER_PAGE: usize = 4;
/// Provenance tag written on engine-created pages (Onyx `BRAIN_SOURCE`).
pub const BRAIN_SOURCE: &str = "brain";
/// Per-message char budget when building the transcript (Onyx
/// `BRAIN_MAX_CHARS_PER_MESSAGE`).
const BRAIN_MAX_CHARS_PER_MESSAGE: usize = 800;
/// Whole-transcript char budget (Onyx `BRAIN_MAX_TRANSCRIPT_CHARS`).
const BRAIN_MAX_TRANSCRIPT_CHARS: usize = 24_000;

/// A recent session feeding the run (stands in for Onyx `ChatSession` +
/// its messages).
#[derive(Debug, Clone)]
pub struct SessionSource {
    /// Opaque session identifier (becomes the citation's `source_id`).
    pub id: String,
    /// Display label (session description); defaults to "Chat session".
    pub label: Option<String>,
    /// Optional deep link back to the session.
    pub url: Option<String>,
    /// Transcript lines, oldest first (already role-labelled or raw text).
    pub lines: Vec<String>,
}

/// A cited document feeding the run when connectors are enabled (stands in
/// for Onyx `SearchDoc`).
#[derive(Debug, Clone)]
pub struct DocumentSource {
    /// Opaque document identifier.
    pub id: String,
    /// Display label (semantic id / title).
    pub label: String,
    /// Optional link.
    pub url: Option<String>,
    /// Short excerpt included in the source material.
    pub blurb: Option<String>,
}

/// The context a caller assembles for one run.
#[derive(Debug, Clone, Default)]
pub struct RunContext {
    /// Recent sessions, newest first. Labeled `[S1]`, `[S2]`, ... in order.
    pub sessions: Vec<SessionSource>,
    /// Cited documents; only used when `use_connectors` is enabled in
    /// settings. Labeled `[D1]`, `[D2]`, ...
    pub documents: Vec<DocumentSource>,
}

/// A citation the provider can reference by its short ref id ("S1", "D2"),
/// resolved to a concrete source row when pages are applied (Onyx
/// `_SourceRef`).
#[derive(Debug, Clone, PartialEq)]
pub struct SourceRef {
    /// Canonical ref key ("S1", "D2").
    pub ref_id: String,
    /// The citation's source type.
    pub source_type: MemorySourceType,
    /// Display label.
    pub label: String,
    /// Opaque source identifier.
    pub source_id: String,
    /// Optional deep link.
    pub url: Option<String>,
}

/// What the provider sees: the numbered source material plus steering
/// context (Onyx builds this into `_BRAIN_EXTRACT_PROMPT`).
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractionInput {
    /// The `[S#]`/`[D#]`-tagged source material.
    pub transcript: String,
    /// Titles of the user's existing pages (so the provider updates instead
    /// of duplicating).
    pub existing_titles: Vec<String>,
    /// The user's focus instructions for this run, when set.
    pub focus_instructions: Option<String>,
    /// Maximum pages the provider should return.
    pub max_pages: usize,
}

/// One structured page returned by the provider (Onyx `_BrainPage`).
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedPage {
    /// Page title (reuse an existing title to update that page).
    pub title: String,
    /// Category value; unknown/missing values fall back to notes.
    pub category: String,
    /// Page body.
    pub content: String,
    /// Titles of related pages to link.
    pub related: Vec<String>,
    /// Source refs this page was derived from (tolerates `[s1]`-style drift).
    pub sources: Vec<String>,
}

/// The pluggable extraction step: given the built context, produce pages.
/// Production wires an LLM; tests inject a deterministic implementation.
pub trait ExtractionProvider {
    /// Extract durable memory pages from the input material.
    fn extract(&self, input: &ExtractionInput) -> anyhow::Result<Vec<ExtractedPage>>;
}

/// Outcome of one self-improvement run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    /// Settings have the enabled flag off; nothing ran, timestamp untouched.
    Disabled,
    /// The run completed but produced no context or no pages; the run
    /// timestamp is still stamped (Onyx marks empty runs complete too).
    NoPages,
    /// Pages were applied; `applied` counts created-or-updated pages.
    Applied {
        /// Number of pages created or updated.
        applied: usize,
    },
}

/// Normalize a provider-supplied source ref (e.g. `"[s1]"`) to a source-map
/// key (`"S1"`), hardening provenance against bracket-echo and case drift
/// (Onyx `_normalize_source_ref`).
pub fn normalize_source_ref(reference: &str) -> String {
    reference
        .trim()
        .trim_matches(|c| c == '[' || c == ']')
        .trim()
        .to_uppercase()
}

/// Build the `[S#]`/`[D#]` transcript and the ref->source map from the run
/// context (Onyx `_build_context`). Documents are included only when
/// `use_connectors` is true.
fn build_context(
    context: &RunContext,
    use_connectors: bool,
) -> (String, HashMap<String, SourceRef>) {
    let mut source_map: HashMap<String, SourceRef> = HashMap::new();
    let mut parts: Vec<String> = Vec::new();
    let mut total = 0usize;

    for (index, session) in context.sessions.iter().enumerate() {
        let ref_id = format!("S{}", index + 1);
        source_map.insert(
            ref_id.clone(),
            SourceRef {
                ref_id: ref_id.clone(),
                source_type: MemorySourceType::ChatSession,
                label: session
                    .label
                    .clone()
                    .filter(|label| !label.trim().is_empty())
                    .unwrap_or_else(|| "Chat session".to_owned()),
                source_id: session.id.clone(),
                url: session.url.clone(),
            },
        );
        for line in &session.lines {
            let text = line.trim();
            if text.is_empty() {
                continue;
            }
            let clipped: String = text.chars().take(BRAIN_MAX_CHARS_PER_MESSAGE).collect();
            let entry = format!("[{ref_id}] {clipped}\n");
            if total + entry.len() > BRAIN_MAX_TRANSCRIPT_CHARS {
                break;
            }
            total += entry.len();
            parts.push(entry);
        }
    }

    if use_connectors && !context.documents.is_empty() {
        parts.push("\nCited documents:\n".to_owned());
        for (index, doc) in context.documents.iter().enumerate() {
            let ref_id = format!("D{}", index + 1);
            source_map.insert(
                ref_id.clone(),
                SourceRef {
                    ref_id: ref_id.clone(),
                    source_type: MemorySourceType::Document,
                    label: doc.label.clone(),
                    source_id: doc.id.clone(),
                    url: doc.url.clone(),
                },
            );
            let blurb: String = doc
                .blurb
                .as_deref()
                .unwrap_or("")
                .trim()
                .chars()
                .take(300)
                .collect();
            let entry = format!("[{ref_id}] {}: {blurb}\n", doc.label);
            if total + entry.len() > BRAIN_MAX_TRANSCRIPT_CHARS {
                break;
            }
            total += entry.len();
            parts.push(entry);
        }
    }

    (parts.concat().trim().to_owned(), source_map)
}

/// Attach the resolved refs to a page as citations, deduped by `source_id`
/// against what is already attached and capped per page (Onyx
/// `_attach_sources`).
fn attach_sources(store: &BrainStore, memory_id: i64, refs: &[&SourceRef]) -> Result<()> {
    if refs.is_empty() {
        return Ok(());
    }
    let mut existing: Vec<Option<String>> = store
        .sources(memory_id)?
        .into_iter()
        .map(|source| source.source_id)
        .collect();
    for source_ref in refs.iter().take(BRAIN_MAX_SOURCES_PER_PAGE) {
        let key = Some(source_ref.source_id.clone());
        if existing.contains(&key) {
            continue;
        }
        store.add_source(
            memory_id,
            source_ref.source_type,
            &source_ref.label,
            Some(&source_ref.source_id),
            source_ref.url.as_deref(),
        )?;
        existing.push(key);
    }
    Ok(())
}

/// Apply extracted pages: create or (matching on the normalized stored title)
/// update pages, attach per-page sources via the normalized refs, then link
/// related pages once every page has an id (Onyx `_apply_pages`).
fn apply_pages(
    store: &BrainStore,
    pages: &[ExtractedPage],
    source_map: &HashMap<String, SourceRef>,
) -> Result<usize> {
    // title-key -> page id for the user's existing pages.
    let mut by_title: HashMap<String, i64> = store
        .list_pages()?
        .into_iter()
        .map(|page| (page.title.trim().to_lowercase(), page.id))
        .collect();

    let mut applied: HashMap<String, i64> = HashMap::new();
    for page in pages.iter().take(BRAIN_MAX_PAGES_PER_RUN) {
        let title = page.title.trim();
        let content = page.content.trim();
        if title.is_empty() || content.is_empty() {
            continue;
        }
        let category = MemoryCategory::parse(&page.category).unwrap_or(MemoryCategory::Notes);
        let page_refs: Vec<&SourceRef> = page
            .sources
            .iter()
            .filter_map(|reference| source_map.get(&normalize_source_ref(reference)))
            .collect();

        // Match the stored (normalized) title so re-runs update rather than
        // duplicate a near-identical page.
        let key = memory_title_for_content(content, Some(title))
            .trim()
            .to_lowercase();
        let memory_id = match by_title.get(&key) {
            Some(&existing_id) => {
                store.update_page(
                    existing_id,
                    PageUpdate {
                        title: Some(title.to_owned()),
                        memory_text: Some(content.to_owned()),
                        category: Some(category),
                        source: Some(BRAIN_SOURCE.to_owned()),
                    },
                )?;
                existing_id
            }
            None => {
                store
                    .create_page(NewPage {
                        title: Some(title.to_owned()),
                        memory_text: content.to_owned(),
                        category,
                        source: Some(BRAIN_SOURCE.to_owned()),
                    })?
                    .id
            }
        };
        by_title.insert(key.clone(), memory_id);
        applied.insert(key, memory_id);
        attach_sources(store, memory_id, &page_refs)?;
    }

    // Link related pages once every page has an id.
    for page in pages.iter().take(BRAIN_MAX_PAGES_PER_RUN) {
        let source_key = memory_title_for_content(page.content.trim(), Some(page.title.trim()))
            .trim()
            .to_lowercase();
        let Some(&source_id) = applied.get(&source_key) else {
            continue;
        };
        for related_title in &page.related {
            let related_key = related_title.trim().to_lowercase();
            if let Some(&target_id) = by_title.get(&related_key)
                && target_id != source_id
            {
                store.add_relation(source_id, target_id)?;
            }
        }
    }

    Ok(applied.len())
}

/// Run one self-improvement pass (Onyx `_run_for_user` + the task gating).
///
/// - No-op returning [`RunOutcome::Disabled`] when settings have the enabled
///   flag off (the timestamp stays untouched).
/// - Builds the transcript from `context` (documents only when
///   `use_connectors` is set), passes the user's focus instructions and
///   existing page titles to the provider, applies the extracted pages, and
///   stamps `last_run_at` on every completed (even empty) run.
pub fn run_self_improvement(
    store: &BrainStore,
    context: &RunContext,
    provider: &dyn ExtractionProvider,
) -> Result<RunOutcome> {
    let settings = store.settings()?;
    if !settings.enabled {
        return Ok(RunOutcome::Disabled);
    }

    let now = Utc::now();
    let (transcript, source_map) = build_context(context, settings.use_connectors);
    if transcript.is_empty() {
        store.mark_run_complete(now)?;
        return Ok(RunOutcome::NoPages);
    }

    let existing_titles = store
        .list_pages()?
        .into_iter()
        .map(|page| {
            if page.title.is_empty() {
                "Untitled memory".to_owned()
            } else {
                page.title
            }
        })
        .collect();

    let input = ExtractionInput {
        transcript,
        existing_titles,
        focus_instructions: settings.focus_instructions.clone(),
        max_pages: BRAIN_MAX_PAGES_PER_RUN,
    };
    let pages = provider.extract(&input).map_err(BrainError::Provider)?;

    let applied = if pages.is_empty() {
        0
    } else {
        apply_pages(store, &pages, &source_map)?
    };
    store.mark_run_complete(now)?;

    if applied == 0 {
        tracing::debug!("brain run produced no pages");
        Ok(RunOutcome::NoPages)
    } else {
        tracing::debug!(applied, "brain run applied pages");
        Ok(RunOutcome::Applied { applied })
    }
}
