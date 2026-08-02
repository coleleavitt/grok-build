//! `advisor` tool — first-class, low-param affordance for consulting the
//! read-only advisor side-reviewer.
//!
//! `AdvisorTool` is a thin synchronous wrapper over the SAME subagent
//! execution engine `TaskTool` uses for `subagent_type: "advisor"`. It exists
//! so the model can reach for a concise side-review with a single optional
//! `focus` parameter instead of the full `task` tool surface (subagent_type,
//! run_in_background, isolation, resume_from, model, ...).
//!
//! The advisor subagent itself (prompt, tool access, enable/budget policy)
//! is unchanged and lives in `xai_tool_types::{ADVISOR_SUBAGENT, ADVISOR_PROMPT,
//! advisor::*}`. This tool only builds a [`SubagentRequest`] and dispatches it
//! through [`SubagentBackendResource`], exactly like `TaskTool`'s advisor path.
//!
//! ## Resources
//!
//! - `SubagentBackendResource` — backend for spawn (required)
//! - `SubagentDepthCounter` — current nesting depth (optional, defaults to 0)
//! - `SessionIdResource` — current session ID for parent scoping (optional)
//! - `CurrentPromptIdResource` — current parent turn ID (optional)

use crate::implementations::grok_build::task::MAX_SUBAGENT_DEPTH;
use crate::implementations::grok_build::task::backend::SubagentBackendResource;
use crate::implementations::grok_build::task::spawn::{SubagentSpawnParams, spawn_and_await};
use crate::implementations::grok_build::task::types::{
    CurrentPromptIdResource, ModelOverrideProvenance, SessionIdResource,
    SubagentAdvisorPreflightOutcome, SubagentDepthCounter,
};
use crate::types::output::ToolOutput;
use crate::types::tool::{ToolKind, ToolNamespace};
use xai_tool_types::{ADVISOR_SUBAGENT, SubagentCapabilityMode, SubagentCompletedOutput};

/// Default advisor question used when the caller omits `focus`.
const DEFAULT_ADVISOR_PROMPT: &str =
    "Review my current approach and flag risks, gaps, and better options before I continue.";

/// Input for the `advisor` tool.
///
/// Deliberately minimal (JFC-style): a single optional `focus` string. No
/// `subagent_type`, `model`, `isolation`, `resume_from`, or
/// `run_in_background` — this tool always targets the built-in advisor
/// subagent and always runs synchronously.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct AdvisorToolInput {
    /// Optional focus for the review (e.g. "check auth", "is this plan complete").
    /// When omitted, the advisor reviews the current approach generally.
    #[serde(default)]
    #[schemars(description = "Optional area to focus the advisor's review on. \
        Omit to have the advisor review your current approach generally.")]
    pub focus: Option<String>,
}

/// `advisor` tool.
///
/// Consults the read-only advisor subagent for a concise side-review.
/// Always synchronous (never backgrounded) and always targets
/// [`ADVISOR_SUBAGENT`] — the model cannot redirect it to another subagent
/// type.
#[derive(Debug, Default)]
pub struct AdvisorTool;

impl crate::types::tool_metadata::ToolMetadata for AdvisorTool {
    fn kind(&self) -> ToolKind {
        ToolKind::Advisor
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::GrokBuild
    }

    fn description_template(&self) -> &str {
        "Consult the read-only advisor for a concise side-review of your current approach — \
         call it before substantive work, when you are stuck, and before declaring done. \
         It does not implement or edit; it returns advice."
    }

    fn is_read_only(&self) -> bool {
        // Spawns a subagent (side effect: token spend, coordinator bookkeeping)
        // even though the advisor itself never edits anything.
        false
    }
}

impl xai_tool_runtime::Tool for AdvisorTool {
    type Args = AdvisorToolInput;
    type Output = ToolOutput;

    fn id(&self) -> xai_tool_protocol::ToolId {
        xai_tool_protocol::ToolId::new("advisor").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::xai_tool_runtime::ListToolsContext,
    ) -> xai_tool_types::ToolDescription {
        xai_tool_types::ToolDescription::new(
            "advisor",
            crate::types::tool_metadata::ToolMetadata::description_template(self),
        )
    }

    fn capabilities(&self) -> xai_tool_protocol::ToolCapabilities {
        xai_tool_protocol::ToolCapabilities {
            is_read_only: false,
            tool_scope: Some(xai_tool_protocol::ToolScope::Write),
            ..Default::default()
        }
    }

