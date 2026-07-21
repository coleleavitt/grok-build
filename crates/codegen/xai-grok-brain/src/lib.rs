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

mod engine_impl;
mod service;
mod store;
#[cfg(test)]
mod tests;
mod types;

pub use service::{BrainRequest, BrainRequestOutcome, BrainService, default_store_path};
pub use store::BrainStore;
pub use types::{
    BrainSettings, BrainSettingsUpdate, MemoryCategory, MemoryGraph, MemoryGraphEdge,
    MemoryGraphNode, MemoryPage, MemorySource, MemorySourceType, NewPage, PageUpdate, RelatedPages,
};

pub mod engine {
    //! Self-improvement run: context building, extraction, and application.
    pub use crate::engine_impl::{
        BRAIN_MAX_PAGES_PER_RUN, BRAIN_MAX_SOURCES_PER_PAGE, BRAIN_SOURCE, DocumentSource,
        ExtractedPage, ExtractionInput, ExtractionProvider, RunContext, RunOutcome, SessionSource,
        SourceRef, normalize_source_ref, run_self_improvement,
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
    /// The extraction provider failed.
    #[error("extraction provider error: {0}")]
    Provider(#[source] anyhow::Error),
}

/// Convenience result alias for Brain operations.
pub type Result<T, E = BrainError> = std::result::Result<T, E>;
