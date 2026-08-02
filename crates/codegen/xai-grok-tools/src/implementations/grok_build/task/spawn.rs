//! Shared subagent-spawn request construction.
//!
//! `TaskTool`, `AdvisorTool`, and `SubagentEconomyInvoker` each build a
//! [`SubagentRequest`] by hand (~18 fields plus the `result_tx` placeholder
//! and UUID generation). [`SubagentSpawnParams`] centralizes that assembly
//! so the three call sites only supply the knobs that actually vary for
//! their product surface, while the request-shape plumbing (id generation,
//! the placeholder oneshot channel — replaced by the backend on send — and
//! the `SubagentRuntimeOverrides` nesting) lives in one place.
//!
//! This module intentionally does NOT change any product behavior: each
//! call site still decides its own field values; this only removes the
//! duplicated literal syntax.

use super::backend::SubagentBackendResource;
use super::types::{
    ModelOverrideProvenance, SubagentOwner, SubagentRequest, SubagentResult,
    SubagentRuntimeOverrides, SubagentSpawnRequest,
};
use xai_tool_types::{SubagentCapabilityMode, SubagentIsolationMode};

/// Every knob that varies across the three `SubagentRequest` construction
/// sites (`TaskTool::run`, `AdvisorTool::run`,
/// `SubagentEconomyInvoker::spawn_subagent`).
///
/// `id: None` generates a fresh UUIDv7 in [`Self::into_request`], mirroring
/// each call site's own fallback (`TaskTool` additionally allows the caller
/// to pass through `TaskToolInput.task_id` via `id: Some(..)`).
#[derive(Debug, Clone, Default)]
pub struct SubagentSpawnParams {
    pub id: Option<String>,
    pub prompt: String,
    pub description: String,
    pub subagent_type: String,
    pub parent_session_id: String,
    pub parent_prompt_id: Option<String>,
    pub resume_from: Option<String>,
    pub cwd: Option<String>,
    pub model: Option<String>,
    pub model_override_provenance: ModelOverrideProvenance,
    pub reasoning_effort: Option<String>,
    pub persona: Option<String>,
    pub capability_mode: Option<SubagentCapabilityMode>,
    pub isolation: Option<SubagentIsolationMode>,
    pub harness_agent_type: Option<String>,
    pub run_in_background: bool,
    pub surface_completion: bool,
    pub fork_context: bool,
    pub advisor_gate_prevalidated: bool,
}

impl SubagentSpawnParams {
    /// Assemble the [`SubagentRequest`], generating an id when none was supplied.
    pub fn into_request(self) -> SubagentRequest {
        let id = self.id.unwrap_or_else(|| uuid::Uuid::now_v7().to_string());

        SubagentRequest {
            id,
            prompt: self.prompt,
            description: self.description,
            subagent_type: self.subagent_type,
            parent_session_id: self.parent_session_id,
            parent_prompt_id: self.parent_prompt_id,
            resume_from: self.resume_from,
            cwd: self.cwd,
            runtime_overrides: SubagentRuntimeOverrides {
                model: self.model,
                model_override_provenance: self.model_override_provenance,
                reasoning_effort: self.reasoning_effort,
                persona: self.persona,
                capability_mode: self.capability_mode,
                isolation: self.isolation,
                harness_agent_type: self.harness_agent_type,
                ..SubagentRuntimeOverrides::default()
            },
            run_in_background: self.run_in_background,
            surface_completion: self.surface_completion,
            await_to_completion: !self.run_in_background,
            fork_context: self.fork_context,
            advisor_gate_prevalidated: self.advisor_gate_prevalidated,
            owner: SubagentOwner::Task,
            cancel_token: tokio_util::sync::CancellationToken::new(),
        }
    }

    /// Assemble a coordinator spawn envelope using the caller's own `result_tx`.
    pub fn into_request_with_result_tx(
        self,
        result_tx: tokio::sync::oneshot::Sender<SubagentResult>,
    ) -> SubagentSpawnRequest {
        SubagentSpawnRequest {
            request: Box::new(self.into_request()),
            result_tx,
        }
    }
}

