//! Domain types for the Brain memory graph, mirroring the Onyx enums and
//! Pydantic models (`MemoryCategory`, `MemorySourceType`, `BrainSettings`,
//! `MemoryGraph{Node,Edge}`).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Maximum stored title length (Onyx `MEMORY_TITLE_MAX_LENGTH`).
pub(crate) const MEMORY_TITLE_MAX_LENGTH: usize = 200;

/// The four Brain page categories (Onyx `MemoryCategory`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryCategory {
    /// Simple durable details that fit nowhere else. The default category.
    Notes,
    /// Reusable ideas, artifacts, or definitions.
    Concepts,
    /// People, organizations, products, or systems.
    Entities,
    /// Ongoing initiatives or projects.
    Workstreams,
}

impl MemoryCategory {
    /// Stable lowercase wire value, matching the Onyx enum values.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Notes => "notes",
            Self::Concepts => "concepts",
            Self::Entities => "entities",
            Self::Workstreams => "workstreams",
        }
    }

    /// Parse a wire value, tolerating case drift. Unknown values map to
    /// `None`; the engine falls back to [`MemoryCategory::Notes`] like Onyx.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "notes" => Some(Self::Notes),
            "concepts" => Some(Self::Concepts),
            "entities" => Some(Self::Entities),
            "workstreams" => Some(Self::Workstreams),
            _ => None,
        }
    }

    /// All categories in the Onyx display order.
    pub fn all() -> [Self; 4] {
        [
            Self::Notes,
            Self::Concepts,
            Self::Entities,
            Self::Workstreams,
        ]
    }
}

/// The kind of source a citation points at (Onyx `MemorySourceType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemorySourceType {
    /// A chat session the page was derived from.
    ChatSession,
    /// An indexed document cited in a session.
    Document,
    /// A connector-level source.
    Connector,
    /// An uploaded file.
    File,
    /// A manually-attached citation.
    Manual,
}

impl MemorySourceType {
    /// Stable lowercase wire value, matching the Onyx enum values.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ChatSession => "chat_session",
            Self::Document => "document",
            Self::Connector => "connector",
            Self::File => "file",
            Self::Manual => "manual",
        }
    }

    /// Parse a stored wire value.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "chat_session" => Some(Self::ChatSession),
            "document" => Some(Self::Document),
            "connector" => Some(Self::Connector),
            "file" => Some(Self::File),
            "manual" => Some(Self::Manual),
            _ => None,
        }
    }
}

/// Scope kind for a Brain page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryScopeKind {
    /// User-global memory, available in every workspace.
    Global,
    /// Workspace/repo-scoped memory, preferred only when the active workspace matches.
    Workspace,
}

impl MemoryScopeKind {
    /// Stable lowercase DB/wire value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Workspace => "workspace",
        }
    }

    /// Parse a stored value. Unknown values fall back to global for compatibility.
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "workspace" => Self::Workspace,
            _ => Self::Global,
        }
    }
}

/// Freshness semantics for a Brain page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryFreshness {
    /// Durable knowledge that should be treated as broadly reusable.
    Durable,
    /// Operational state that can become stale and should be verified before use.
    TimeSensitive,
}

impl MemoryFreshness {
    /// Stable lowercase DB/wire value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Durable => "durable",
            Self::TimeSensitive => "time_sensitive",
        }
    }

    /// Parse a stored value. Unknown values fall back to durable for compatibility.
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "time_sensitive" | "time-sensitive" | "current_state" => Self::TimeSensitive,
            _ => Self::Durable,
        }
    }
}

/// A stored Brain memory page (Onyx `Memory` row).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryPage {
    /// Row id; stable across updates.
    pub id: i64,
    /// Display title (normalized, capped at 200 chars).
    pub title: String,
    /// The page body.
    pub memory_text: String,
    /// One of the four Brain categories.
    pub category: MemoryCategory,
    /// Scope kind (`global` or `workspace`).
    pub scope_kind: MemoryScopeKind,
    /// Opaque scope id when scoped (e.g. normalized workspace path).
    pub scope_id: Option<String>,
    /// Provenance tag (e.g. `"brain"` for engine-created pages).
    pub source: Option<String>,
    /// Freshness semantics for recall and display.
    pub freshness: MemoryFreshness,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Last-update timestamp.
    pub updated_at: DateTime<Utc>,
}

