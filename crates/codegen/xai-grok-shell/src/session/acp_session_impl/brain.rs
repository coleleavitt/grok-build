//! Brain integration for the real session prompt path.
//!
//! The heavy lifting lives in `xai-grok-brain`; this module is the thin shell
//! seam that supplies session/prompt ids and hands the returned context block to
//! `ChatStateActor::build_request` as a normal memory reminder.

use super::*;

/// Process one user prompt through Brain at an explicit store path. Tests drive
/// this helper directly; production calls [`process_brain_request_for_prompt`]
/// with the default durable store path.
pub(crate) fn process_brain_request_at_path(
    store_path: &std::path::Path,
    session_id: &str,
    prompt_id: &str,
    user_text: &str,
    initialize_enabled: bool,
) -> Result<xai_grok_brain::BrainRequestOutcome, xai_grok_brain::BrainError> {
    let service = xai_grok_brain::BrainService::open(store_path)?;
    if initialize_enabled && !service.store().settings_initialized()? {
        service.update_settings(xai_grok_brain::BrainSettingsUpdate {
            enabled: Some(true),
            ..Default::default()
        })?;
    }
    service.process_request(xai_grok_brain::BrainRequest {
        session_id,
        prompt_id,
        user_text,
    })
}

/// Merge the existing markdown/embedding memory reminder with the Brain
/// reminder. Both flow through the same shipped prompt injection mechanism.
pub(crate) fn combine_memory_reminders(
    existing: Option<String>,
    brain: Option<String>,
) -> Option<String> {
    match (existing, brain) {
        (None, None) => None,
        (Some(existing), None) => Some(existing),
        (None, Some(brain)) => Some(brain),
        (Some(existing), Some(brain)) => Some(format!("{existing}\n\n{brain}")),
    }
}

impl SessionActor {
    /// Production prompt-path hook: open the stable Grok Brain store and process
    /// this real user prompt. Failures are logged and fail-open so Brain cannot
    /// break normal inference.
    pub(super) async fn process_brain_request_for_prompt(
        &self,
        prompt_id: &str,
        user_text: &str,
    ) -> Option<String> {
        match process_brain_request_at_path(
            &xai_grok_brain::default_store_path(),
            self.session_info.id.0.as_ref(),
            prompt_id,
            user_text,
            true,
        ) {
            Ok(outcome) => outcome.injected_context,
            Err(err) => {
                tracing::warn!(
                    target: xai_grok_telemetry::memory_log::TARGET,
                    error = %err,
                    "BRAIN_INTEGRATION: failed to process prompt"
                );
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xai_chat_state::{ChatStateActor, NullChatPersistence};
    use xai_grok_sampling_types::{ConversationItem, SamplingConfig};

    #[test]
    fn shell_helper_remembers_reopens_and_recalls_into_context() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = tmp.path().join("brain.sqlite");

        let first = process_brain_request_at_path(
            &db,
            "session-a",
            "prompt-a",
            "Please remember that my project codename is Zephyr-Shell.",
            true,
        )
        .unwrap();
        let page = first
            .remembered_page
            .expect("remember prompt should create a Brain page");
        assert!(page.memory_text.contains("Zephyr-Shell"));
        assert!(first.injected_context.is_none());

        // Reopen by using the helper again: this is the same path a fresh Grok
        // request uses after process restart.
        let later = process_brain_request_at_path(
            &db,
            "session-b",
            "prompt-b",
            "What is my project codename?",
            true,
        )
        .unwrap();
        let context = later
            .injected_context
            .expect("fresh request should receive Brain context");
        assert!(context.contains("Zephyr-Shell"));
        assert!(later.remembered_page.is_none());

        let service = xai_grok_brain::BrainService::open(&db).unwrap();
        let graph = service.graph().unwrap();
        assert_eq!(graph.nodes.len(), 1);
        assert_eq!(graph.nodes[0].degree, 0);
        let sources = service.sources(page.id).unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].source_id.as_deref(), Some("session-a"));
    }

    #[test]
    fn disabled_setting_blocks_create_and_recall_from_shell_helper() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = tmp.path().join("brain.sqlite");
        let service = xai_grok_brain::BrainService::open(&db).unwrap();
        service
            .update_settings(xai_grok_brain::BrainSettingsUpdate {
                enabled: Some(false),
                ..Default::default()
            })
            .unwrap();
        drop(service);

        let outcome = process_brain_request_at_path(
            &db,
            "session-a",
            "prompt-a",
            "Remember that my favorite language is Rust.",
            true,
        )
        .unwrap();
        assert!(outcome.injected_context.is_none());
        assert!(outcome.remembered_page.is_none());
        assert!(
            xai_grok_brain::BrainService::open(&db)
                .unwrap()
                .store()
                .list_pages()
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn brain_context_uses_real_chat_state_request_injection() {
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let h = ChatStateActor::spawn(
            vec![
                ConversationItem::system("You are helpful."),
                ConversationItem::user("What is my project codename?"),
            ],
            SamplingConfig {
                base_url: "http://localhost".to_owned(),
                model: "test".to_owned(),
                max_completion_tokens: None,
                temperature: None,
                top_p: None,
                api_backend: Default::default(),
                extra_headers: Default::default(),
                context_window: std::num::NonZeroU64::new(256_000).unwrap(),
                reasoning_effort: None,
                stream_tool_calls: None,
                provider_request_adapter: None,
            },
            Box::new(NullChatPersistence),
            event_tx,
            tokio_util::sync::CancellationToken::new(),
        );
        let brain = Some(
            "<brain_context>\n- [entities] Project Codename: The user's project codename is Zephyr-Shell.\n</brain_context>"
                .to_owned(),
        );
        let request = h
            .build_request(
                vec![],
                combine_memory_reminders(None, brain),
                false,
                None,
                "conv".to_owned(),
                "req".to_owned(),
            )
            .await
            .unwrap();
        let ConversationItem::System(sys) = &request.items[0] else {
            panic!("expected system message");
        };
        assert!(sys.content.contains("<brain_context>"));
        assert!(sys.content.contains("Zephyr-Shell"));
    }

    #[test]
    fn combines_existing_memory_and_brain_reminders() {
        let combined = combine_memory_reminders(
            Some("<memory_context>old</memory_context>".to_owned()),
            Some("<brain_context>new</brain_context>".to_owned()),
        )
        .unwrap();
        assert!(combined.contains("<memory_context>old</memory_context>"));
        assert!(combined.contains("<brain_context>new</brain_context>"));
    }
}
