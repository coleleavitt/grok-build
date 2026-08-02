//! Grok Build integration boundary for the Brain store.
//!
//! This module is intentionally small and deterministic: the shell/session
//! layer can call it from the real prompt path without knowing SQLite details,
//! while tests can drive the exact same entry points with a temp store and a
//! deterministic extraction provider.

use std::path::{Path, PathBuf};

use crate::backfill::{BackfillSelection, read_bounded_run_context};
use crate::engine::{ExtractionProvider, RunContext, RunOutcome, run_self_improvement};
use crate::{
    BrainSettings, BrainSettingsUpdate, BrainStatus, BrainStore, MemoryCategory, MemoryGraph,
    MemoryPage, MemoryRevision, MemorySource, MemorySourceType, NewPage, RecallOptions,
    RecalledMemoryPage, Result,
};

const MAX_RECALLED_PAGES: usize = 20;

/// A user prompt entering Grok Build's request path.
#[derive(Debug, Clone)]
pub struct BrainRequest<'a> {
    /// Stable session id used for source citations.
    pub session_id: &'a str,
    /// Prompt/turn id used for source labels and deep links.
    pub prompt_id: &'a str,
    /// The real user query text, before model sampling.
    pub user_text: &'a str,
    /// Active workspace/repo scope id, when known.
    pub workspace_scope: Option<&'a str>,
}

/// Result of processing one request through the Brain integration boundary.
#[derive(Debug, Clone)]
pub struct BrainRequestOutcome {
    /// Context block to inject into the model request, if any memories exist.
    pub injected_context: Option<String>,
    /// Page created from a remember-style prompt, if one was detected.
    pub remembered_page: Option<MemoryPage>,
}

/// Result of a Grok session-history backfill/self-improvement run.
#[derive(Debug)]
pub struct BrainBackfillOutcome {
    /// Onyx-equivalent selected session/context metadata.
    pub selection: BackfillSelection,
    /// Engine outcome after applying the selected context.
    pub outcome: RunOutcome,
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

        let injected_context =
            self.recall_context_scoped(request.user_text, request.workspace_scope)?;
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
    pub fn recall_context(&self, query: &str) -> Result<Option<String>> {
        self.recall_context_scoped(query, None)
    }

    /// Render recalled pages for an active workspace scope.
    pub fn recall_context_scoped(
        &self,
        query: &str,
        workspace_scope: Option<&str>,
    ) -> Result<Option<String>> {
        if !self.store.settings()?.enabled {
            return Ok(None);
        }
        let pages = self.store.recall_pages(RecallOptions {
            query: query.to_owned(),
            workspace_scope: workspace_scope.map(str::to_owned),
            limit: MAX_RECALLED_PAGES,
        })?;
        if pages.is_empty() {
            return Ok(None);
        }
        Ok(Some(format_recalled_brain_context(pages.into_iter())))
    }

    /// Run deterministic-provider self-improvement through the service boundary.
    pub fn run_self_improvement(
        &self,
        context: &RunContext,
        provider: &dyn ExtractionProvider,
    ) -> Result<RunOutcome> {
        run_self_improvement(&self.store, context, provider)
    }

    /// Read persisted Grok session history from a `~/.grok` root and run the
    /// Onyx-equivalent bounded self-improvement/backfill path.
    pub fn run_backfill_from_grok_home(
        &self,
        grok_home: &Path,
        provider: &dyn ExtractionProvider,
    ) -> Result<BrainBackfillOutcome> {
        self.run_backfill_from_grok_home_at(grok_home, provider, chrono::Utc::now())
    }