    #[tracing::instrument(name = "tool.advisor", skip_all)]
    async fn run(
        &self,
        ctx: xai_tool_runtime::ToolCallContext,
        input: AdvisorToolInput,
    ) -> Result<ToolOutput, xai_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;

        // 1. Gather resources exactly as TaskTool does.
        let (depth, backend, parent_session_id, parent_prompt_id) = {
            let res = resources.lock().await;

            let depth = res.get::<SubagentDepthCounter>().map(|d| d.0).unwrap_or(0);

            let backend = res
                .get::<SubagentBackendResource>()
                .ok_or_else(|| {
                    xai_tool_runtime::ToolError::custom(
                        "missing_resource",
                        "SubagentBackendResource (subagent support not initialized)",
                    )
                })?
                .clone();

            let parent_session_id = res
                .get::<SessionIdResource>()
                .map(|s| s.0.clone())
                .unwrap_or_default();

            let parent_prompt_id = res
                .get::<CurrentPromptIdResource>()
                .map(|p| p.0.clone())
                .filter(|prompt_id| !prompt_id.is_empty());

            (depth, backend, parent_session_id, parent_prompt_id)
        };

        // 2. Depth check — advisor cannot be launched from inside a subagent.
        if depth >= MAX_SUBAGENT_DEPTH {
            return Err(xai_tool_runtime::ToolError::invalid_arguments(format!(
                "Subagent depth limit exceeded (current depth: {depth}, max: {MAX_SUBAGENT_DEPTH}). \
                 Cannot spawn further nested subagents."
            )));
        }

        let prompt = input
            .focus
            .and_then(|f| {
                let trimmed = f.trim();
                (!trimmed.is_empty()).then(|| trimmed.to_string())
            })
            .unwrap_or_else(|| DEFAULT_ADVISOR_PROMPT.to_string());

        // 3. Advisor preflight — the SAME gate TaskTool enforces for
        //    subagent_type: "advisor" (enable/budget policy in
        //    xai_tool_types::advisor).
        let advisor_gate_prevalidated = match backend
            .backend()
            .validate_advisor_spawn(ADVISOR_SUBAGENT.name, &parent_session_id, &prompt, None)
            .await
        {
            SubagentAdvisorPreflightOutcome::Ok => true,
            SubagentAdvisorPreflightOutcome::Rejected { message } => {
                return Err(xai_tool_runtime::ToolError::invalid_arguments(format!(
                    "Advisor unavailable: {message}"
                )));
            }
            SubagentAdvisorPreflightOutcome::ValidationUnavailable => {
                return Err(xai_tool_runtime::ToolError::custom(
                    "validation_unavailable",
                    "Cannot validate advisor launch: the subagent coordinator is unreachable. \
                     Retry shortly or notify ops.",
                ));
            }
        };

        // 4. Build the subagent request — always synchronous, always forks
        //    the bounded parent-context snapshot, never surfaces via the
        //    between-turn idle-completion reminder path twice (advisor
        //    completes inline).
        let id = uuid::Uuid::now_v7().to_string();

        let params = SubagentSpawnParams {
            id: Some(id.clone()),
            prompt: prompt.clone(),
            description: "advisor review".to_string(),
            subagent_type: ADVISOR_SUBAGENT.name.to_string(),
            parent_session_id,
            parent_prompt_id,
            resume_from: None,
            cwd: None,
            model: None,
            model_override_provenance: ModelOverrideProvenance::Harness,
            reasoning_effort: None,
            persona: None,
            capability_mode: Some(SubagentCapabilityMode::ReadOnly),
            isolation: None,
            harness_agent_type: None,
            run_in_background: false,
            surface_completion: true,
            fork_context: true,
            advisor_gate_prevalidated,
        };

        // 5. Spawn and await — always blocking (never backgrounded by design).
        let result = spawn_and_await(&backend, params).await?;

        // Defense in depth: if the coordinator's await budget expired and it
        // auto-backgrounded the child anyway, surface a poll hint like TaskTool
        // does rather than silently losing the result.
        if result.backgrounded {
            return Ok(ToolOutput::Text(
                format!(
                    "The advisor review took longer than the foreground budget and was moved to \
                     the background. It is still running — you will be notified when it \
                     completes.\n\
                     subagent_id: {id}\n\
                     type: {}\n\
                     description: advisor review",
                    ADVISOR_SUBAGENT.name,
                )
                .into(),
            ));
        }

