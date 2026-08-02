//! Fresh-consumer launch/parity check for Grok session-history backfill.
//!
//! Drives the public BrainService API the way Grok Build does: a durable store,
//! a `~/.grok`-shaped sessions tree, a deterministic provider, then a later
//! request-path recall through `process_request`.

use std::sync::Mutex;

use xai_grok_brain::engine::{ExtractedPage, ExtractionInput, ExtractionProvider, RunOutcome};
use xai_grok_brain::{BrainService, BrainSettingsUpdate, MemoryCategory};

struct Provider {
    seen: Mutex<Vec<ExtractionInput>>,
}

impl ExtractionProvider for Provider {
    fn extract(&self, input: &ExtractionInput) -> anyhow::Result<Vec<ExtractedPage>> {
        self.seen.lock().unwrap().push(input.clone());
        Ok(vec![
            ExtractedPage {
                title: "Zephyr Project".to_owned(),
                category: "entities".to_owned(),
                content: "The Zephyr project codename came from historical session backfill."
                    .to_owned(),
                related: vec!["Launch Plan".to_owned()],
                // Deliberate drift + document ref.
                sources: vec!["[s1]".to_owned(), "d1".to_owned(), "missing".to_owned()],
            },
            ExtractedPage {
                title: "Launch Plan".to_owned(),
                category: "workstreams".to_owned(),
                content: "The launch plan is connected to Zephyr.".to_owned(),
                related: vec!["Zephyr Project".to_owned()],
                sources: vec!["S1".to_owned()],
            },
        ])
    }
}

#[test]
fn backfill_then_request_recall_uses_public_service_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    let grok_home = tmp.path().join("grok-home");
    let session_dir = grok_home.join("sessions/workspace/session-1");
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::write(
        session_dir.join("summary.json"),
        r#"{"info":{"id":"session-1"},"session_summary":"Historical Zephyr session","updated_at":"2026-07-21T12:00:00Z"}"#,
    )
    .unwrap();
    std::fs::write(
        session_dir.join("chat_history.jsonl"),
        r#"{"type":"user","content":[{"type":"text","text":"We decided the project codename is Zephyr. See file://docs/zephyr.md"}]}
{"type":"assistant","content":"Noted. Zephyr is tied to the launch plan."}
"#,
    )
    .unwrap();

    let service = BrainService::open(&tmp.path().join("brain.sqlite")).unwrap();
    service
        .update_settings(BrainSettingsUpdate {
            enabled: Some(true),
            use_connectors: Some(true),
            focus_instructions: Some(Some("Capture codenames and launches".to_owned())),
        })
        .unwrap();
    let provider = Provider {
        seen: Mutex::new(Vec::new()),
    };

    let out = service
        .run_backfill_from_grok_home(&grok_home, &provider)
        .unwrap();
    assert_eq!(out.outcome, RunOutcome::Applied { applied: 2 });
    assert_eq!(out.selection.sessions.len(), 1);

    let seen = provider.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].transcript.contains("[S1] User:"));
    assert!(seen[0].transcript.contains("[D1]"));
    assert_eq!(
        seen[0].focus_instructions.as_deref(),
        Some("Capture codenames and launches")
    );
    drop(seen);

    let graph = service.graph().unwrap();
    assert_eq!(graph.nodes.len(), 2);
    assert_eq!(graph.edges.len(), 1);
    assert!(graph.nodes.iter().all(|node| node.degree == 1));

    let entity = service
        .store()
        .list_pages_by_category(MemoryCategory::Entities)
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(entity.title, "Zephyr Project");
    let sources = service.sources(entity.id).unwrap();
    assert_eq!(sources.len(), 2, "unknown source refs must be ignored");
    assert!(
        sources
            .iter()
            .any(|s| s.source_id.as_deref() == Some("session-1"))
    );
    assert!(
        sources
            .iter()
            .any(|s| s.source_id.as_deref() == Some("file://docs/zephyr.md"))
    );
    assert!(service.settings().unwrap().last_run_at.is_some());

    // A later public request gets the backfilled value through service recall.
    let recall = service
        .process_request(xai_grok_brain::BrainRequest {
            session_id: "fresh-session",
            prompt_id: "fresh-prompt",
            user_text: "What was the Zephyr project?",
            workspace_scope: None,
        })
        .unwrap();
    let context = recall
        .injected_context
        .expect("backfilled page should recall");
    assert!(context.contains("Zephyr project codename"));
    assert!(recall.remembered_page.is_none());

    // Repeat run after last_run_at should select no sessions and not duplicate.
    let second = service
        .run_backfill_from_grok_home(&grok_home, &provider)
        .unwrap();
    assert_eq!(second.outcome, RunOutcome::NoPages);
    assert_eq!(service.store().list_pages().unwrap().len(), 2);
}
