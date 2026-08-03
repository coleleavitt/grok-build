//! One-shot bounty market tool.
//!
//! A real tool flow: post/open a bounty, spawn solver subagents in isolated
//! worktrees, validate surviving patches, settle, and apply the winning patch.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use crate::implementations::grok_build::task::backend::SubagentBackendResource;
use crate::implementations::grok_build::task::spawn::{SubagentSpawnParams, spawn_and_await};
use crate::implementations::grok_build::task::types::{
    CurrentPromptIdResource, ModelOverrideProvenance, SessionIdResource, SubagentForegroundWait,
};
use crate::types::output::{TextOutput, ToolOutput};
use crate::types::resources::{Cwd, State};
use crate::types::tool::{ToolKind, ToolNamespace};
use tokio::sync::Mutex;
use xai_grok_economy::{
    AgentId, AgentInvoker, BountyReport, Charter, MarketOrchestrator, Solution, SwarmProvider,
    ValidatorOutcome, ValidatorPrompt,
};
use xai_tool_types::SubagentIsolationMode;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BountyAction {
    Run,
    Status,
}

impl Default for BountyAction {
    fn default() -> Self {
        Self::Run
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct BountyInput {
    #[serde(default)]
    pub action: BountyAction,
    #[serde(default)]
    #[schemars(
        description = "Existing bounty id. If omitted with action=run, a new bounty is posted and run."
    )]
    pub bounty_id: Option<String>,
    #[serde(default)]
    #[schemars(description = "Task/bug/feature description for a new bounty.")]
    pub description: Option<String>,
    #[serde(default)]
    #[schemars(description = "Acceptance criteria for a new bounty.")]
    pub acceptance_criteria: Option<String>,
    #[serde(default)]
    #[schemars(description = "Token budget/reward for a new bounty. Defaults to 1000.")]
    pub budget: Option<u64>,
    #[serde(default)]
    #[schemars(description = "Number of solver agents. Defaults to 2, capped at 5.")]
    pub max_solvers: Option<u8>,
    #[serde(default)]
    #[schemars(description = "Validators per solution. Defaults to 1, capped at 3.")]
    pub validators: Option<u8>,
}

#[derive(Clone)]
struct BountyMarket {
    orchestrator: Arc<Mutex<MarketOrchestrator>>,
}

impl Default for BountyMarket {
    fn default() -> Self {
        Self {
            orchestrator: Arc::new(Mutex::new(MarketOrchestrator::new(Charter::default()))),
        }
    }
}

#[derive(Debug, Default)]
pub struct BountyTool;

impl crate::types::tool_metadata::ToolMetadata for BountyTool {
    fn kind(&self) -> ToolKind {
        ToolKind::Bounty
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::GrokBuild
    }

    fn description_template(&self) -> &str {
        "Run a complete bounty flow in one call: post/open a code bounty, spawn isolated solver subagents, validate candidate patches, settle the winner, apply the winning patch, or show market status."
    }

    fn is_read_only(&self) -> bool {
        false
    }
}

impl xai_tool_runtime::Tool for BountyTool {
    type Args = BountyInput;
    type Output = ToolOutput;

