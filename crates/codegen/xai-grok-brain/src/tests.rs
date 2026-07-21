//! Unit tests for Brain parity semantics, driving the shipped store and
//! engine (no re-implementations).

use std::sync::Mutex;

use chrono::Utc;
use tempfile::TempDir;

use crate::engine::{
    BRAIN_SOURCE, DocumentSource, ExtractedPage, ExtractionInput, ExtractionProvider, RunContext,
    RunOutcome, SessionSource, normalize_source_ref, run_self_improvement,
};
use crate::{
    BrainError, BrainSettingsUpdate, BrainStore, MemoryCategory, MemorySourceType, NewPage,
    PageUpdate,
};

fn page(store: &BrainStore, title: &str, category: MemoryCategory) -> i64 {
    store
        .create_page(NewPage {
            title: Some(title.to_owned()),
            memory_text: format!("{title} body"),
            category,
            source: None,
        })
        .unwrap()
        .id
}

// ---------------------------------------------------------------------------
// Criterion 1: page CRUD + persistence across close/reopen
// ---------------------------------------------------------------------------

#[test]
fn page_crud_persists_across_reopen() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("brain.sqlite");

    let created_id;
    {
        let store = BrainStore::open(&db).unwrap();
        let created = store
            .create_page(NewPage {
                title: Some("Acme Corp".to_owned()),
                memory_text: "Acme is the client.".to_owned(),
                category: MemoryCategory::Entities,
                source: None,
            })
            .unwrap();
        created_id = created.id;
        assert_eq!(created.title, "Acme Corp");
        assert_eq!(created.category, MemoryCategory::Entities);

        // Update: patch only the text; title/category untouched.
        let updated = store
            .update_page(
                created_id,
                PageUpdate {
                    memory_text: Some("Acme is the main client.".to_owned()),
                    ..PageUpdate::default()
                },
            )
            .unwrap();
        assert_eq!(updated.title, "Acme Corp");
        assert_eq!(updated.memory_text, "Acme is the main client.");

        // A second page, then delete it.
        let doomed = page(&store, "Doomed", MemoryCategory::Notes);
        assert!(store.delete_page(doomed).unwrap());
        assert!(store.get_page(doomed).unwrap().is_none());
        assert_eq!(store.list_pages().unwrap().len(), 1);
    } // store dropped: connection closed.

    let reopened = BrainStore::open(&db).unwrap();
    let pages = reopened.list_pages().unwrap();
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].id, created_id);
    assert_eq!(pages[0].memory_text, "Acme is the main client.");
    assert_eq!(pages[0].category, MemoryCategory::Entities);
}

#[test]
fn title_derived_from_first_sentence_when_absent() {
    let store = BrainStore::open_in_memory().unwrap();
    let created = store
        .create_page(NewPage {
            title: None,
            memory_text: "Prefers dark mode. Uses vim keybindings everywhere.".to_owned(),
            category: MemoryCategory::Notes,
            source: None,
        })
        .unwrap();
    assert_eq!(created.title, "Prefers dark mode");
}

#[test]
fn update_missing_page_is_page_not_found() {
    let store = BrainStore::open_in_memory().unwrap();
    let err = store.update_page(999, PageUpdate::default()).unwrap_err();
    assert!(matches!(err, BrainError::PageNotFound(999)));
}

// ---------------------------------------------------------------------------
// Criterion 2: relations + graph
// ---------------------------------------------------------------------------

#[test]
fn relation_dedup_both_directions() {
    let store = BrainStore::open_in_memory().unwrap();
    let a = page(&store, "A", MemoryCategory::Notes);
    let b = page(&store, "B", MemoryCategory::Notes);

    assert!(store.add_relation(a, b).unwrap());
    assert!(store.add_relation(a, b).unwrap()); // same direction
    assert!(store.add_relation(b, a).unwrap()); // reversed direction

    let graph = store.graph().unwrap();
    assert_eq!(graph.edges.len(), 1, "duplicate edges must collapse");
    assert_eq!(store.related_page_ids(a).unwrap(), vec![b]);
}

