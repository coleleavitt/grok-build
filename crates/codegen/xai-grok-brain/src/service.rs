//! Grok Build integration boundary for the Brain store.
//!
//! This module is intentionally small and deterministic: the shell/session
//! layer can call it from the real prompt path without knowing SQLite details,
//! while tests can drive the exact same entry points with a temp store and a
//! deterministic extraction provider.

use std::path::{Path, PathBuf};

use crate::engine::{ExtractionProvider, RunContext, RunOutcome, run_self_improvement};
use crate::{
    BrainSettings, BrainSettingsUpdate, BrainStore, MemoryCategory, MemoryGraph, MemoryPage,
    MemorySource, MemorySourceType, NewPage, Result,
};

const MAX_RECALLED_PAGES: usize = 12;

/// A user prompt entering Grok Build's request path.
#[derive(Debug, Clone)]
pub struct BrainRequest<'a> {
    /// Stable session id used for source citations.
    pub session_id: &'a str,
    /// Prompt/turn id used for source labels and deep links.
    pub prompt_id: &'a str,
    /// The real user query text, before model sampling.
    pub user_text: &'a str,
}

/// Result of processing one request through the Brain integration boundary.
#[derive(Debug, Clone)]
pub struct BrainRequestOutcome {
    /// Context block to inject into the model request, if any memories exist.
    pub injected_context: Option<String>,
    /// Page created from a remember-style prompt, if one was detected.
    pub remembered_page: Option<MemoryPage>,
}

/// High-level service wrapping the durable Brain store and engine.
#[derive(Debug)]
pub struct BrainService {
    store: BrainStore,
}

impl BrainService {
    /// Open a service at an explicit store path.
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self {
            store: BrainStore::open(path)?,
        })
    }

    /// Open the normal Grok Build Brain store and initialize it enabled when it
    /// has never been configured. An explicit disabled setting remains disabled.
    pub fn open_grok_default() -> Result<Self> {
        let service = Self::open(&default_store_path())?;
        service.initialize_enabled_if_unconfigured()?;
        Ok(service)
    }

    /// The store, for parity/verification checks.
    pub fn store(&self) -> &BrainStore {
        &self.store
    }

    /// Current settings.
    pub fn settings(&self) -> Result<BrainSettings> {
        self.store.settings()
    }

    /// Patch settings.
    pub fn update_settings(&self, update: BrainSettingsUpdate) -> Result<BrainSettings> {
        self.store.update_settings(update)
    }

    /// Process a real request: if Brain is enabled, first recall existing pages
    /// into an injectable context block, then persist a new page when the user
    /// asks us to remember a durable fact.
    pub fn process_request(&self, request: BrainRequest<'_>) -> Result<BrainRequestOutcome> {
        if !self.store.settings()?.enabled {
            return Ok(BrainRequestOutcome {
                injected_context: None,
                remembered_page: None,
            });
        }

        let injected_context = self.recall_context(request.user_text)?;
        let remembered_page = self.remember_from_request(request)?;
        Ok(BrainRequestOutcome {
            injected_context,
            remembered_page,
        })
    }

    /// Render currently stored pages as a model-facing Brain context block.
    /// The first implementation intentionally favors recall determinism over
    /// ranking sophistication: the local CLI store is small, and a complete
    /// bounded block proves the request path receives the durable fact.
    pub fn recall_context(&self, _query: &str) -> Result<Option<String>> {
        if !self.store.settings()?.enabled {
            return Ok(None);
        }
        let pages = self.store.list_pages()?;
        if pages.is_empty() {
            return Ok(None);
        }
        Ok(Some(format_brain_context(
            pages.into_iter().take(MAX_RECALLED_PAGES),
        )))
    }

    /// Run deterministic-provider self-improvement through the service boundary.
    pub fn run_self_improvement(
        &self,
        context: &RunContext,
        provider: &dyn ExtractionProvider,
    ) -> Result<RunOutcome> {
        run_self_improvement(&self.store, context, provider)
    }

    /// Convenience graph accessor used by integration tests and future UI/API
    /// surfaces.
    pub fn graph(&self) -> Result<MemoryGraph> {
        self.store.graph()
    }

    /// Convenience source accessor.
    pub fn sources(&self, memory_id: i64) -> Result<Vec<MemorySource>> {
        self.store.sources(memory_id)
    }

    fn initialize_enabled_if_unconfigured(&self) -> Result<()> {
        if !self.store.settings_initialized()? {
            self.store.update_settings(BrainSettingsUpdate {
                enabled: Some(true),
                ..BrainSettingsUpdate::default()
            })?;
        }
        Ok(())
    }

    fn remember_from_request(&self, request: BrainRequest<'_>) -> Result<Option<MemoryPage>> {
        let Some(fact) = extract_remembered_fact(request.user_text) else {
            return Ok(None);
        };
        let page = self.store.create_page(NewPage {
            title: Some(fact.title),
            memory_text: fact.content,
            category: fact.category,
            source: Some("request".to_owned()),
        })?;
        self.store.add_source(
            page.id,
            MemorySourceType::ChatSession,
            &format!("Grok Build request {}", request.prompt_id),
            Some(request.session_id),
            Some(&format!(
                "grok://session/{}/prompt/{}",
                request.session_id, request.prompt_id
            )),
        )?;
        Ok(Some(page))
    }
}