/// Input for creating a page.
#[derive(Debug, Clone)]
pub struct NewPage {
    /// Optional title; when empty, derived from the first sentence of the text
    /// (Onyx `memory_title_for_content`).
    pub title: Option<String>,
    /// The page body.
    pub memory_text: String,
    /// Category; engine-created pages default unknown categories to notes.
    pub category: MemoryCategory,
    /// Provenance tag.
    pub source: Option<String>,
}

/// Patch for updating a page; `None` fields are left untouched.
#[derive(Debug, Clone, Default)]
pub struct PageUpdate {
    /// New title (normalized/capped on write).
    pub title: Option<String>,
    /// New body text.
    pub memory_text: Option<String>,
    /// New category.
    pub category: Option<MemoryCategory>,
    /// New provenance tag.
    pub source: Option<String>,
    /// New freshness semantics.
    pub freshness: Option<MemoryFreshness>,
}

/// A typed citation attached to a page (Onyx `MemorySource` row).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemorySource {
    /// Row id.
    pub id: i64,
    /// The page this citation belongs to.
    pub memory_id: i64,
    /// The kind of source.
    pub source_type: MemorySourceType,
    /// Opaque identifier of the source in its own domain (e.g. session id).
    pub source_id: Option<String>,
    /// Human-readable label (capped at 512 chars like Onyx).
    pub label: String,
    /// Optional deep link.
    pub url: Option<String>,
    /// Attachment timestamp.
    pub created_at: DateTime<Utc>,
}

/// Per-user Brain settings (Onyx `User.brain_*` columns). Defaults match the
/// Onyx column defaults: disabled, no connectors, no focus text, never run.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BrainSettings {
    /// Master switch for the self-improvement run.
    pub enabled: bool,
    /// Whether cited documents/connectors may feed the run.
    pub use_connectors: bool,
    /// Optional steering text passed into the extraction prompt.
    pub focus_instructions: Option<String>,
    /// When the run last completed.
    pub last_run_at: Option<DateTime<Utc>>,
}

/// Patch for updating settings. Mirrors Onyx `update_brain_settings`: omitted
/// fields stay untouched; `focus_instructions: Some(None)` clears the text
/// (the Onyx `_UNSET` sentinel maps to the outer `Option`).
#[derive(Debug, Clone, Default)]
pub struct BrainSettingsUpdate {
    /// New enabled flag, when provided.
    pub enabled: Option<bool>,
    /// New use-connectors flag, when provided.
    pub use_connectors: Option<bool>,
    /// `Some(Some(text))` sets, `Some(None)` clears, `None` leaves untouched.
    /// Set text is whitespace-trimmed; an all-whitespace string clears.
    pub focus_instructions: Option<Option<String>>,
}

/// A node in the user's memory graph (Onyx `MemoryGraphNode`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryGraphNode {
    /// Page id.
    pub id: i64,
    /// Page title (`"Untitled memory"` when blank, like Onyx).
    pub title: String,
    /// Page category.
    pub category: MemoryCategory,
    /// Number of undirected edges touching this node. Degree-0 nodes are
    /// included, not dropped.
    pub degree: usize,
    /// Page last-update timestamp.
    pub updated_at: DateTime<Utc>,
}

/// An undirected edge in the memory graph (stored once per unordered pair).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryGraphEdge {
    /// Lower page id of the pair.
    pub source: i64,
    /// Higher page id of the pair.
    pub target: i64,
}

/// The full graph: every page as a node plus the undirected edges.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryGraph {
    /// All of the user's pages, newest-updated first.
    pub nodes: Vec<MemoryGraphNode>,
    /// All undirected edges between those pages.
    pub edges: Vec<MemoryGraphEdge>,
}