/// Build the request from `params` and spawn it via `backend`, awaiting the
/// result.
///
/// Returns the backend's raw `Result` (same as calling
/// `backend.backend().spawn(request).await` directly) so each caller keeps
/// its own error handling: `TaskTool`/`AdvisorTool` propagate via `?` into
/// `ToolError` (their `run()` already returns
/// `Result<ToolOutput, ToolError>`), while `SubagentEconomyInvoker` maps the
/// error to `String`. A shared error enum would only add an indirection
/// layer both callers immediately unwrap, so the raw backend `Result` is
/// kept instead.
pub async fn spawn_and_await(
    backend: &SubagentBackendResource,
    params: SubagentSpawnParams,
) -> Result<SubagentResult, xai_tool_runtime::ToolError> {
    backend.backend().spawn(params.into_request()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_params() -> SubagentSpawnParams {
        SubagentSpawnParams {
            id: None,
            prompt: "do the thing".to_string(),
            description: "desc".to_string(),
            subagent_type: "general-purpose".to_string(),
            parent_session_id: "parent-session".to_string(),
            parent_prompt_id: Some("parent-prompt".to_string()),
            resume_from: Some("resume-id".to_string()),
            cwd: Some("/tmp/work".to_string()),
            model: Some("test-model".to_string()),
            model_override_provenance: ModelOverrideProvenance::Tool,
            reasoning_effort: Some("high".to_string()),
            persona: Some("reviewer".to_string()),
            capability_mode: Some(SubagentCapabilityMode::ReadOnly),
            isolation: Some(SubagentIsolationMode::Worktree),
            harness_agent_type: Some("cursor".to_string()),
            run_in_background: true,
            surface_completion: false,
            fork_context: true,
            advisor_gate_prevalidated: true,
        }
    }

    #[test]
    fn into_request_maps_every_knob() {
        let params = base_params();
        let request = params.into_request();

        assert_eq!(request.prompt, "do the thing");
        assert_eq!(request.description, "desc");
        assert_eq!(request.subagent_type, "general-purpose");
        assert_eq!(request.parent_session_id, "parent-session");
        assert_eq!(request.parent_prompt_id.as_deref(), Some("parent-prompt"));
        assert_eq!(request.resume_from.as_deref(), Some("resume-id"));
        assert_eq!(request.cwd.as_deref(), Some("/tmp/work"));
        assert_eq!(
            request.runtime_overrides.model.as_deref(),
            Some("test-model")
        );
        assert_eq!(
            request.runtime_overrides.model_override_provenance,
            ModelOverrideProvenance::Tool
        );
        assert_eq!(
            request.runtime_overrides.reasoning_effort.as_deref(),
            Some("high")
        );
        assert_eq!(
            request.runtime_overrides.persona.as_deref(),
            Some("reviewer")
        );
        assert_eq!(
            request.runtime_overrides.capability_mode,
            Some(SubagentCapabilityMode::ReadOnly)
        );
        assert_eq!(
            request.runtime_overrides.isolation,
            Some(SubagentIsolationMode::Worktree)
        );
        assert_eq!(
            request.runtime_overrides.harness_agent_type.as_deref(),
            Some("cursor")
        );
        assert!(request.run_in_background);
        assert!(!request.surface_completion);
        assert!(request.fork_context);
        assert!(request.advisor_gate_prevalidated);
    }

    #[test]
    fn into_request_generates_id_when_absent() {
        let params = base_params();
        let request = params.into_request();
        // A fresh UUIDv7 was generated (36-char hyphenated form).
        assert_eq!(request.id.len(), 36);
        assert!(uuid::Uuid::parse_str(&request.id).is_ok());
    }

    #[test]
    fn into_request_preserves_explicit_id() {
        let mut params = base_params();
        params.id = Some("explicit-id".to_string());
        let request = params.into_request();
        assert_eq!(request.id, "explicit-id");
    }
    #[test]
    fn into_request_with_result_tx_uses_callers_channel() {
        let params = base_params();
        let (result_tx, mut result_rx) = tokio::sync::oneshot::channel();
        let request = params.into_request_with_result_tx(result_tx);
        assert!(request.result_tx.send(SubagentResult::default()).is_ok());
        let received = result_rx
            .try_recv()
            .expect("caller's result_rx should observe the send through request.result_tx");
        assert!(!received.success);
    }

    // -- Per-site field-profile snapshots -----------------------------------
    //
    // These lock `into_request()` to the exact per-site literal profile each
    // call site (`AdvisorTool::run`, `SubagentEconomyInvoker::spawn_subagent`,
    // `spawn_research_subagent`, `TaskTool::run`) constructs today. If a
    // future edit to `into_request()` changes how a field maps, one of these
    // fails alongside `into_request_maps_every_knob` — a real behavior drift,
    // not a primitive-contract drift, to report rather than silently patch.

    /// Mirrors `AdvisorTool::run`'s `SubagentSpawnParams` literal
    /// (advisor/mod.rs): explicit id, `ADVISOR_SUBAGENT` type,
    /// `Harness` provenance, `ReadOnly` capability, synchronous
    /// (`run_in_background: false`), always surfaced and forked.
    #[test]
    fn into_request_matches_advisor_site_profile() {
        let params = SubagentSpawnParams {
            id: Some("advisor-id".to_string()),
            prompt: "review this".to_string(),
            description: "advisor review".to_string(),
            subagent_type: xai_tool_types::ADVISOR_SUBAGENT.name.to_string(),
            parent_session_id: "parent-session".to_string(),
            parent_prompt_id: Some("parent-prompt".to_string()),
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
            advisor_gate_prevalidated: true,
        };
        let request = params.into_request();

        assert_eq!(request.id, "advisor-id");
        assert_eq!(request.description, "advisor review");
        assert_eq!(request.subagent_type, xai_tool_types::ADVISOR_SUBAGENT.name);
        assert_eq!(request.resume_from, None);
        assert_eq!(request.cwd, None);
        assert_eq!(request.runtime_overrides.model, None);
        assert_eq!(
            request.runtime_overrides.model_override_provenance,
            ModelOverrideProvenance::Harness
        );
        assert_eq!(
            request.runtime_overrides.capability_mode,
            Some(SubagentCapabilityMode::ReadOnly)
        );
        assert_eq!(request.runtime_overrides.isolation, None);
        assert!(!request.run_in_background);
        assert!(request.surface_completion);
        assert!(request.fork_context);
        assert!(request.advisor_gate_prevalidated);
    }

    /// Mirrors `SubagentEconomyInvoker::spawn_subagent`'s literal
    /// (bounty.rs): generated id, caller-supplied `cwd`/`isolation`,
    /// `Harness` provenance, no capability mode, never surfaced, never
    /// forked, never advisor-gated.
    #[test]
    fn into_request_matches_bounty_site_profile() {
        let params = SubagentSpawnParams {
            id: None,
            prompt: "fix the bug".to_string(),
            description: "bounty attempt".to_string(),
            subagent_type: "bounty-worker".to_string(),
            parent_session_id: "bounty-parent-session".to_string(),
            parent_prompt_id: Some("bounty-parent-prompt".to_string()),
            resume_from: None,
            cwd: Some("/work/bounty".to_string()),
            model: None,
            model_override_provenance: ModelOverrideProvenance::Harness,
            reasoning_effort: None,
            persona: None,
            capability_mode: None,
            isolation: Some(SubagentIsolationMode::Worktree),
            harness_agent_type: None,
            run_in_background: false,
            surface_completion: false,
            fork_context: false,
            advisor_gate_prevalidated: false,
        };
        let request = params.into_request();

        assert_eq!(request.description, "bounty attempt");
        assert_eq!(request.subagent_type, "bounty-worker");
        assert_eq!(request.resume_from, None);
        assert_eq!(request.cwd.as_deref(), Some("/work/bounty"));
        assert_eq!(request.runtime_overrides.model, None);
        assert_eq!(
            request.runtime_overrides.model_override_provenance,
            ModelOverrideProvenance::Harness
        );
        assert_eq!(request.runtime_overrides.capability_mode, None);
        assert_eq!(
            request.runtime_overrides.isolation,
            Some(SubagentIsolationMode::Worktree)
        );
        assert!(!request.run_in_background);
        assert!(!request.surface_completion);
        assert!(!request.fork_context);
        assert!(!request.advisor_gate_prevalidated);
    }

    /// Mirrors `spawn_research_subagent`'s literal (research.rs): generated
    /// id, fixed `"deep-research"` type, `Harness` provenance, `ReadOnly`
    /// capability, `cwd`/`isolation` unset, all boolean flags false.
    #[test]
    fn into_request_matches_research_site_profile() {
        let params = SubagentSpawnParams {
            id: None,
            prompt: "synthesize deep research evidence".to_string(),
            description: "synthesize deep research evidence".to_string(),
            subagent_type: "deep-research".to_string(),
            parent_session_id: "research-parent-session".to_string(),
            parent_prompt_id: Some("research-parent-prompt".to_string()),
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
            surface_completion: false,
            fork_context: false,
            advisor_gate_prevalidated: false,
        };
        let request = params.into_request();

        assert_eq!(request.subagent_type, "deep-research");
        assert_eq!(request.cwd, None);
        assert_eq!(request.runtime_overrides.model, None);
        assert_eq!(
            request.runtime_overrides.model_override_provenance,
            ModelOverrideProvenance::Harness
        );
        assert_eq!(
            request.runtime_overrides.capability_mode,
            Some(SubagentCapabilityMode::ReadOnly)
        );
        assert_eq!(request.runtime_overrides.isolation, None);
        assert!(!request.run_in_background);
        assert!(!request.surface_completion);
        assert!(!request.fork_context);
        assert!(!request.advisor_gate_prevalidated);
    }

    /// Mirrors `TaskTool::run`'s literal (task/mod.rs): id passthrough
    /// (`input.task_id` when present), `Tool` provenance, input-controlled
    /// `capability_mode`/`isolation`/`run_in_background`, always surfaced,
    /// and `fork_context` mirroring `is_advisor_subagent(subagent_type)` —
    /// checked for both the advisor and non-advisor branches.
    #[test]
    fn into_request_matches_task_site_profile_non_advisor() {
        let params = SubagentSpawnParams {
            id: Some("task-id".to_string()),
            prompt: "do the task".to_string(),
            description: "task desc".to_string(),
            subagent_type: "general-purpose".to_string(),
            parent_session_id: "task-parent-session".to_string(),
            parent_prompt_id: Some("task-parent-prompt".to_string()),
            resume_from: Some("resume-id".to_string()),
            cwd: Some("/work/task".to_string()),
            model: Some("task-model".to_string()),
            model_override_provenance: ModelOverrideProvenance::Tool,
            reasoning_effort: None,
            persona: None,
            capability_mode: Some(SubagentCapabilityMode::ReadOnly),
            isolation: Some(SubagentIsolationMode::Worktree),
            harness_agent_type: None,
            run_in_background: false,
            surface_completion: true,
            // `is_advisor_subagent("general-purpose")` is false.
            fork_context: false,
            advisor_gate_prevalidated: false,
        };
        let request = params.into_request();

        assert_eq!(request.id, "task-id");
        assert_eq!(request.subagent_type, "general-purpose");
        assert_eq!(request.resume_from.as_deref(), Some("resume-id"));
        assert_eq!(request.cwd.as_deref(), Some("/work/task"));
        assert_eq!(
            request.runtime_overrides.model.as_deref(),
            Some("task-model")
        );
        assert_eq!(
            request.runtime_overrides.model_override_provenance,
            ModelOverrideProvenance::Tool
        );
        assert_eq!(
            request.runtime_overrides.capability_mode,
            Some(SubagentCapabilityMode::ReadOnly)
        );
        assert_eq!(
            request.runtime_overrides.isolation,
            Some(SubagentIsolationMode::Worktree)
        );
        assert!(request.surface_completion);
        assert!(!request.fork_context);
    }

    #[test]
    fn into_request_matches_task_site_profile_advisor() {
        let mut params = SubagentSpawnParams {
            id: Some("task-advisor-id".to_string()),
            prompt: "do the task".to_string(),
            description: "task desc".to_string(),
            subagent_type: xai_tool_types::ADVISOR_SUBAGENT.name.to_string(),
            parent_session_id: "task-parent-session".to_string(),
            parent_prompt_id: None,
            resume_from: None,
            cwd: None,
            model: None,
            model_override_provenance: ModelOverrideProvenance::Tool,
            reasoning_effort: None,
            persona: None,
            capability_mode: None,
            isolation: None,
            harness_agent_type: None,
            run_in_background: false,
            surface_completion: true,
            // `is_advisor_subagent(ADVISOR_SUBAGENT.name)` is true.
            fork_context: true,
            advisor_gate_prevalidated: false,
        };
        // TaskTool never sets `harness_agent_type`; assert it's unchanged by
        // `into_request` regardless (locks the runtime-overrides mapping,
        // not just this site's literal value).
        params.harness_agent_type = None;
        let request = params.into_request();

        assert_eq!(request.subagent_type, xai_tool_types::ADVISOR_SUBAGENT.name);
        assert_eq!(request.runtime_overrides.harness_agent_type, None);
        assert!(request.fork_context);
        assert!(request.surface_completion);
    }
}