    /// Deterministic-time variant used by adversarial tests.
    pub fn run_backfill_from_grok_home_at(
        &self,
        grok_home: &Path,
        provider: &dyn ExtractionProvider,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<BrainBackfillOutcome> {
        let settings = self.store.settings()?;
        if !settings.enabled {
            let selection = read_bounded_run_context(grok_home, &settings, now)?;
            return Ok(BrainBackfillOutcome {
                selection,
                outcome: RunOutcome::Disabled,
            });
        }
        let selection = read_bounded_run_context(grok_home, &settings, now)?;
        let outcome = run_self_improvement(&self.store, &selection.context, provider)?;
        Ok(BrainBackfillOutcome { selection, outcome })
    }

    /// Aggregated status.
    pub fn status(&self) -> Result<BrainStatus> {
        self.store.status()
    }

    /// List pages visible in an optional workspace scope.
    pub fn list_pages(&self, workspace_scope: Option<&str>) -> Result<Vec<MemoryPage>> {
        self.store.list_pages_for_scope(workspace_scope)
    }

    /// Delete a page and everything hanging off it (relations, sources,
    /// revisions cascade in the schema). Returns whether a page was removed.
    ///
    /// Recall injects stored pages into every request, so a wrong or junk page
    /// is not cosmetic — without this the only remedy is editing the SQLite
    /// file by hand.
    pub fn forget_page(&self, id: i64) -> Result<bool> {
        self.store.delete_page(id)
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

    /// List page revisions.
    pub fn revisions(&self, memory_id: i64) -> Result<Vec<MemoryRevision>> {
        self.store.revisions(memory_id)
    }

    /// Restore a page from a revision.
    pub fn restore_revision(&self, revision_id: i64) -> Result<MemoryPage> {
        self.store.restore_revision(revision_id)
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
        let scope = (fact.category != MemoryCategory::Notes)
            .then_some(request.workspace_scope)
            .flatten();
        let page = self.store.create_or_update_page_by_title_scoped(
            NewPage {
                title: Some(fact.title),
                memory_text: fact.content,
                category: fact.category,
                source: Some("request".to_owned()),
            },
            scope,
        )?;
        self.store.add_source_if_missing(
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

/// Maximum length of a remembered fact. A durable memory is a sentence, not a
/// pasted work item; anything longer is a prompt that merely mentions the word
/// "remember".
const REMEMBER_MAX_FACT_CHARS: usize = 320;

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
    // The trigger must open the message or a line inside it. Matching anywhere
    // turned every prompt that merely discusses remembering into a memory page
    // titled with the rest of that prompt.
    let mut fact = None;
    for trigger in triggers {
        let Some(idx) = lower
            .match_indices(trigger)
            .map(|(idx, _)| idx)
            .find(|idx| starts_instruction(&lower, *idx))
        else {
            continue;
        };
        fact = Some(text[idx + trigger.len()..].trim());
        break;
    }
    // A fact is a single statement: stop at the first line break, then at the
    // first sentence end, so a trailing prompt body never lands in the page.
    let fact = first_sentence(fact?.lines().next().unwrap_or_default().trim());
    let fact = fact.trim_matches(|c: char| c == ':' || c == '-' || c.is_whitespace());
    if fact.is_empty() || fact.chars().count() > REMEMBER_MAX_FACT_CHARS {
        return None;
    }

    let (title, content, category) = derive_title_content_category(fact);
    Some(RememberedFact {
        title,
        content,
        category,
    })
}

/// Whether `idx` opens the text or a line within it (allowing a leading
/// bullet/quote marker), i.e. the user is issuing an instruction rather than
/// mentioning the word mid-sentence.
fn starts_instruction(lower: &str, idx: usize) -> bool {
    let line_start = lower[..idx].rfind('\n').map_or(0, |newline| newline + 1);
    lower[line_start..idx]
        .chars()
        .all(|ch| matches!(ch, ' ' | '\t' | '>' | '-' | '*' | '"' | '\'' | '`'))
}

/// Text up to the first sentence end. A period only ends a sentence when
/// whitespace (or the end of the text) follows, so `https://x.ai/v1` and
/// `v1.2.3` survive intact.
fn first_sentence(text: &str) -> &str {
    for (idx, ch) in text.char_indices() {
        if matches!(ch, '.' | '!' | '?') {
            let rest = &text[idx + ch.len_utf8()..];
            if rest.is_empty() || rest.starts_with(char::is_whitespace) {
                return &text[..idx];
            }
        }
    }
    text
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

fn format_recalled_brain_context(pages: impl Iterator<Item = RecalledMemoryPage>) -> String {
    let mut out = String::from(
        "<brain_context>\nThe following durable Brain memories were recalled for this request:\n",
    );
    for recalled in pages {
        let page = recalled.page;
        out.push_str(&format!(
            "- [{}] {}: {}\n",
            page.category.as_str(),
            page.title,
            page.memory_text.trim()
        ));
        if !recalled.source_labels.is_empty() {
            let labels = recalled
                .source_labels
                .iter()
                .take(2)
                .map(|label| label.trim())
                .filter(|label| !label.is_empty())
                .collect::<Vec<_>>();
            if !labels.is_empty() {
                out.push_str(&format!("  Sources: {}\n", labels.join(", ")));
            }
        }
    }
    out.push_str("</brain_context>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{DocumentSource, ExtractedPage, ExtractionInput, SessionSource, SourceRef};
    use std::sync::Mutex;

    /// Real prompts that merely discuss remembering must not become pages.
    /// These two shapes are exactly what polluted a live store: a work-item
    /// paste quoting `Brain Updated: Remembered #12 ...`, and a long question
    /// with "remember" buried mid-sentence.
    #[test]
    fn mid_sentence_mentions_do_not_create_a_fact() {
        for prompt in [
            "Updates: - E.g. `Brain Updated: Remembered #12 [notes] Visible Feedback.` \
             Verification passed: rustfmt --check on the focused Brain/shell files.",
            "how like i could like upload sharepoint documents or select folders like \
             read our existing modals and stuff right, do you remember how sharepoint \
             ingestion worked in the connector",
            "Can you check whether the agent will remember this across sessions?",
            "The tool output said: remember to rerun the gate before merging",
        ] {
            assert_eq!(
                extract_remembered_fact(prompt),
                None,
                "must not extract a fact from: {prompt}"
            );
        }
    }

    /// The instruction forms still work, and only the instruction itself is
    /// stored — not the rest of a long prompt that follows it.
    #[test]
    fn instruction_forms_extract_only_the_first_statement() {
        let fact = extract_remembered_fact(
            "Please remember that my project codename is Zephyr-Nine. Now go run the tests \
             and report back with the full diff, then open a PR.",
        )
        .expect("an explicit instruction is a fact");
        assert_eq!(fact.title, "Project Codename");
        assert_eq!(fact.content, "The user's project codename is Zephyr-Nine.");

        let bulleted = extract_remembered_fact("- remember: the deploy script lives in bin/deploy")
            .expect("a bulleted instruction line is a fact");
        assert!(
            bulleted.content.contains("bin/deploy"),
            "content: {}",
            bulleted.content
        );

        let multiline = extract_remembered_fact(
            "Here is the context I pasted.\nRemember that the ERS staging host is ers-stage-1\n\
             and here is a pile of unrelated log output that must not be stored.",
        )
        .expect("an instruction opening a line is a fact");
        assert!(
            !multiline.content.contains("log output"),
            "content must stop at the line: {}",
            multiline.content
        );
    }

    /// A dotted value is not a sentence boundary.
    #[test]
    fn dotted_values_survive_sentence_trimming() {
        let fact = extract_remembered_fact("remember that the ERS base url is https://ers.x.ai/v1")
            .expect("instruction is a fact");
        assert!(
            fact.content.contains("https://ers.x.ai/v1"),
            "content: {}",
            fact.content
        );
    }

    /// An oversized "fact" is a pasted document, not a memory.
    #[test]
    fn oversized_statements_are_rejected() {
        let long = format!("Remember that {}", "x".repeat(REMEMBER_MAX_FACT_CHARS + 1));
        assert_eq!(extract_remembered_fact(&long), None);
    }

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
                workspace_scope: None,
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
                workspace_scope: None,
            })
            .unwrap();
        let context = later.injected_context.expect("later request recalls page");
        assert!(context.contains("<brain_context>"));
        assert!(context.contains("Zephyr-Nine"));
        assert!(context.contains("Project Codename"));
        assert!(later.remembered_page.is_none());
    }

    #[test]
    fn repeated_remember_updates_existing_page_and_dedups_same_session_source() {
        let service = BrainService::open_in_memory_for_tests();
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
                user_text: "Remember that my project codename is Zephyr-One.",
                workspace_scope: None,
            })
            .unwrap()
            .remembered_page
            .unwrap();
        let second = service
            .process_request(BrainRequest {
                session_id: "session-1",
                prompt_id: "prompt-1",
                user_text: "Remember that my project codename is Zephyr-Two.",
                workspace_scope: None,
            })
            .unwrap()
            .remembered_page
            .unwrap();
        assert_eq!(first.id, second.id, "same title updates existing page");
        assert_eq!(service.store.list_pages().unwrap().len(), 1);
        assert!(second.memory_text.contains("Zephyr-Two"));
        assert_eq!(service.sources(second.id).unwrap().len(), 1);
    }

    #[test]
    fn scoped_recall_context_includes_compact_source_labels() {
        let service = BrainService::open_in_memory_for_tests();
        service
            .update_settings(BrainSettingsUpdate {
                enabled: Some(true),
                ..Default::default()
            })
            .unwrap();
        let global = service
            .store
            .create_page(NewPage {
                title: Some("User Shell Path Preference".to_owned()),
                memory_text: "The user prefers relative paths.".to_owned(),
                category: MemoryCategory::Notes,
                source: None,
            })
            .unwrap();
        let workspace = service
            .store
            .create_page_scoped(
                NewPage {
                    title: Some("ERS Deploy Script".to_owned()),
                    memory_text: "The ERS deploy script validates signing keys before deploy."
                        .to_owned(),
                    category: MemoryCategory::Workstreams,
                    source: None,
                },
                Some("/repo/ers-rs"),
            )
            .unwrap();
        for label in [
            "ERS API Request Signing",
            "Deploy script preflight",
            "Extra source label should be omitted from context",
        ] {
            service
                .store
                .add_source(
                    workspace.id,
                    MemorySourceType::ChatSession,
                    label,
                    None,
                    None,
                )
                .unwrap();
        }

        let context = service
            .recall_context_scoped("deploy signing keys", Some("/repo/ers-rs"))
            .unwrap()
            .expect("enabled store should recall pages");
        let workspace_pos = context.find("ERS Deploy Script").unwrap();
        let global_pos = context.find("User Shell Path Preference").unwrap();
        assert!(
            workspace_pos < global_pos,
            "query/workspace page should rank first: {context}"
        );
        assert!(context.contains("Sources: ERS API Request Signing, Deploy script preflight"));
        assert!(
            !context.contains("Extra source label should be omitted"),
            "context should keep citations compact: {context}",
        );
        assert!(context.contains(&format!("[{}]", MemoryCategory::Workstreams.as_str())));
        assert!(context.contains(&global.title));
    }

    #[test]
    fn disabled_settings_do_not_create_or_recall() {
        let service = BrainService::open_in_memory_for_tests();
        let outcome = service
            .process_request(BrainRequest {
                session_id: "s",
                prompt_id: "p",
                user_text: "Remember that my favorite language is Rust.",
                workspace_scope: None,
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
