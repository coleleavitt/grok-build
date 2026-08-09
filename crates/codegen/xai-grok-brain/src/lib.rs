//! Brain: a self-improving memory graph, ported from Onyx's Perplexity-"Brain"
//! parity feature (`backend/onyx/db/brain.py` + `brain/tasks.py`).
//!
//! The crate provides the domain/data layer and the run engine:
//! - [`BrainStore`]: a durable SQLite store of memory pages, undirected
//!   page-to-page relations, typed source citations, and brain settings.
//! - [`engine`]: the self-improvement run that turns recent session context
//!   into categorized, source-cited, cross-linked memory pages via a pluggable
//!   [`engine::ExtractionProvider`].
//!
//! HTTP endpoints, UI, and scheduling are intentionally out of scope; callers
//! invoke [`engine::run_self_improvement`] themselves.

mod backfill;
mod engine_impl;
mod health;
mod procedure;
mod search;
mod service;
mod store;
#[cfg(test)]
mod tests;
mod types;

pub use backfill::{
    BRAIN_LOOKBACK_DAYS, BRAIN_MAX_CHARS_PER_MESSAGE, BRAIN_MAX_DOCS,
    BRAIN_MAX_MESSAGES_PER_SESSION, BRAIN_MAX_SESSIONS_PER_RUN, BRAIN_MAX_TRANSCRIPT_CHARS,
    BackfillSelection, PersistedBrainSession, build_bounded_run_context, read_bounded_run_context,
    read_persisted_sessions,
};
pub use health::{GraphHealth, GraphRegime, HealthThresholds};
pub use procedure::{Procedure, ProcedureOutcome, ProcedureRecallOptions, RecalledProcedure};
pub use search::{
    BrainEmbeddingProvider, BrainSearchEngine, BrainSearchMode, BrainSearchOptions,
    BrainSearchOutcome, PreparedBrainSearch,
};
pub use service::{
    BrainBackfillOutcome, BrainRequest, BrainRequestOutcome, BrainService, CurrentStateMemory,
    default_store_path,
};
pub use store::BrainStore;
pub use types::{
    BrainSettings, BrainSettingsUpdate, BrainStatus, MemoryCategory, MemoryFreshness, MemoryGraph,
    MemoryGraphEdge, MemoryGraphNode, MemoryPage, MemoryRevision, MemoryScopeKind, MemorySource,
    MemorySourceType, NewPage, PageUpdate, RecallOptions, RecalledMemoryPage, RelatedPages,
};

pub mod engine {
    //! Self-improvement run: context building, extraction, and application.
    pub use crate::engine_impl::{
        BRAIN_MAX_PAGES_PER_RUN, BRAIN_MAX_SOURCES_PER_PAGE, BRAIN_SOURCE, DocumentSource,
        ExtractedPage, ExtractionInput, ExtractionProvider, PreparedRunOutcome,
        PreparedSelfImprovementRun, RunContext, RunOutcome, SessionSource, SourceRef,
        complete_self_improvement, normalize_source_ref, prepare_self_improvement,
        run_self_improvement,
    };
}

/// Errors surfaced by the Brain store and engine.
#[derive(Debug, thiserror::Error)]
pub enum BrainError {
    /// Underlying SQLite failure.
    #[error("brain storage error: {0}")]
    Storage(#[from] rusqlite::Error),
    /// The referenced memory page does not exist.
    #[error("memory page {0} not found")]
    PageNotFound(i64),
    /// A relation endpoint is invalid (self-edge or unknown page).
    #[error("invalid relation between {0} and {1}: {2}")]
    InvalidRelation(i64, i64, &'static str),
    /// File-system failure while reading persisted Grok sessions.
    #[error("brain session I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// JSON parse failure while reading persisted Grok sessions.
    #[error("brain session JSON error: {0}")]
    Json(#[from] serde_json::Error),
    /// The extraction provider failed.
    #[error("extraction provider error: {0}")]
    Provider(#[source] anyhow::Error),
    /// A caller-supplied value failed a store precondition.
    #[error("invalid brain input: {0}")]
    Validation(String),
}

/// Convenience result alias for Brain operations.
pub type Result<T, E = BrainError> = std::result::Result<T, E>;