#[test]
fn self_edge_rejected() {
    let store = BrainStore::open_in_memory().unwrap();
    let a = page(&store, "A", MemoryCategory::Notes);
    let err = store.add_relation(a, a).unwrap_err();
    assert!(matches!(
        err,
        BrainError::InvalidRelation(_, _, "self-edge")
    ));
    assert!(store.graph().unwrap().edges.is_empty());
}

#[test]
fn relation_to_unknown_page_rejected() {
    let store = BrainStore::open_in_memory().unwrap();
    let a = page(&store, "A", MemoryCategory::Notes);
    let err = store.add_relation(a, 12345).unwrap_err();
    assert!(matches!(
        err,
        BrainError::InvalidRelation(_, _, "unknown page")
    ));
}

#[test]
fn relation_removal() {
    let store = BrainStore::open_in_memory().unwrap();
    let a = page(&store, "A", MemoryCategory::Notes);
    let b = page(&store, "B", MemoryCategory::Notes);
    store.add_relation(a, b).unwrap();
    // Remove via the reversed direction: still finds the ordered pair.
    assert!(store.remove_relation(b, a).unwrap());
    assert!(store.related_page_ids(a).unwrap().is_empty());
    // Removing an absent edge still succeeds (Onyx parity).
    assert!(store.remove_relation(a, b).unwrap());
}

#[test]
fn related_pages_grouped_by_category() {
    let store = BrainStore::open_in_memory().unwrap();
    let hub = page(&store, "Q3 Launch", MemoryCategory::Workstreams);
    let entity = page(&store, "Acme Corp", MemoryCategory::Entities);
    let concept = page(&store, "Brand Kit", MemoryCategory::Concepts);
    let note = page(&store, "Ship on Fridays", MemoryCategory::Notes);
    let unrelated = page(&store, "Unrelated", MemoryCategory::Entities);

    store.add_relation(hub, entity).unwrap();
    store.add_relation(hub, concept).unwrap();
    store.add_relation(hub, note).unwrap();

    let related = store.related_pages(hub).unwrap();
    assert_eq!(related.len(), 3);
    assert_eq!(
        related.entities.iter().map(|p| p.id).collect::<Vec<_>>(),
        vec![entity]
    );
    assert_eq!(
        related.concepts.iter().map(|p| p.id).collect::<Vec<_>>(),
        vec![concept]
    );
    assert_eq!(
        related.notes.iter().map(|p| p.id).collect::<Vec<_>>(),
        vec![note]
    );
    assert!(related.workstreams.is_empty());
    assert!(!related.entities.iter().any(|p| p.id == unrelated));

    // A page with no relations has an empty grouping.
    assert!(store.related_pages(unrelated).unwrap().is_empty());
}

#[test]
fn graph_includes_degree_zero_nodes() {
    let store = BrainStore::open_in_memory().unwrap();
    let a = page(&store, "A", MemoryCategory::Notes);
    let b = page(&store, "B", MemoryCategory::Concepts);
    let isolated = page(&store, "Prefers dark mode", MemoryCategory::Notes);
    store.add_relation(a, b).unwrap();

    let graph = store.graph().unwrap();
    assert_eq!(graph.nodes.len(), 3, "degree-0 node must not be dropped");
    assert_eq!(graph.edges.len(), 1);

    let degree_of = |id: i64| graph.nodes.iter().find(|n| n.id == id).unwrap().degree;
    assert_eq!(degree_of(a), 1);
    assert_eq!(degree_of(b), 1);
    assert_eq!(degree_of(isolated), 0);

    let edge = graph.edges[0];
    assert_eq!((edge.source, edge.target), (a.min(b), a.max(b)));
}

// ---------------------------------------------------------------------------
// Criterion 3: sources + ref normalization
// ---------------------------------------------------------------------------

