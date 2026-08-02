//! Native web-search provider router used by `web_search` and deep research.
//!
//! The existing Responses-API web search remains available as a fallback, but
//! this module gives Grok the JFC-style provider surface: explicit backend
//! prefixes (`arxiv:`, `openalex:`, `pubmed:`, `wiki:`, `ddg:`, `papers:`,
//! `uni:`, `millionshort:`, `4get:`, etc.), deterministic
//! routing, partial-failure-tolerant fusion, citations, and clear setup errors
//! for feature-gated credential-backed providers.

pub mod backends;
pub mod router;
pub mod types;
mod util;

pub use router::{BACKEND_PREFIXES, has_backend_prefix, search, split_backend_prefix};
pub use types::{BackendId, ProviderSearchOutput, QueryClass, SearchBackend, SearchResult};