    fn id(&self) -> xai_tool_protocol::ToolId {
        xai_tool_protocol::ToolId::new("bounty").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &xai_tool_runtime::ListToolsContext,
    ) -> xai_tool_types::ToolDescription {
        xai_tool_types::ToolDescription::new(
            "bounty",
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

    #[tracing::instrument(name = "tool.bounty", skip_all)]
    async fn run(
        &self,
        ctx: xai_tool_runtime::ToolCallContext,
        input: BountyInput,
    ) -> Result<ToolOutput, xai_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;
        let (market, cwd, backend, parent_session_id, parent_prompt_id, foreground_wait) = {
            let mut res = resources.lock().await;
            let market = res.get_or_default::<State<BountyMarket>>().0.clone();
            let cwd = res
                .get::<Cwd>()
                .map(|cwd| cwd.0.clone())
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
            let backend = res.get::<SubagentBackendResource>().cloned().ok_or_else(|| {
                xai_tool_runtime::ToolError::custom(
                    "missing_resource",
                    "bounty requires SubagentBackendResource so it can spawn real solver/validator agents",
                )
            })?;
            let parent_session_id = res
                .get::<SessionIdResource>()
                .map(|s| s.0.clone())
                .unwrap_or_default();
            let parent_prompt_id = res.get::<CurrentPromptIdResource>().map(|p| p.0.clone());
            let foreground_wait = res.get::<SubagentForegroundWait>().cloned();
            (
                market,
                cwd,
                backend,
                parent_session_id,
                parent_prompt_id,
                foreground_wait,
            )
        };

        let text = match input.action {
            BountyAction::Status => market_status(&market, &cwd).await,
            BountyAction::Run => {
                run_bounty(
                    input,
                    &market,
                    &cwd,
                    backend,
                    parent_session_id,
                    parent_prompt_id,
                    foreground_wait,
                )
                .await
            }
        };
        Ok(ToolOutput::Text(TextOutput::from(text)))
    }
}

async fn run_bounty(
    input: BountyInput,
    market: &BountyMarket,
    cwd: &Path,
    backend: SubagentBackendResource,
    parent_session_id: String,
    parent_prompt_id: Option<String>,
    foreground_wait: Option<SubagentForegroundWait>,
) -> String {
    let bounty_id = if let Some(id) = input.bounty_id.clone() {
        id
    } else {
        let description = input
            .description
            .clone()
            .unwrap_or_else(|| "Implement the requested change".to_owned());
        let acceptance = input.acceptance_criteria.clone().unwrap_or_else(|| {
            "A targeted verification command passes and the patch is reviewable".to_owned()
        });
        let mut orch = market.orchestrator.lock().await;
        match orch.post_bounty(
            description,
            input.budget.unwrap_or(1_000),
            acceptance,
            input.max_solvers,
        ) {
            Ok(id) => {
                record_bounty_event(
                    cwd,
                    &id,
                    "posted",
                    serde_json::json!({
                        "budget": input.budget.unwrap_or(1_000),
                        "max_solvers": input.max_solvers,
                    }),
                );
                id
            }
            Err(error) => return format!("bounty post failed: {error}"),
        }
    };

    let invoker = SubagentEconomyInvoker {
        backend,
        cwd: cwd.to_path_buf(),
        parent_session_id,
        parent_prompt_id,
        foreground_wait,
    };
    let swarm = ExistingSubagentWorktreeSwarm;
    let n_solvers = input.max_solvers.unwrap_or(2).clamp(1, 5);
    let n_validators = input.validators.unwrap_or(1).clamp(1, 3);
    record_bounty_event(
        cwd,
        &bounty_id,
        "dispatch_started",
        serde_json::json!({
            "n_solvers": n_solvers,
            "n_validators": n_validators,
        }),
    );
    let outcome = {
        let mut orch = market.orchestrator.lock().await;
        orch.run_bounty_cycle(&bounty_id, &invoker, &swarm, n_solvers, n_validators)
            .await
    };

    match outcome {
        Ok(outcome) => {
            let applied =
                apply_winning_solution(cwd, &bounty_id, outcome.winning_solution.as_ref());
            cleanup_worktrees(cwd, &outcome.report);
            let brain = persist_bounty_to_brain_default(cwd, &bounty_id, &outcome, &applied)
                .map(|page| format!("Brain page `{}`", page.title))
                .unwrap_or_else(|error| format!("Brain persistence failed: {error}"));
            record_bounty_event(
                cwd,
                &bounty_id,
                "settled",
                serde_json::json!({
                    "winner": outcome.settlement.winner.as_ref().map(|winner| winner.label()),
                    "total_cost": outcome.settlement.total_cost,
                    "payouts": outcome.settlement.payouts.len(),
                    "trust_updates": outcome.settlement.trust_updates.len(),
                    "apply_summary": applied.summary.clone(),
                    "brain": brain.clone(),
                }),
            );
            format!(
                "Bounty `{}` settled.\nWinner: {}\nTotal cost: {} tok\nPayouts: {}\nTrust updates: {}\n{}\n{}",
                bounty_id,
                outcome
                    .settlement
                    .winner
                    .as_ref()
                    .map(|winner| winner.label())
                    .unwrap_or("(no winning solution)"),
                outcome.settlement.total_cost,
                outcome.settlement.payouts.len(),
                outcome.settlement.trust_updates.len(),
                applied.summary,
                brain,
            )
        }
        Err(error) => {
            record_bounty_event(
                cwd,
                &bounty_id,
                "failed",
                serde_json::json!({ "error": error.to_string() }),
            );
            format!("bounty `{bounty_id}` failed: {error}")
        }
    }
}

async fn market_status(market: &BountyMarket, cwd: &Path) -> String {
    let orch = market.orchestrator.lock().await;
    let mut out = format!(
        "**Bounty market**\n\n- Spend: {} tok used / {} tok remaining\n- Audit events: {}",
        orch.ledger().total_spent(),
        orch.ledger().remaining(),
        orch.bounties().audit_log().len()
    );
    for entry in orch.bounties().audit_log().iter().rev().take(10) {
        out.push_str(&format!("\n- `{}`: {:?}", entry.bounty_id, entry.event));
    }
    let durable = durable_bounty_activity(cwd);
    if !durable.is_empty() {
        out.push_str("\n\n**Durable bounty activity**");
        out.push_str(&durable);
    }
    out
}

fn durable_bounty_activity(cwd: &Path) -> String {
    let root = cwd.join(".grok").join("bounties");
    let Ok(entries) = std::fs::read_dir(root) else {
        return String::new();
    };
    let mut rows = Vec::new();
    for entry in entries.flatten() {
        let bounty_id = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path().join("events.jsonl");
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in text.lines().rev().take(3) {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let kind = value
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            let when = value
                .get("created_at_ms")
                .and_then(serde_json::Value::as_i64)
                .map(format_ms)
                .unwrap_or_else(|| "unknown time".to_owned());
            let payload = value
                .get("payload")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            rows.push(format!(
                "\n- `{bounty_id}` · {kind} · {when} · {}",
                one_line(&payload.to_string())
                    .chars()
                    .take(160)
                    .collect::<String>()
            ));
            if rows.len() >= 10 {
                return rows.join("");
            }
        }
    }
    rows.join("")
}

fn format_ms(ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
        .map(|dt| dt.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| format!("{ms}ms"))
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

struct ExistingSubagentWorktreeSwarm;

#[async_trait::async_trait]
impl SwarmProvider for ExistingSubagentWorktreeSwarm {
    async fn create_worktree(&self, _bounty_id: &str, _agent_id: &AgentId) -> Option<PathBuf> {
        None
    }

    async fn remove_worktree(&self, _path: &Path) {}
}

struct SubagentEconomyInvoker {
    backend: SubagentBackendResource,
    cwd: PathBuf,
    parent_session_id: String,
    parent_prompt_id: Option<String>,
    foreground_wait: Option<SubagentForegroundWait>,
}

#[async_trait::async_trait]
impl AgentInvoker for SubagentEconomyInvoker {
    async fn invoke_solver(
        &self,
        prompt: xai_grok_economy::SolverPrompt,
    ) -> Result<Solution, String> {
        let task_prompt = format!(
            "You are a competitive solver agent in a code-bounty market.\n\n\
             Bounty: {}\n\nDescription:\n{}\n\nAcceptance criteria:\n{}\n\n\
             Work in your isolated worktree. Implement the smallest correct patch and run a relevant verification command.\n\
             Final response: concise summary only; the parent will collect `git diff HEAD` from your worktree.",
            prompt.bounty_id, prompt.bounty_description, prompt.acceptance_criteria,
        );
        let result = self
            .spawn_subagent(
                format!("economy solver {}", prompt.agent_id.label()),
                "general-purpose",
                task_prompt,
                Some(SubagentIsolationMode::Worktree),
            )
            .await?;
        if !result.success {
            return Err(result.error.unwrap_or_else(|| result.output.to_string()));
        }
        let worktree = result.worktree_path.as_deref().map(PathBuf::from);
        let patch = match worktree.as_deref() {
            Some(path) => git_diff(path).await.unwrap_or_default(),
            None => String::new(),
        };
        let verification = match worktree.as_deref() {
            Some(path) => verify_bounty_solution(path, &patch).await,
            None => MechanisticVerification {
                passed: false,
                summary: "no solver worktree was returned".to_owned(),
            },
        };
        Ok(Solution {
            agent_id: prompt.agent_id,
            bounty_id: prompt.bounty_id,
            patch,
            explanation: format!(
                "{}\n\nMechanistic verification: {}",
                result.output, verification.summary
            ),
            self_assessment: if verification.passed { 0.8 } else { 0.2 },
            tokens_consumed: result.tokens_used.max(estimate_tokens(&result.output)),
            compiles: Some(verification.passed),
            tests_pass: Some(verification.passed),
            suspicious: !verification.passed,
            worktree_path: worktree,
        })
    }

    async fn invoke_validator(&self, prompt: ValidatorPrompt) -> Result<ValidatorOutcome, String> {
        let validator_prompt = format!(
            "You are an adversarial validator in a code-bounty market. Find a real flaw in the submitted patch.\n\n\
             Bounty {} — {}\n\nPatch:\n```diff\n{}\n```\n\nSolver explanation:\n{}\n\n\
             Output exactly:\nFLAW: <description or NONE>\nCONFIDENCE: <0.0-1.0>\nTEST: <minimal Rust test code that proves the flaw, or NONE>",
            prompt.bounty_id,
            prompt.bounty_description,
            prompt
                .solution
                .patch
                .chars()
                .take(12_000)
                .collect::<String>(),
            prompt
                .solution
                .explanation
                .chars()
                .take(2_000)
                .collect::<String>(),
        );
        let result = self
            .spawn_subagent(
                format!("economy validator {}", prompt.validator_id.label()),
                "plan",
                validator_prompt,
                None,
            )
            .await?;
        if !result.success {
            return Err(result.error.unwrap_or_else(|| result.output.to_string()));
        }
        let (flaw, confidence, test_code) = parse_validator_output(&result.output);
        Ok(ValidatorOutcome {
            flaw,
            test_code,
            confidence,
            tokens_consumed: result.tokens_used.max(estimate_tokens(&result.output)),
        })
    }

    async fn adjudicate_test(&self, test_code: &str, worktree: Option<&Path>) -> bool {
        let Some(worktree) = worktree else {
            return false;
        };
        adjudicate_rust_test(worktree, test_code).await
    }
}

impl SubagentEconomyInvoker {
    async fn spawn_subagent(
        &self,
        description: String,
        subagent_type: &str,
        prompt: String,
        isolation: Option<SubagentIsolationMode>,
    ) -> Result<crate::implementations::grok_build::task::types::SubagentResult, String> {
        let params = SubagentSpawnParams {
            id: None,
            prompt,
            description,
            subagent_type: subagent_type.to_owned(),
            parent_session_id: self.parent_session_id.clone(),
            parent_prompt_id: self.parent_prompt_id.clone(),
            resume_from: None,
            cwd: Some(self.cwd.display().to_string()),
            model: None,
            model_override_provenance: ModelOverrideProvenance::Harness,
            reasoning_effort: None,
            persona: None,
            capability_mode: None,
            isolation,
            harness_agent_type: None,
            run_in_background: false,
            surface_completion: false,
            fork_context: false,
            advisor_gate_prevalidated: false,
        };
        spawn_and_await(&self.backend, params, self.foreground_wait.as_ref())
            .await
            .map_err(|error| error.to_string())
    }
}

async fn git_diff(worktree: &Path) -> Result<String, String> {
    let out = tokio::process::Command::new("git")
        .args(["diff", "HEAD"])
        .current_dir(worktree)
        .output()
        .await
        .map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn estimate_tokens(text: &str) -> u64 {
    (text.len() as u64 / 4).max(100)
}

#[derive(Debug)]
struct MechanisticVerification {
    passed: bool,
    summary: String,
}

async fn verify_bounty_solution(worktree: &Path, patch: &str) -> MechanisticVerification {
    if patch.trim().is_empty() {
        return MechanisticVerification {
            passed: false,
            summary: "solver produced no git diff".to_owned(),
        };
    }
    if patch
        .lines()
        .any(|line| line.starts_with('-') && line.contains("#[test]"))
    {
        return MechanisticVerification {
            passed: false,
            summary: "charter violation: patch deletes existing #[test] annotations".to_owned(),
        };
    }
    let Some((program, args, label)) = verification_command(worktree) else {
        return MechanisticVerification {
            passed: true,
            summary: "patch present; no known verification command detected".to_owned(),
        };
    };
    let result = tokio::time::timeout(std::time::Duration::from_secs(120), {
        let mut command = tokio::process::Command::new(program);
        command
            .args(args)
            .current_dir(worktree)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.output()
    })
    .await;
    match result {
        Ok(Ok(output)) if output.status.success() => MechanisticVerification {
            passed: true,
            summary: format!("{label} passed"),
        },
        Ok(Ok(output)) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let text = if stderr.trim().is_empty() {
                stdout
            } else {
                stderr
            };
            MechanisticVerification {
                passed: false,
                summary: format!("{label} failed: {}", truncate(text.trim(), 800)),
            }
        }
        Ok(Err(error)) => MechanisticVerification {
            passed: false,
            summary: format!("failed to run {label}: {error}"),
        },
        Err(_) => MechanisticVerification {
            passed: false,
            summary: format!("{label} timed out after 120s"),
        },
    }
}

fn verification_command(
    root: &Path,
) -> Option<(&'static str, &'static [&'static str], &'static str)> {
    if root.join("Cargo.toml").exists() {
        return Some(("cargo", &["test", "--quiet"], "cargo test --quiet"));
    }
    if root.join("package.json").exists() {
        return Some(("npm", &["test", "--", "--runInBand"], "npm test"));
    }
    if root.join("go.mod").exists() {
        return Some(("go", &["test", "./..."], "go test ./..."));
    }
    if root.join("pyproject.toml").exists() || root.join("pytest.ini").exists() {
        return Some(("python", &["-m", "pytest"], "python -m pytest"));
    }
    None
}

fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &text[..end])
}

async fn adjudicate_rust_test(worktree: &Path, test_code: &str) -> bool {
    if !worktree.join("Cargo.toml").exists() || test_code.trim().is_empty() {
        return false;
    }
    let test_dir = worktree.join("tests");
    let test_file = test_dir.join("_bounty_validator_test.rs");
    if std::fs::create_dir_all(&test_dir).is_err() || std::fs::write(&test_file, test_code).is_err()
    {
        return false;
    }
    let result = tokio::process::Command::new("cargo")
        .args([
            "test",
            "--test",
            "_bounty_validator_test",
            "--",
            "--nocapture",
        ])
        .current_dir(worktree)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await;
    std::fs::remove_file(&test_file).ok();
    result.is_ok_and(|output| !output.status.success())
}

struct AppliedSolution {
    summary: String,
}

fn apply_winning_solution(
    cwd: &Path,
    bounty_id: &str,
    solution: Option<&Solution>,
) -> AppliedSolution {
    let Some(solution) = solution else {
        return AppliedSolution {
            summary: "No winning solution — nothing written.".to_owned(),
        };
    };
    if solution.tests_pass == Some(false) || solution.suspicious {
        return AppliedSolution {
            summary:
                "Refused to apply winning solution: validation marked it suspicious or failing."
                    .to_owned(),
        };
    }
    let audit_dir = cwd.join(".grok").join("bounties").join(bounty_id);
    if let Err(error) = std::fs::create_dir_all(&audit_dir) {
        return AppliedSolution {
            summary: format!("Failed to create audit dir: {error}"),
        };
    }
    let patch_path = audit_dir.join("winner.patch");
    std::fs::write(&patch_path, &solution.patch).ok();
    std::fs::write(audit_dir.join("winner.md"), &solution.explanation).ok();
    if looks_like_unified_diff(&solution.patch) {
        match std::process::Command::new("git")
            .arg("-C")
            .arg(cwd)
            .arg("apply")
            .arg("--whitespace=nowarn")
            .arg(&patch_path)
            .output()
        {
            Ok(output) if output.status.success() => AppliedSolution {
                summary: format!("Applied winning diff (audit: {}).", audit_dir.display()),
            },
            Ok(output) => AppliedSolution {
                summary: format!(
                    "Winner patch saved to {}, but git apply failed: {}",
                    audit_dir.display(),
                    String::from_utf8_lossy(&output.stderr)
                ),
            },
            Err(error) => AppliedSolution {
                summary: format!(
                    "Winner patch saved to {}, but git apply could not run: {error}",
                    audit_dir.display()
                ),
            },
        }
    } else {
        AppliedSolution {
            summary: format!(
                "Winner did not produce a unified diff; audit copy at {}.",
                audit_dir.display()
            ),
        }
    }
}

fn record_bounty_event(cwd: &Path, bounty_id: &str, kind: &str, payload: serde_json::Value) {
    let dir = cwd.join(".grok").join("bounties").join(bounty_id);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let row = serde_json::json!({
        "kind": kind,
        "payload": payload,
        "created_at_ms": chrono::Utc::now().timestamp_millis(),
    });
    if let Ok(mut line) = serde_json::to_string(&row) {
        line.push('\n');
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("events.jsonl"))
            .and_then(|mut file| {
                use std::io::Write;
                file.write_all(line.as_bytes())
            });
    }
}