/// Aggregated Brain store status for slash-command/TUI display.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrainStatus {
    /// Current settings row.
    pub settings: BrainSettings,
    /// Total pages in the store.
    pub page_count: usize,
    /// Total relation rows.
    pub relation_count: usize,
    /// Total source citation rows.
    pub source_count: usize,
    /// Per-category page counts.
    pub category_counts: std::collections::BTreeMap<MemoryCategory, usize>,
    /// Number of global pages.
    pub global_count: usize,
    /// Number of workspace-scoped pages.
    pub workspace_count: usize,
    /// Total revision rows.
    pub revision_count: usize,
    /// Total stored procedures (procedural memory).
    #[serde(default)]
    pub procedure_count: usize,
    /// Relation-graph health, or `None` when the store has no pages.
    ///
    /// Surfaced here so graph degeneration is actually reported somewhere
    /// rather than being computable in principle: a memory graph only accretes,
    /// and both degenerate directions are silent unless something reads them.
    #[serde(default)]
    pub graph_regime: Option<crate::GraphRegime>,
}

/// One recorded page revision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryRevision {
    /// Revision row id.
    pub id: i64,
    /// Memory page id.
    pub memory_id: i64,
    /// Snapshot title.
    pub title: String,
    /// Snapshot body.
    pub memory_text: String,
    /// Snapshot category.
    pub category: MemoryCategory,
    /// Snapshot scope kind.
    pub scope_kind: MemoryScopeKind,
    /// Snapshot scope id.
    pub scope_id: Option<String>,
    /// Snapshot provenance tag.
    pub source: Option<String>,
    /// Snapshot freshness semantics.
    pub freshness: MemoryFreshness,
    /// Why this revision was recorded (`create`, `update`, `restore`, etc.).
    pub revision_source: String,
    /// Revision timestamp.
    pub created_at: DateTime<Utc>,
}

/// Recall configuration used by query-aware Brain context building.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecallOptions {
    /// Natural-language query to rank against.
    pub query: String,
    /// Active workspace/repo scope id, when known.
    pub workspace_scope: Option<String>,
    /// Maximum pages to return.
    pub limit: usize,
}

/// A recalled page plus score metadata and source labels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecalledMemoryPage {
    /// The page.
    pub page: MemoryPage,
    /// Ranking score.
    pub score: i64,
    /// Human-readable source labels for prompt display.
    pub source_labels: Vec<String>,
}

/// Related pages for one page, grouped by category (Onyx
/// `GET /memory/{id}/related` shape).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelatedPages {
    /// Neighbors in the `notes` category.
    pub notes: Vec<MemoryPage>,
    /// Neighbors in the `concepts` category.
    pub concepts: Vec<MemoryPage>,
    /// Neighbors in the `entities` category.
    pub entities: Vec<MemoryPage>,
    /// Neighbors in the `workstreams` category.
    pub workstreams: Vec<MemoryPage>,
}

impl RelatedPages {
    /// True when there are no neighbors in any category.
    pub fn is_empty(&self) -> bool {
        self.notes.is_empty()
            && self.concepts.is_empty()
            && self.entities.is_empty()
            && self.workstreams.is_empty()
    }

    /// Total neighbor count across categories.
    pub fn len(&self) -> usize {
        self.notes.len() + self.concepts.len() + self.entities.len() + self.workstreams.len()
    }
}

/// Derive the stored title for a page: a normalized explicit title when
/// present, otherwise the first sentence of the text, capped at 200 chars
/// (Onyx `memory_title_for_content`).
pub(crate) fn memory_title_for_content(memory_text: &str, title: Option<&str>) -> String {
    let normalized_title = normalize_ws(title.unwrap_or(""));
    if !normalized_title.is_empty() {
        return truncate_chars(&normalized_title, MEMORY_TITLE_MAX_LENGTH);
    }
    let normalized_text = normalize_ws(memory_text);
    let first = normalized_text
        .split_once(". ")
        .map(|(head, _)| head)
        .unwrap_or(&normalized_text);
    let first = if first.is_empty() {
        "Untitled memory"
    } else {
        first
    };
    truncate_chars(first, MEMORY_TITLE_MAX_LENGTH)
}

/// Collapse all whitespace runs to single spaces and trim.
fn normalize_ws(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Truncate to at most `max` characters on a char boundary.
pub(crate) fn truncate_chars(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}