#[test]
fn source_attach_and_list() {
    let store = BrainStore::open_in_memory().unwrap();
    let id = page(&store, "Q3 Launch", MemoryCategory::Workstreams);

    store
        .add_source(
            id,
            MemorySourceType::ChatSession,
            "Brain kickoff chat",
            Some("sess-1"),
            Some("/app?chatId=sess-1"),
        )
        .unwrap();
    store
        .add_source(
            id,
            MemorySourceType::Document,
            "Brain PRD.pdf",
            Some("doc-1"),
            None,
        )
        .unwrap();
    store
        .add_source(id, MemorySourceType::File, "notes.txt", None, None)
        .unwrap();

    let sources = store.sources(id).unwrap();
    assert_eq!(sources.len(), 3);
    assert_eq!(sources[0].source_type, MemorySourceType::ChatSession);
    assert_eq!(sources[0].source_id.as_deref(), Some("sess-1"));
    assert_eq!(sources[0].url.as_deref(), Some("/app?chatId=sess-1"));
    assert_eq!(sources[1].source_type, MemorySourceType::Document);
    assert_eq!(sources[2].source_type, MemorySourceType::File);
    assert!(sources[2].source_id.is_none());

    // Attaching to a missing page fails loudly, not silently.
    assert!(matches!(
        store.add_source(999, MemorySourceType::Manual, "x", None, None),
        Err(BrainError::PageNotFound(999))
    ));
}

#[test]
fn source_ref_normalization_tolerates_drift() {
    // The exact drifted forms from the plan (and the Onyx adversarial case).
    assert_eq!(normalize_source_ref("[s1]"), "S1");
    assert_eq!(normalize_source_ref("S1"), "S1");
    assert_eq!(normalize_source_ref("[D3]"), "D3");
    assert_eq!(normalize_source_ref("d4"), "D4");
    assert_eq!(normalize_source_ref("  [ s2 ] "), "S2");
}

// ---------------------------------------------------------------------------
// Criterion 4: settings round-trip + run-complete
// ---------------------------------------------------------------------------

#[test]
fn settings_defaults_and_roundtrip() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("brain.sqlite");

    {
        let store = BrainStore::open(&db).unwrap();
        let initial = store.settings().unwrap();
        assert!(!initial.enabled);
        assert!(!initial.use_connectors);
        assert!(initial.focus_instructions.is_none());
        assert!(initial.last_run_at.is_none());

        let updated = store
            .update_settings(BrainSettingsUpdate {
                enabled: Some(true),
                use_connectors: Some(true),
                focus_instructions: Some(Some("  Track Acme work  ".to_owned())),
            })
            .unwrap();
        assert!(updated.enabled);
        assert!(updated.use_connectors);
        assert_eq!(
            updated.focus_instructions.as_deref(),
            Some("Track Acme work")
        );

        // Partial patch: only one field changes, the rest stay put.
        let patched = store
            .update_settings(BrainSettingsUpdate {
                use_connectors: Some(false),
                ..BrainSettingsUpdate::default()
            })
            .unwrap();
        assert!(patched.enabled);
        assert!(!patched.use_connectors);
        assert_eq!(
            patched.focus_instructions.as_deref(),
            Some("Track Acme work")
        );

        // Explicit clear (whitespace clears too, Onyx parity).
        let cleared = store
            .update_settings(BrainSettingsUpdate {
                focus_instructions: Some(Some("   ".to_owned())),
                ..BrainSettingsUpdate::default()
            })
            .unwrap();
        assert!(cleared.focus_instructions.is_none());
    }

    // Settings persist across reopen.
    let reopened = BrainStore::open(&db).unwrap();
    let settings = reopened.settings().unwrap();
    assert!(settings.enabled);
    assert!(!settings.use_connectors);
    assert!(settings.focus_instructions.is_none());
}

#[test]
fn mark_run_complete_updates_timestamp() {
    let store = BrainStore::open_in_memory().unwrap();
    assert!(store.settings().unwrap().last_run_at.is_none());
    let stamp = Utc::now();
    store.mark_run_complete(stamp).unwrap();
    let recorded = store.settings().unwrap().last_run_at.unwrap();
    assert_eq!(recorded.timestamp_millis(), stamp.timestamp_millis());
}

// ---------------------------------------------------------------------------
// Criterion 5: self-improvement run with a deterministic provider
// ---------------------------------------------------------------------------

/// Deterministic provider: records the input it saw and returns fixed pages.
struct FixedProvider {
    pages: Vec<ExtractedPage>,
    seen: Mutex<Vec<ExtractionInput>>,
}

impl FixedProvider {
    fn new(pages: Vec<ExtractedPage>) -> Self {
        Self {
            pages,
            seen: Mutex::new(Vec::new()),
        }
    }
}