fn cleanup_worktrees(cwd: &Path, report: &BountyReport) {
    for path in report
        .solvers
        .iter()
        .filter_map(|solver| solver.worktree_path.as_deref())
    {
        let _ = std::process::Command::new("git")
            .arg("-C")
            .arg(cwd)
            .arg("worktree")
            .arg("remove")
            .arg("--force")
            .arg(path)
            .output();
    }
}

fn persist_bounty_to_brain_default(
    workspace_scope: &Path,
    bounty_id: &str,
    outcome: &xai_grok_economy::CycleOutcome,
    applied: &AppliedSolution,
) -> xai_grok_brain::Result<xai_grok_brain::MemoryPage> {
    persist_bounty_to_brain_at(
        &xai_grok_brain::default_store_path(),
        workspace_scope,
        bounty_id,
        outcome,
        applied,
    )
}

fn persist_bounty_to_brain_at(
    store_path: &Path,
    workspace_scope: &Path,
    bounty_id: &str,
    outcome: &xai_grok_economy::CycleOutcome,
    applied: &AppliedSolution,
) -> xai_grok_brain::Result<xai_grok_brain::MemoryPage> {
    use xai_grok_brain::{MemoryCategory, MemorySourceType, NewPage};

    let service = xai_grok_brain::BrainService::open(store_path)?;
    if !service.store().settings_initialized()? {
        service.update_settings(xai_grok_brain::BrainSettingsUpdate {
            enabled: Some(true),
            ..Default::default()
        })?;
    }
    let winner = outcome
        .settlement
        .winner
        .as_ref()
        .map(|winner| winner.label())
        .unwrap_or("(none)");
    let body = format!(
        "Bounty `{bounty_id}` settled for `{}`.\n\nAcceptance criteria:\n{}\n\nWinner: {winner}\nTotal cost: {} tok\nPayouts: {}\nTrust updates: {}\nApply result: {}\n\nSolvers:\n{}",
        outcome.report.description,
        outcome.report.acceptance_criteria,
        outcome.settlement.total_cost,
        outcome.settlement.payouts.len(),
        outcome.settlement.trust_updates.len(),
        applied.summary,
        outcome
            .report
            .solvers
            .iter()
            .map(|solver| format!(
                "- {} status={} compiles={:?} tests={:?} suspicious={}",
                solver.agent_id.label(),
                solver.status,
                solver.compiles,
                solver.tests_pass,
                solver.suspicious
            ))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let scope = workspace_scope.display().to_string();
    let page = service.store().create_or_update_page_by_title_scoped(
        NewPage {
            title: Some(format!("Bounty: {bounty_id}")),
            memory_text: body,
            category: MemoryCategory::Workstreams,
            source: Some("bounty_tool".to_owned()),
        },
        Some(&scope),
    )?;
    service.store().add_source_if_missing(
        page.id,
        MemorySourceType::Manual,
        &format!("Grok Build bounty {bounty_id}"),
        Some(&format!("bounty:{bounty_id}")),
        None,
    )?;
    Ok(page)
}

fn looks_like_unified_diff(text: &str) -> bool {
    text.lines()
        .any(|line| line.starts_with("diff --git ") || line.starts_with("--- "))
        && text.lines().any(|line| line.starts_with("+++ "))
        && text.lines().any(|line| line.starts_with("@@"))
}

fn parse_validator_output(text: &str) -> (Option<String>, f32, Option<String>) {
    let mut flaw = None;
    let mut confidence = 0.0f32;
    let mut test_code = None;
    let mut current: Option<&str> = None;
    let mut buf = String::new();
    let flush = |key: Option<&str>,
                 buf: &mut String,
                 flaw: &mut Option<String>,
                 confidence: &mut f32,
                 test_code: &mut Option<String>| {
        let value = buf.trim().to_owned();
        match key {
            Some("FLAW") if !value.is_empty() && !value.eq_ignore_ascii_case("none") => {
                *flaw = Some(value)
            }
            Some("CONFIDENCE") => {
                if let Ok(parsed) = value.parse::<f32>() {
                    *confidence = parsed.clamp(0.0, 1.0);
                }
            }
            Some("TEST") if !value.is_empty() && !value.eq_ignore_ascii_case("none") => {
                *test_code = Some(value)
            }
            _ => {}
        }
        buf.clear();
    };
    for line in text.lines() {
        let trimmed = line.trim();
        let key = ["FLAW", "CONFIDENCE", "TEST"]
            .iter()
            .find(|key| trimmed.to_ascii_uppercase().starts_with(&format!("{key}:")))
            .copied();
        if let Some(key) = key {
            flush(
                current,
                &mut buf,
                &mut flaw,
                &mut confidence,
                &mut test_code,
            );
            current = Some(key);
            if let Some((_, rest)) = trimmed.split_once(':') {
                buf.push_str(rest.trim());
            }
        } else if current.is_some() {
            if !buf.is_empty() {
                buf.push('\n');
            }
            buf.push_str(line);
        }
    }
    flush(
        current,
        &mut buf,
        &mut flaw,
        &mut confidence,
        &mut test_code,
    );
    (flaw, confidence, test_code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validator_output_parser_is_tolerant() {
        let (flaw, confidence, test) =
            parse_validator_output("FLAW: bad edge\nCONFIDENCE: 0.7\nTEST: #[test] fn repro() {}");
        assert_eq!(flaw.as_deref(), Some("bad edge"));
        assert_eq!(confidence, 0.7);
        assert!(test.unwrap().contains("repro"));
    }

    #[test]
    fn suspicious_solution_is_not_applied() {
        let solution = Solution {
            agent_id: AgentId::from_label("solver"),
            bounty_id: "b1".into(),
            patch: "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-a\n+b\n".into(),
            explanation: "x".into(),
            self_assessment: 0.1,
            tokens_consumed: 1,
            compiles: Some(false),
            tests_pass: Some(false),
            suspicious: true,
            worktree_path: None,
        };
        let applied = apply_winning_solution(Path::new("."), "b1", Some(&solution));
        assert!(applied.summary.contains("Refused"));
    }

    #[test]
    fn bounty_outcome_persists_to_brain_scope() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("brain.sqlite");
        let workspace = dir.path().join("repo");
        std::fs::create_dir_all(&workspace).unwrap();
        let agent = AgentId::from_label("solver");
        let outcome = xai_grok_economy::CycleOutcome {
            settlement: xai_grok_economy::types::Settlement {
                bounty_id: "b1".into(),
                winner: Some(agent.clone()),
                payouts: vec![],
                trust_updates: vec![],
                total_cost: 42,
            },
            winning_solution: None,
            report: BountyReport {
                bounty_id: "b1".into(),
                description: "Fix bug".into(),
                acceptance_criteria: "tests pass".into(),
                state: "Complete".into(),
                solvers: vec![xai_grok_economy::SolverReport {
                    agent_id: agent,
                    status: "Completed".into(),
                    tokens_consumed: 1,
                    compiles: Some(true),
                    tests_pass: Some(true),
                    suspicious: false,
                    worktree_path: None,
                }],
                validations: vec![],
                settlement: None,
                total_spent: 42,
                remaining_budget: 0,
                warnings: vec![],
            },
        };
        let applied = AppliedSolution {
            summary: "Applied".into(),
        };

        let page = persist_bounty_to_brain_at(&db, &workspace, "b1", &outcome, &applied).unwrap();

        assert_eq!(page.category, xai_grok_brain::MemoryCategory::Workstreams);
        assert_eq!(page.scope_id.as_deref(), Some(workspace.to_str().unwrap()));
        let sources = xai_grok_brain::BrainService::open(&db)
            .unwrap()
            .sources(page.id)
            .unwrap();
        assert_eq!(sources.len(), 1);
    }

    #[test]
    fn durable_bounty_activity_reads_event_files() {
        let dir = tempfile::tempdir().unwrap();
        record_bounty_event(
            dir.path(),
            "b1",
            "settled",
            serde_json::json!({"winner": "solver-0"}),
        );

        let rendered = durable_bounty_activity(dir.path());

        assert!(rendered.contains("`b1`"));
        assert!(rendered.contains("settled"));
        assert!(rendered.contains("solver-0"));
    }
}
