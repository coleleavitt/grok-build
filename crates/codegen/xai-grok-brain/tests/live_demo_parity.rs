//! Portable port of Onyx testsprite's live memory demo
//! (`testsprite_tests/memory_demo_populate.py`) for the library crate.
//!
//! The original Onyx script logs in, creates memories across all four
//! categories, reads `/memory` category counts and `/memory/graph`, asks a fresh
//! chat to recall the stored codename, then deletes its demo rows. This crate
//! has no HTTP/chat/UI layer by design, so this test drives the equivalent
//! shipped library path: file-backed store -> populate all categories -> counts
//! + graph -> deterministic self-improvement provider refreshes/recalls the
//! codename page and attaches a session source -> cleanup -> empty store.

use std::sync::Mutex;

use xai_grok_brain::engine::{
    ExtractedPage, ExtractionInput, ExtractionProvider, RunContext, RunOutcome, SessionSource,
    run_self_improvement,
};
use xai_grok_brain::{BrainSettingsUpdate, BrainStore, MemoryCategory, MemorySourceType, NewPage};

struct RecallProvider {
    seen: Mutex<Vec<ExtractionInput>>,
}

impl ExtractionProvider for RecallProvider {
    fn extract(&self, input: &ExtractionInput) -> anyhow::Result<Vec<ExtractedPage>> {
        self.seen.lock().unwrap().push(input.clone());
        Ok(vec![ExtractedPage {
            title: "Project codename".to_owned(),
            category: "entities".to_owned(),
            content: "The user's secret project codename is Zephyr-Library.".to_owned(),
            related: vec!["Q3 launch".to_owned(), "Style".to_owned()],
            // Deliberate bracket/case drift, like the Brain engine hardening
            // test: this must resolve to the S1 chat-session citation.
            sources: vec!["[s1]".to_owned()],
        }])
    }
}

#[test]
fn live_demo_populate_graph_recall_and_cleanup() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = BrainStore::open(&dir.path().join("brain.sqlite")).unwrap();
    let seed = [
        (
            "[demo] The user's secret project codename is Zephyr-Library.",
            "Project codename",
            MemoryCategory::Entities,
        ),
        (
            "[demo] The user prefers concise, bulleted answers.",
            "Style",
            MemoryCategory::Notes,
        ),
        (
            "[demo] The user cares about retrieval quality (RAG).",
            "RAG",
            MemoryCategory::Concepts,
        ),
        (
            "[demo] The user is driving the Q3 launch.",
            "Q3 launch",
            MemoryCategory::Workstreams,
        ),
    ];

    let mut created_ids = Vec::new();
    for (content, title, category) in seed {
        let page = store
            .create_page(NewPage {
                title: Some(title.to_owned()),
                memory_text: content.to_owned(),
                category,
                source: Some("manual".to_owned()),
            })
            .unwrap();
        created_ids.push(page.id);
    }

    // Onyx demo step 3: read back `/memory` total + category counts.
    assert_eq!(store.list_pages().unwrap().len(), 4);
    let counts = store.category_counts().unwrap();
    assert_eq!(counts[&MemoryCategory::Entities], 1);
    assert_eq!(counts[&MemoryCategory::Notes], 1);
    assert_eq!(counts[&MemoryCategory::Concepts], 1);
    assert_eq!(counts[&MemoryCategory::Workstreams], 1);

    // Onyx demo also prints `/memory/graph`: all populated memories should be
    // present, with degree 0 until the engine adds relations.
    let graph_before = store.graph().unwrap();
    assert_eq!(graph_before.nodes.len(), 4);
    assert!(graph_before.nodes.iter().all(|node| node.degree == 0));
    assert!(graph_before.edges.is_empty());

    // Library analog of the fresh-chat recall: an enabled self-improvement run
    // sees the existing codename title, updates that page instead of duplicating
    // it, attaches a session citation, and links related pages.
    store
        .update_settings(BrainSettingsUpdate {
            enabled: Some(true),
            focus_instructions: Some(Some("Keep demo codename facts recallable".to_owned())),
            ..Default::default()
        })
        .unwrap();
    let provider = RecallProvider {
        seen: Mutex::new(Vec::new()),
    };
    let outcome = run_self_improvement(
        &store,
        &RunContext {
            sessions: vec![SessionSource {
                id: "demo-session".to_owned(),
                label: Some("Memory demo chat".to_owned()),
                url: Some("/app?chatId=demo-session".to_owned()),
                lines: vec![
                    "User: What is my secret project codename?".to_owned(),
                    "Assistant: Zephyr-Library".to_owned(),
                ],
            }],
            documents: Vec::new(),
        },
        &provider,
    )
    .unwrap();
    assert_eq!(outcome, RunOutcome::Applied { applied: 1 });
    assert_eq!(
        store.list_pages().unwrap().len(),
        4,
        "run updates, not duplicates"
    );

    let seen = provider.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        seen[0].focus_instructions.as_deref(),
        Some("Keep demo codename facts recallable")
    );
    assert!(
        seen[0]
            .existing_titles
            .iter()
            .any(|title| title == "Project codename")
    );

    let codename = store
        .list_pages_by_category(MemoryCategory::Entities)
        .unwrap()
        .into_iter()
        .find(|page| page.title == "Project codename")
        .unwrap();
    assert!(codename.memory_text.contains("Zephyr-Library"));
    let sources = store.sources(codename.id).unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].source_type, MemorySourceType::ChatSession);
    assert_eq!(sources[0].source_id.as_deref(), Some("demo-session"));

    let graph_after = store.graph().unwrap();
    let codename_degree = graph_after
        .nodes
        .iter()
        .find(|node| node.id == codename.id)
        .unwrap()
        .degree;
    assert_eq!(codename_degree, 2);
    assert_eq!(graph_after.edges.len(), 2);
    assert!(store.settings().unwrap().last_run_at.is_some());

    // Onyx demo cleanup: delete only the demo rows and prove the account/store
    // is back to a clean state.
    for id in created_ids {
        assert!(store.delete_page(id).unwrap());
    }
    assert!(store.list_pages().unwrap().is_empty());
    assert!(store.graph().unwrap().nodes.is_empty());
    assert!(store.graph().unwrap().edges.is_empty());
    assert!(
        store
            .category_counts()
            .unwrap()
            .values()
            .all(|count| *count == 0)
    );
}