/// Default durable Brain DB path: `$GROK_BRAIN_DB`, otherwise
/// `$GROK_HOME/brain/brain.sqlite`, otherwise `$HOME/.grok/brain/brain.sqlite`.
pub fn default_store_path() -> PathBuf {
    if let Some(path) = std::env::var_os("GROK_BRAIN_DB") {
        return PathBuf::from(path);
    }
    let root = std::env::var_os("GROK_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".grok")))
        .unwrap_or_else(|| PathBuf::from(".grok"));
    root.join("brain").join("brain.sqlite")
}

/// A deterministic remembered fact extracted from a remember-style prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RememberedFact {
    title: String,
    content: String,
    category: MemoryCategory,
}

fn extract_remembered_fact(input: &str) -> Option<RememberedFact> {
    let text = input.trim();
    if text.is_empty() {
        return None;
    }
    let lower = text.to_ascii_lowercase();
    let triggers = [
        "please remember that",
        "remember that",
        "please remember:",
        "remember:",
        "remember ",
    ];
    let mut fact = None;
    for trigger in triggers {
        if let Some(idx) = lower.find(trigger) {
            let start = idx + trigger.len();
            fact = Some(text[start..].trim());
            break;
        }
    }
    let fact = fact?.trim_matches(|c: char| c == ':' || c == '-' || c.is_whitespace());
    let fact = fact.trim_end_matches(['.', '!', '?']).trim();
    if fact.is_empty() {
        return None;
    }

    let (title, content, category) = derive_title_content_category(fact);
    Some(RememberedFact {
        title,
        content,
        category,
    })
}

fn derive_title_content_category(fact: &str) -> (String, String, MemoryCategory) {
    let lower = fact.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("my ")
        && let Some(is_pos) = rest.find(" is ")
    {
        let subject_start = 3;
        let subject_end = subject_start + is_pos;
        let subject = fact[subject_start..subject_end].trim();
        let value = fact[subject_end + 4..].trim();
        let title = title_case_subject(subject);
        let content = format!("The user's {subject} is {value}.");
        let category = category_for_subject(subject, value);
        return (title, content, category);
    }

    let title = fact
        .split_once(" is ")
        .map(|(head, _)| title_case_subject(head.trim_start_matches("the user' s ")))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Remembered note".to_owned());
    let content = if lower.starts_with("the user") {
        format!("{fact}.")
    } else {
        format!("The user asked to remember: {fact}.")
    };
    let category = category_for_subject(&title, fact);
    (title, content, category)
}

fn category_for_subject(subject: &str, value: &str) -> MemoryCategory {
    let text = format!("{subject} {value}").to_ascii_lowercase();
    if text.contains("project")
        || text.contains("company")
        || text.contains("customer")
        || text.contains("client")
        || text.contains("codename")
    {
        MemoryCategory::Entities
    } else if text.contains("launch") || text.contains("initiative") || text.contains("workstream")
    {
        MemoryCategory::Workstreams
    } else if text.contains("concept") || text.contains("definition") || text.contains("rag") {
        MemoryCategory::Concepts
    } else {
        MemoryCategory::Notes
    }
}