        if result.success {
            let resume_from_hint = result.subagent_id.clone();
            Ok(ToolOutput::SubagentCompleted(SubagentCompletedOutput {
                output: result.output.to_string(),
                subagent_id: result.subagent_id,
                subagent_type: ADVISOR_SUBAGENT.name.to_string(),
                tool_calls: result.tool_calls,
                turns: result.turns,
                duration_ms: result.duration_ms,
                worktree_path: result.worktree_path,
                persona: None,
                resume_from_hint,
                persona_hint: None,
            }))
        } else {
            Err(xai_tool_runtime::ToolError::invalid_arguments(
                result
                    .error
                    .unwrap_or_else(|| "Unknown advisor error".to_string()),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::implementations::grok_build::task::backend::{
        ChannelBackend, SubagentBackendResource,
    };
    use crate::implementations::grok_build::task::types::{
        SubagentAdvisorPreflightOutcome, SubagentEvent, SubagentRequest, SubagentResult,
        SubagentValidateTypeOutcome,
    };
    use crate::types::resources::Resources;
    use crate::types::tool_metadata::test_ctx;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    /// Backend whose `ValidateAdvisor` events are auto-acked with `Ok`.
    fn make_backend() -> (
        SubagentBackendResource,
        mpsc::UnboundedReceiver<SubagentEvent>,
    ) {
        make_backend_with_advisor_outcome(SubagentAdvisorPreflightOutcome::Ok)
    }

    fn make_backend_with_advisor_outcome(
        outcome: SubagentAdvisorPreflightOutcome,
    ) -> (
        SubagentBackendResource,
        mpsc::UnboundedReceiver<SubagentEvent>,
    ) {
        let (raw_tx, mut raw_rx) = mpsc::unbounded_channel::<SubagentEvent>();
        let (proxy_tx, proxy_rx) = mpsc::unbounded_channel::<SubagentEvent>();
        let backend = SubagentBackendResource(Arc::new(ChannelBackend::new(raw_tx)));
        tokio::spawn(async move {
            while let Some(event) = raw_rx.recv().await {
                match event {
                    SubagentEvent::ValidateType(req) => {
                        let _ = req.respond_to.send(SubagentValidateTypeOutcome::Ok);
                    }
                    SubagentEvent::ValidateAdvisor(req) => {
                        let _ = req.respond_to.send(outcome.clone());
                    }
                    other => {
                        if proxy_tx.send(other).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        (backend, proxy_rx)
    }

    fn resources_for_advisor(backend: SubagentBackendResource) -> Resources {
        let mut resources = Resources::new();
        resources.insert(backend);
        resources.insert(SubagentDepthCounter(0));
        resources.insert(SessionIdResource("parent-session".to_string()));
        resources.insert(CurrentPromptIdResource("prompt-123".to_string()));
        resources
    }

    fn unwrap_spawn(event: SubagentEvent) -> SubagentRequest {
        match event {
            SubagentEvent::Spawn(r) => *r,
            _ => panic!("Expected SubagentEvent::Spawn"),
        }
    }

    // ── ToolMetadata / Tool::id ────────────────────────────────────────

    #[test]
    fn tool_kind_is_advisor() {
        assert_eq!(
            crate::types::tool_metadata::ToolMetadata::kind(&AdvisorTool),
            ToolKind::Advisor
        );
    }

    #[test]
    fn tool_id_renders_advisor() {
        assert_eq!(xai_tool_runtime::Tool::id(&AdvisorTool).as_str(), "advisor");
    }
    #[test]
    fn tool_registered_in_registry_builder_as_advisor_kind() {
        // Adversarial case 4 (identity): the builder `ToolRegistry::new()`
        // wires up (`crate::registry::types::ToolRegistryBuilder::new()`)
        // must know the fully-qualified id and report `ToolKind::Advisor`
        // for it, not merely have `AdvisorTool` compile in isolation.
        let builder = crate::registry::types::ToolRegistryBuilder::new();
        assert!(
            builder.has_tool_id("GrokBuild:advisor"),
            "registry must resolve the advisor tool by its fully-qualified id"
        );
        let kinds = builder.known_tool_kinds();
        assert_eq!(
            kinds.get("GrokBuild:advisor"),
            Some(&ToolKind::Advisor),
            "registry must report ToolKind::Advisor for the advisor tool"
        );
    }

    // ── AdvisorToolInput serde ───────────────────────────────────────

    #[test]
    fn input_deserializes_from_empty_object() {
        let input: AdvisorToolInput = serde_json::from_str("{}").unwrap();
        assert!(input.focus.is_none());
    }

    #[test]
    fn input_deserializes_with_focus() {
        let input: AdvisorToolInput = serde_json::from_str(r#"{"focus": "check auth"}"#).unwrap();
        assert_eq!(input.focus.as_deref(), Some("check auth"));
    }
    #[test]
    fn input_deserialization_ignores_unexpected_extra_fields() {
        // Adversarial case 4 (identity): a model that hallucinates the full
        // `task`-style surface (subagent_type/model/isolation/...) onto
        // `advisor` must not be rejected or silently redirected — the
        // deliberately minimal `AdvisorToolInput` has no `deny_unknown_fields`,
        // so extra keys are dropped and only `focus` is honored.
        let input: AdvisorToolInput = serde_json::from_str(
            r#"{"focus": "check auth", "subagent_type": "researcher", "model": "x", "run_in_background": true}"#,
        )
        .expect("extra unrecognized fields must not fail deserialization");
        assert_eq!(input.focus.as_deref(), Some("check auth"));
    }

    #[test]
    fn input_deserialization_rejects_wrong_focus_type() {
        // A malformed `focus` (wrong type) must fail deserialization rather
        // than silently coercing to a default/empty value.
        let result: Result<AdvisorToolInput, _> = serde_json::from_str(r#"{"focus": 12345}"#);
        assert!(result.is_err(), "non-string focus must be rejected");
    }

    // ── Dispatch smoke tests ─────────────────────────────────────────

    #[tokio::test]
    async fn dispatch_spawns_advisor_subagent_synchronously_with_forked_context() {
        let (backend, mut rx) = make_backend();
        let resources = resources_for_advisor(backend);
        let shared = resources.into_shared();

        let handle = tokio::spawn(async move {
            let request = unwrap_spawn(rx.recv().await.unwrap());
            assert_eq!(request.subagent_type, "advisor");
            assert!(!request.run_in_background, "advisor tool never backgrounds");
            assert!(request.fork_context, "advisor must fork parent context");
            assert!(request.advisor_gate_prevalidated);
            assert_eq!(request.parent_session_id, "parent-session");
            request
                .result_tx
                .send(SubagentResult {
                    success: true,
                    output: std::sync::Arc::from("Risks: none. Recommendation: proceed."),
                    subagent_id: request.id.clone(),
                    child_session_id: request.id.clone(),
                    tool_calls: 1,
                    turns: 1,
                    duration_ms: 42,
                    ..Default::default()
                })
                .unwrap();
        });

        let result = xai_tool_runtime::Tool::run(
            &AdvisorTool,
            test_ctx(shared),
            AdvisorToolInput { focus: None },
        )
        .await
        .expect("advisor dispatch should succeed");

        handle.await.unwrap();

        match result {
            ToolOutput::SubagentCompleted(sub) => {
                assert_eq!(sub.subagent_type, "advisor");
                assert!(sub.output.contains("Recommendation: proceed"));
            }
            other => panic!("Expected SubagentCompleted, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_uses_default_prompt_when_focus_omitted() {
        let (backend, mut rx) = make_backend();
        let resources = resources_for_advisor(backend);
        let shared = resources.into_shared();

        let handle = tokio::spawn(async move {
            let request = unwrap_spawn(rx.recv().await.unwrap());
            assert_eq!(request.prompt, DEFAULT_ADVISOR_PROMPT);
            request
                .result_tx
                .send(SubagentResult {
                    success: true,
                    output: "ok".into(),
                    subagent_id: request.id.clone(),
                    child_session_id: request.id.clone(),
                    ..Default::default()
                })
                .unwrap();
        });

        let _ = xai_tool_runtime::Tool::run(
            &AdvisorTool,
            test_ctx(shared),
            AdvisorToolInput { focus: None },
        )
        .await
        .unwrap();
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn dispatch_threads_focus_into_prompt() {
        let (backend, mut rx) = make_backend();
        let resources = resources_for_advisor(backend);
        let shared = resources.into_shared();

        let handle = tokio::spawn(async move {
            let request = unwrap_spawn(rx.recv().await.unwrap());
            assert_eq!(request.prompt, "check auth");
            request
                .result_tx
                .send(SubagentResult {
                    success: true,
                    output: "ok".into(),
                    subagent_id: request.id.clone(),
                    child_session_id: request.id.clone(),
                    ..Default::default()
                })
                .unwrap();
        });

        let _ = xai_tool_runtime::Tool::run(
            &AdvisorTool,
            test_ctx(shared),
            AdvisorToolInput {
                focus: Some("check auth".to_string()),
            },
        )
        .await
        .unwrap();
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn depth_limit_exceeded_rejects_before_spawn() {
        let (backend, mut rx) = make_backend();
        let mut resources = resources_for_advisor(backend);
        resources.insert(SubagentDepthCounter(MAX_SUBAGENT_DEPTH));

        let result = xai_tool_runtime::Tool::run(
            &AdvisorTool,
            test_ctx(resources.into_shared()),
            AdvisorToolInput { focus: None },
        )
        .await;

        let err = result.unwrap_err().to_string();
        assert!(err.contains("depth limit exceeded"), "error: {err}");
        assert!(rx.try_recv().is_err(), "must not spawn past depth limit");
    }

    #[tokio::test]
    async fn missing_backend_returns_error() {
        let resources = Resources::new();

        let result = xai_tool_runtime::Tool::run(
            &AdvisorTool,
            test_ctx(resources.into_shared()),
            AdvisorToolInput { focus: None },
        )
        .await;

        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("SubagentBackendResource"),
            "error should mention missing resource: {err}"
        );
    }

    #[tokio::test]
    async fn advisor_disabled_rejects_before_spawn() {
        let (backend, mut rx) =
            make_backend_with_advisor_outcome(SubagentAdvisorPreflightOutcome::Rejected {
                message: "advisor is disabled".to_string(),
            });
        let resources = resources_for_advisor(backend);

        let result = xai_tool_runtime::Tool::run(
            &AdvisorTool,
            test_ctx(resources.into_shared()),
            AdvisorToolInput { focus: None },
        )
        .await;

        let msg = result
            .expect_err("disabled advisor must reject")
            .to_string();
        assert!(msg.contains("Advisor unavailable: advisor is disabled"));
        assert!(rx.try_recv().is_err(), "must not spawn when disabled");
    }

    #[tokio::test]
    async fn advisor_validation_unavailable_returns_custom_error() {
        let (backend, mut rx) = make_backend_with_advisor_outcome(
            SubagentAdvisorPreflightOutcome::ValidationUnavailable,
        );
        let resources = resources_for_advisor(backend);

        let result = xai_tool_runtime::Tool::run(
            &AdvisorTool,
            test_ctx(resources.into_shared()),
            AdvisorToolInput { focus: None },
        )
        .await;

        let msg = result.expect_err("must error").to_string();
        assert!(msg.contains("subagent coordinator is unreachable"));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn failed_advisor_returns_error() {
        let (backend, mut rx) = make_backend();
        let resources = resources_for_advisor(backend);
        let shared = resources.into_shared();

        let handle = tokio::spawn(async move {
            let request = unwrap_spawn(rx.recv().await.unwrap());
            request
                .result_tx
                .send(SubagentResult {
                    success: false,
                    error: Some("child session crashed".to_string()),
                    ..Default::default()
                })
                .unwrap();
        });

        let result = xai_tool_runtime::Tool::run(
            &AdvisorTool,
            test_ctx(shared),
            AdvisorToolInput { focus: None },
        )
        .await;
        handle.await.unwrap();

        let err = result.unwrap_err().to_string();
        assert!(err.contains("child session crashed"), "error: {err}");
    }

    #[tokio::test]
    async fn auto_backgrounded_result_returns_task_id_text() {
        let (backend, mut rx) = make_backend();
        let resources = resources_for_advisor(backend);
        let shared = resources.into_shared();

        let drain = tokio::spawn(async move {
            if let Some(SubagentEvent::Spawn(boxed)) = rx.recv().await {
                let _ = boxed.result_tx.send(SubagentResult {
                    backgrounded: true,
                    subagent_id: boxed.id.clone(),
                    child_session_id: boxed.id.clone(),
                    ..Default::default()
                });
            }
        });

        let result = xai_tool_runtime::Tool::run(
            &AdvisorTool,
            test_ctx(shared),
            AdvisorToolInput { focus: None },
        )
        .await
        .expect("auto-backgrounded advisor spawn returns Ok");

        match result {
            ToolOutput::Text(text) => {
                assert!(text.text.contains("moved to"));
                assert!(text.text.contains("subagent_id:"));
            }
            other => panic!("expected Text output, got {other:?}"),
        }

        let _ = tokio::time::timeout(std::time::Duration::from_millis(500), drain).await;
    }
}