impl ExtractionProvider for FixedProvider {
    fn extract(&self, input: &ExtractionInput) -> anyhow::Result<Vec<ExtractedPage>> {
        self.seen.lock().unwrap().push(input.clone());
        Ok(self.pages.clone())
    }
}

fn sample_context() -> RunContext {
    RunContext {
        sessions: vec![SessionSource {
            id: "sess-1".to_owned(),
            label: Some("Brain kickoff chat".to_owned()),
            url: Some("/app?chatId=sess-1".to_owned()),
            lines: vec!["User: we are launching Q3 with Acme Corp".to_owned()],
        }],
        documents: vec![DocumentSource {
            id: "doc-1".to_owned(),
            label: "Brain PRD.pdf".to_owned(),
            url: None,
            blurb: Some("PRD for the brain feature".to_owned()),
        }],
    }
}

#[test]
fn run_skipped_when_disabled() {
    let store = BrainStore::open_in_memory().unwrap();
    let provider = FixedProvider::new(vec![ExtractedPage {
        title: "Should not exist".to_owned(),
        category: "notes".to_owned(),
        content: "nope".to_owned(),
        related: vec![],
        sources: vec![],
    }]);

    let outcome = run_self_improvement(&store, &sample_context(), &provider).unwrap();
    assert_eq!(outcome, RunOutcome::Disabled);
    assert!(
        provider.seen.lock().unwrap().is_empty(),
        "provider must not be called"
    );
    assert!(store.list_pages().unwrap().is_empty());
    assert!(
        store.settings().unwrap().last_run_at.is_none(),
        "disabled run must not stamp"
    );
}

#[test]
fn run_applies_categorized_linked_cited_pages() {
    let store = BrainStore::open_in_memory().unwrap();
    store
        .update_settings(BrainSettingsUpdate {
            enabled: Some(true),
            use_connectors: Some(true),
            focus_instructions: Some(Some("Track the Acme launch".to_owned())),
        })
        .unwrap();

    let provider = FixedProvider::new(vec![
        ExtractedPage {
            title: "Acme Corp".to_owned(),
            category: "entities".to_owned(),
            content: "Acme Corp is the launch client.".to_owned(),
            related: vec!["Q3 Product Launch".to_owned()],
            // Drifted refs on purpose: must normalize to S1/D1.
            sources: vec!["[s1]".to_owned(), "d1".to_owned()],
        },
        ExtractedPage {
            title: "Q3 Product Launch".to_owned(),
            category: "workstreams".to_owned(),
            content: "Ongoing Q3 launch with Acme.".to_owned(),
            related: vec!["Acme Corp".to_owned()],
            sources: vec!["S1".to_owned()],
        },
        ExtractedPage {
            title: "Brand Kit".to_owned(),
            category: "concepts".to_owned(),
            content: "Reusable brand kit for launches.".to_owned(),
            related: vec![],
            sources: vec!["[D1]".to_owned()],
        },
        ExtractedPage {
            title: "Ship on Fridays".to_owned(),
            category: "bogus-category".to_owned(), // falls back to notes
            content: "Team prefers Friday ships.".to_owned(),
            related: vec![],
            sources: vec!["[X9]".to_owned()], // unknown ref: resolved to nothing
        },
    ]);

    let outcome = run_self_improvement(&store, &sample_context(), &provider).unwrap();
    assert_eq!(outcome, RunOutcome::Applied { applied: 4 });

    // Provider input: transcript with refs, focus instructions included.
    let seen = provider.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].transcript.contains("[S1]"));
    assert!(
        seen[0].transcript.contains("[D1]"),
        "connectors on: docs in transcript"
    );
    assert_eq!(
        seen[0].focus_instructions.as_deref(),
        Some("Track the Acme launch")
    );

    // Pages landed in the right categories, tagged as brain-created.
    let pages = store.list_pages().unwrap();
    assert_eq!(pages.len(), 4);
    let by_title = |title: &str| pages.iter().find(|p| p.title == title).unwrap();
    assert_eq!(by_title("Acme Corp").category, MemoryCategory::Entities);
    assert_eq!(
        by_title("Q3 Product Launch").category,
        MemoryCategory::Workstreams
    );
    assert_eq!(by_title("Brand Kit").category, MemoryCategory::Concepts);
    assert_eq!(by_title("Ship on Fridays").category, MemoryCategory::Notes);
    assert_eq!(by_title("Acme Corp").source.as_deref(), Some(BRAIN_SOURCE));

    // Sources resolved via normalized refs.
    let acme_sources = store.sources(by_title("Acme Corp").id).unwrap();
    assert_eq!(acme_sources.len(), 2);
    assert!(
        acme_sources
            .iter()
            .any(|s| s.source_type == MemorySourceType::ChatSession
                && s.source_id.as_deref() == Some("sess-1"))
    );
    assert!(
        acme_sources
            .iter()
            .any(|s| s.source_type == MemorySourceType::Document
                && s.source_id.as_deref() == Some("doc-1"))
    );
    assert!(
        store
            .sources(by_title("Ship on Fridays").id)
            .unwrap()
            .is_empty()
    );

    // Relations created (mutual mentions collapse to one edge).
    let graph = store.graph().unwrap();
    assert_eq!(graph.edges.len(), 1);
    let related = store.related_pages(by_title("Acme Corp").id).unwrap();
    assert_eq!(related.workstreams.len(), 1);
    assert_eq!(related.workstreams[0].title, "Q3 Product Launch");

    // Run stamped.
    assert!(store.settings().unwrap().last_run_at.is_some());
}

