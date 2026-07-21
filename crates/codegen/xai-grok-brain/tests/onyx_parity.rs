//! Direct port of the Onyx testsprite-covered brain suite
//! (`backend/tests/external_dependency_unit/tools/test_memory_recall_and_graph.py`)
//! against the Rust crate, asserting the same scenarios produce the same
//! results.
//!
//! Port notes:
//! - `test_brain_graph_edges_and_source_citations` ports 1:1 (same seed data,
//!   same topology, same assertions).
//! - `test_relation_rejects_self_edge_and_cross_user_edge`: the self-edge
//!   guard ports 1:1. The cross-user guard has no analog in a single-user
//!   store (a plan non-goal); the equivalent invalid-endpoint guard —
//!   an edge to a nonexistent page — is asserted instead.
//! - The two recall-context tests exercise Onyx's chat layer
//!   (`get_memories`/`as_formatted_list`/`use_memories`), which is out of
//!   scope; the portable part — recency ordering (updated_at desc, creation
//!   order tie-break) — is asserted against `list_pages`.

use xai_grok_brain::{BrainStore, MemoryCategory, MemorySourceType, NewPage};

fn create(store: &BrainStore, title: &str, category: MemoryCategory, text: &str) -> i64 {
    store
        .create_page(NewPage {
            title: Some(title.to_owned()),
            memory_text: text.to_owned(),
            category,
            source: Some("manual".to_owned()),
        })
        .expect("memory creation should be allowed")
        .id
}

/// Onyx `test_brain_graph_edges_and_source_citations`: seed related-page edges
/// + source citations and prove the graph query returns the right nodes (with
/// degree), edges, related memories, and sources.
#[test]
fn brain_graph_edges_and_source_citations() {
    let store = BrainStore::open_in_memory().unwrap();

    let acme = create(
        &store,
        "Acme Corp",
        MemoryCategory::Entities,
        "Acme Corp is the flagship customer.",
    );
    let renewal = create(
        &store,
        "Contract renewal",
        MemoryCategory::Concepts,
        "Acme's contract renews in Q3.",
    );
    let planning = create(
        &store,
        "Q3 planning",
        MemoryCategory::Workstreams,
        "Plan the Q3 renewal push.",
    );

    // renewal is the hub: connected to both acme and planning.
    assert!(store.add_relation(acme, renewal).unwrap());
    assert!(store.add_relation(renewal, planning).unwrap());

    store
        .add_source(
            acme,
            MemorySourceType::ChatSession,
            "Kickoff call",
            Some("session-abc"),
            None,
        )
        .unwrap();
    store
        .add_source(
            renewal,
            MemorySourceType::Document,
            "Renewal terms.pdf",
            Some("doc-42"),
            Some("https://example.com/renewal.pdf"),
        )
        .unwrap();

    // Related memories: renewal <-> {acme, planning}; acme <-> {renewal}.
    let mut renewal_related = store.related_page_ids(renewal).unwrap();
    renewal_related.sort();
    let mut expected = vec![acme, planning];
    expected.sort();
    assert_eq!(renewal_related, expected);
    assert_eq!(store.related_page_ids(acme).unwrap(), vec![renewal]);

    // Source citations.
    let acme_sources = store.sources(acme).unwrap();
    assert_eq!(acme_sources.len(), 1);
    assert_eq!(acme_sources[0].source_type, MemorySourceType::ChatSession);
    assert_eq!(acme_sources[0].source_id.as_deref(), Some("session-abc"));

    let renewal_sources = store.sources(renewal).unwrap();
    assert_eq!(renewal_sources.len(), 1);
    assert_eq!(renewal_sources[0].source_type, MemorySourceType::Document);
    assert_eq!(
        renewal_sources[0].url.as_deref(),
        Some("https://example.com/renewal.pdf")
    );

    // Graph: 3 nodes, 2 edges, renewal has degree 2, the leaves degree 1.
    let graph = store.graph().unwrap();
    let node_ids: std::collections::HashSet<i64> = graph.nodes.iter().map(|n| n.id).collect();
    assert_eq!(node_ids, [acme, renewal, planning].into_iter().collect());
    assert_eq!(graph.edges.len(), 2);
    let degree = |id: i64| graph.nodes.iter().find(|n| n.id == id).unwrap().degree;
    assert_eq!(degree(renewal), 2);
    assert_eq!(degree(acme), 1);
    assert_eq!(degree(planning), 1);
}

/// Onyx `test_relation_rejects_self_edge_and_cross_user_edge`: guards on the
/// graph writer. Cross-user becomes invalid-endpoint in the single-user store.
#[test]
fn relation_rejects_self_edge_and_invalid_endpoint() {
    let store = BrainStore::open_in_memory().unwrap();
    let mine = create(&store, "Mine", MemoryCategory::Notes, "mine");

    // Self-edge is rejected.
    assert!(store.add_relation(mine, mine).is_err());
    // An edge to a page that does not exist is rejected (the single-user
    // analog of Onyx's not-owned-by-this-user guard).
    assert!(store.add_relation(mine, mine + 1000).is_err());
    // No edges were created.
    assert!(store.related_page_ids(mine).unwrap().is_empty());
    assert!(store.related_pages(mine).unwrap().is_empty());
}

/// Portable core of Onyx
/// `test_populated_memories_are_recalled_into_context_recency_ordered`: seed
/// every category, then prove listing returns all of them newest-first
/// (updated_at desc, creation-order tie-break) — the ordering the Onyx recall
/// context is built from.
#[test]
fn pages_listed_recency_ordered_across_all_categories() {
    let store = BrainStore::open_in_memory().unwrap();

    let seeded = [
        (
            "Favorite color",
            MemoryCategory::Notes,
            "The user's favorite color is teal.",
        ),
        (
            "Onyx",
            MemoryCategory::Entities,
            "Onyx is the user's primary work project.",
        ),
        (
            "Retrieval",
            MemoryCategory::Concepts,
            "The user cares about RAG quality.",
        ),
        (
            "Q3 launch",
            MemoryCategory::Workstreams,
            "The user is driving the Q3 launch.",
        ),
    ];
    for (title, category, text) in seeded {
        create(&store, title, category, text);
    }

    let pages = store.list_pages().unwrap();
    assert_eq!(pages.len(), seeded.len());

    // Every seeded text is present.
    let texts: std::collections::HashSet<&str> =
        pages.iter().map(|p| p.memory_text.as_str()).collect();
    for (_, _, text) in seeded {
        assert!(texts.contains(text));
    }

    // Recency-ordered: the memory created last shows up first.
    assert_eq!(pages[0].memory_text, seeded[seeded.len() - 1].2);

    // An update bumps a page back to the top (recency, not creation order).
    let oldest_id = pages[pages.len() - 1].id;
    store
        .update_page(
            oldest_id,
            xai_grok_brain::PageUpdate {
                memory_text: Some("The user's favorite color is now mauve.".to_owned()),
                ..Default::default()
            },
        )
        .unwrap();
    let repages = store.list_pages().unwrap();
    assert_eq!(repages[0].id, oldest_id);
}
