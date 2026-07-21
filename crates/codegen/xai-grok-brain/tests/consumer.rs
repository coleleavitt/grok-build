//! Fresh-consumer launch check: exercise the public API from outside the
//! crate's unit tests — create a store, add two pages plus a relation and a
//! source, and assert the graph query's return value.

use xai_grok_brain::{BrainStore, MemoryCategory, MemorySourceType, NewPage};

#[test]
fn public_api_end_to_end_graph() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = BrainStore::open(&dir.path().join("brain.sqlite")).unwrap();

    let acme = store
        .create_page(NewPage {
            title: Some("Acme Corp".to_owned()),
            memory_text: "Acme Corp is the launch client.".to_owned(),
            category: MemoryCategory::Entities,
            source: None,
        })
        .unwrap();
    let launch = store
        .create_page(NewPage {
            title: Some("Q3 Product Launch".to_owned()),
            memory_text: "Ongoing Q3 launch with Acme.".to_owned(),
            category: MemoryCategory::Workstreams,
            source: None,
        })
        .unwrap();

    store.add_relation(acme.id, launch.id).unwrap();
    store
        .add_source(
            acme.id,
            MemorySourceType::ChatSession,
            "Brain kickoff chat",
            Some("sess-1"),
            None,
        )
        .unwrap();

    // Assert the graph query's RETURN VALUE: 2 nodes, 1 edge, correct degrees.
    let graph = store.graph().unwrap();
    assert_eq!(graph.nodes.len(), 2);
    assert_eq!(graph.edges.len(), 1);

    let node = |id: i64| graph.nodes.iter().find(|n| n.id == id).unwrap();
    assert_eq!(node(acme.id).degree, 1);
    assert_eq!(node(acme.id).title, "Acme Corp");
    assert_eq!(node(acme.id).category, MemoryCategory::Entities);
    assert_eq!(node(launch.id).degree, 1);
    assert_eq!(node(launch.id).category, MemoryCategory::Workstreams);

    let edge = graph.edges[0];
    let (low, high) = (acme.id.min(launch.id), acme.id.max(launch.id));
    assert_eq!((edge.source, edge.target), (low, high));

    // The attached source is retrievable through the public API too.
    let sources = store.sources(acme.id).unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].source_type, MemorySourceType::ChatSession);
    assert_eq!(sources[0].source_id.as_deref(), Some("sess-1"));
}