fn title_case_subject(subject: &str) -> String {
    subject
        .split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => {
                    let mut out = first.to_uppercase().collect::<String>();
                    out.push_str(chars.as_str());
                    out
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn format_brain_context(pages: impl Iterator<Item = MemoryPage>) -> String {
    let mut out = String::from(
        "<brain_context>\nThe following durable Brain memories were recalled for this request:\n",
    );
    for page in pages {
        out.push_str(&format!(
            "- [{}] {}: {}\n",
            page.category.as_str(),
            page.title,
            page.memory_text.trim()
        ));
    }
    out.push_str("</brain_context>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{DocumentSource, ExtractedPage, ExtractionInput, SessionSource, SourceRef};
    use std::sync::Mutex;

    #[test]
    fn remember_request_creates_record_source_and_later_context_after_reopen() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("brain.sqlite");
        let service = BrainService::open(&path).unwrap();
        service
            .update_settings(BrainSettingsUpdate {
                enabled: Some(true),
                ..Default::default()
            })
            .unwrap();

        let first = service
            .process_request(BrainRequest {
                session_id: "session-1",
                prompt_id: "prompt-1",
                user_text: "Please remember that my project codename is Zephyr-Nine.",
            })
            .unwrap();
        let page = first
            .remembered_page
            .expect("remember prompt creates a page");
        assert_eq!(page.title, "Project Codename");
        assert_eq!(page.category, MemoryCategory::Entities);
        assert!(page.memory_text.contains("Zephyr-Nine"));
        assert!(first.injected_context.is_none(), "no prior pages existed");
        let sources = service.sources(page.id).unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].source_type, MemorySourceType::ChatSession);
        assert_eq!(sources[0].source_id.as_deref(), Some("session-1"));

        drop(service);
        let reopened = BrainService::open(&path).unwrap();
        let later = reopened
            .process_request(BrainRequest {
                session_id: "session-2",
                prompt_id: "prompt-2",
                user_text: "What is my project codename?",
            })
            .unwrap();
        let context = later.injected_context.expect("later request recalls page");
        assert!(context.contains("<brain_context>"));
        assert!(context.contains("Zephyr-Nine"));
        assert!(context.contains("Project Codename"));
        assert!(later.remembered_page.is_none());
    }

    #[test]
    fn disabled_settings_do_not_create_or_recall() {
        let service = BrainService::open_in_memory_for_tests();
        let outcome = service
            .process_request(BrainRequest {
                session_id: "s",
                prompt_id: "p",
                user_text: "Remember that my favorite language is Rust.",
            })
            .unwrap();
        assert!(outcome.injected_context.is_none());
        assert!(outcome.remembered_page.is_none());
        assert!(service.store.list_pages().unwrap().is_empty());
    }

    struct Provider {
        seen: Mutex<Vec<ExtractionInput>>,
    }

    impl ExtractionProvider for Provider {
        fn extract(&self, input: &ExtractionInput) -> anyhow::Result<Vec<ExtractedPage>> {
            self.seen.lock().unwrap().push(input.clone());
            Ok(vec![
                ExtractedPage {
                    title: "Acme Corp".to_owned(),
                    category: "entities".to_owned(),
                    content: "Acme Corp is the launch customer.".to_owned(),
                    related: vec!["Q3 Launch".to_owned()],
                    sources: vec!["[s1]".to_owned(), "D1".to_owned()],
                },
                ExtractedPage {
                    title: "Q3 Launch".to_owned(),
                    category: "workstreams".to_owned(),
                    content: "Q3 Launch is active.".to_owned(),
                    related: vec!["Acme Corp".to_owned()],
                    sources: vec!["S1".to_owned()],
                },
            ])
        }
    }

    #[test]
    fn service_self_improvement_uses_deterministic_provider() {
        let service = BrainService::open_in_memory_for_tests();
        service
            .update_settings(BrainSettingsUpdate {
                enabled: Some(true),
                use_connectors: Some(true),
                focus_instructions: Some(Some("Focus on launch facts".to_owned())),
            })
            .unwrap();
        let provider = Provider {
            seen: Mutex::new(Vec::new()),
        };
        let outcome = service
            .run_self_improvement(
                &RunContext {
                    sessions: vec![SessionSource {
                        id: "session-1".to_owned(),
                        label: Some("Launch chat".to_owned()),
                        url: None,
                        lines: vec!["User: Acme launch details".to_owned()],
                    }],
                    documents: vec![DocumentSource {
                        id: "doc-1".to_owned(),
                        label: "Launch PRD".to_owned(),
                        url: None,
                        blurb: Some("Acme launch".to_owned()),
                    }],
                },
                &provider,
            )
            .unwrap();
        assert_eq!(outcome, RunOutcome::Applied { applied: 2 });
        assert_eq!(
            provider.seen.lock().unwrap()[0]
                .focus_instructions
                .as_deref(),
            Some("Focus on launch facts")
        );
        let graph = service.graph().unwrap();
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.edges.len(), 1);
        assert!(service.settings().unwrap().last_run_at.is_some());
        let acme = service
            .store
            .list_pages_by_category(MemoryCategory::Entities)
            .unwrap()
            .pop()
            .unwrap();
        let sources = service.sources(acme.id).unwrap();
        assert_eq!(sources.len(), 2);
        assert!(
            sources
                .iter()
                .any(|s| s.source_id.as_deref() == Some("session-1"))
        );
        assert!(
            sources
                .iter()
                .any(|s| s.source_id.as_deref() == Some("doc-1"))
        );

        // Keep SourceRef reachable in this module's public import list; this is
        // a compile-time smoke for the type exported to shell callers.
        let _ = SourceRef {
            ref_id: "S1".to_owned(),
            source_type: MemorySourceType::ChatSession,
            label: "Launch chat".to_owned(),
            source_id: "session-1".to_owned(),
            url: None,
        };
    }

    impl BrainService {
        fn open_in_memory_for_tests() -> Self {
            Self {
                store: BrainStore::open_in_memory().unwrap(),
            }
        }
    }
}