#[test]
fn run_updates_existing_page_instead_of_duplicating() {
    let store = BrainStore::open_in_memory().unwrap();
    store
        .update_settings(BrainSettingsUpdate {
            enabled: Some(true),
            ..Default::default()
        })
        .unwrap();
    let existing = page(&store, "Acme Corp", MemoryCategory::Notes);

    let provider = FixedProvider::new(vec![ExtractedPage {
        title: "acme corp".to_owned(), // case-insensitive title match
        category: "entities".to_owned(),
        content: "Acme Corp is the launch client.".to_owned(),
        related: vec![],
        sources: vec![],
    }]);
    let outcome = run_self_improvement(&store, &sample_context(), &provider).unwrap();
    assert_eq!(outcome, RunOutcome::Applied { applied: 1 });

    let pages = store.list_pages().unwrap();
    assert_eq!(pages.len(), 1, "re-run must update, not duplicate");
    assert_eq!(pages[0].id, existing);
    assert_eq!(pages[0].category, MemoryCategory::Entities, "recategorized");
    assert_eq!(pages[0].memory_text, "Acme Corp is the launch client.");
}

#[test]
fn run_without_connectors_excludes_documents() {
    let store = BrainStore::open_in_memory().unwrap();
    store
        .update_settings(BrainSettingsUpdate {
            enabled: Some(true),
            ..Default::default()
        })
        .unwrap();

    let provider = FixedProvider::new(vec![ExtractedPage {
        title: "Doc-derived".to_owned(),
        category: "notes".to_owned(),
        content: "Cites a doc that is not in the source map.".to_owned(),
        related: vec![],
        sources: vec!["D1".to_owned()],
    }]);
    run_self_improvement(&store, &sample_context(), &provider).unwrap();

    let seen = provider.seen.lock().unwrap();
    assert!(
        !seen[0].transcript.contains("[D1]"),
        "connectors off: no docs in transcript"
    );
    // The D1 ref resolves to nothing, so no source rows appear.
    let pages = store.list_pages().unwrap();
    assert!(store.sources(pages[0].id).unwrap().is_empty());
}

#[test]
fn empty_context_stamps_run_without_calling_provider() {
    let store = BrainStore::open_in_memory().unwrap();
    store
        .update_settings(BrainSettingsUpdate {
            enabled: Some(true),
            ..Default::default()
        })
        .unwrap();
    let provider = FixedProvider::new(vec![]);

    let outcome = run_self_improvement(&store, &RunContext::default(), &provider).unwrap();
    assert_eq!(outcome, RunOutcome::NoPages);
    assert!(provider.seen.lock().unwrap().is_empty());
    assert!(
        store.settings().unwrap().last_run_at.is_some(),
        "empty runs still stamp (Onyx parity)"
    );
}
