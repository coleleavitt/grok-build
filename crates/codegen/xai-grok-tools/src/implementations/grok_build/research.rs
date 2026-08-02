//! One-shot deep research tool.
//!
//! This is intentionally a real tool flow, not a slash-command wrapper: it
//! plans subqueries, searches external providers plus the local checkout, and
//! optionally asks the built-in `deep-research` subagent to synthesize the
//! gathered evidence. If no subagent backend is available, it still returns a
//! deterministic cited report.

use crate::implementations::grok_build::task::backend::SubagentBackendResource;
use crate::implementations::grok_build::task::spawn::{SubagentSpawnParams, spawn_and_await};
use crate::implementations::grok_build::task::types::{
    CurrentPromptIdResource, ModelOverrideProvenance, SessionIdResource,
};
use crate::types::output::{TextOutput, ToolOutput};
use crate::types::resources::Cwd;
use crate::types::tool::{ToolKind, ToolNamespace};
use std::path::{Path, PathBuf};
use xai_tool_types::SubagentCapabilityMode;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ResearchInput {
    #[schemars(description = "Research question to answer.")]
    pub question: String,
    #[serde(default)]
    #[schemars(description = "Maximum search steps. Defaults to 6, capped at 8.")]
    pub max_steps: Option<usize>,
    #[serde(default)]
    #[schemars(description = "Search results per step. Defaults to 5, capped at 10.")]
    pub results_per_step: Option<usize>,
    #[serde(default)]
    #[schemars(description = "Also write markdown/json artifacts under .grok/research.")]
    pub export: Option<bool>,
}

#[derive(Debug, Clone)]
struct ResearchStep {
    sub_query: String,
    search_query: String,
    outcome: StepOutcome,
}

#[derive(Debug, Clone)]
enum StepOutcome {
    Found(String),
    Failed(String),
}

impl ResearchStep {
    fn succeeded(&self) -> bool {
        matches!(self.outcome, StepOutcome::Found(_))
    }

    fn evidence(&self) -> Option<&str> {
        match &self.outcome {
            StepOutcome::Found(text) => Some(text.as_str()),
            StepOutcome::Failed(_) => None,
        }
    }
}

#[derive(Debug, Default)]
pub struct ResearchTool;

impl crate::types::tool_metadata::ToolMetadata for ResearchTool {
    fn kind(&self) -> ToolKind {
        ToolKind::Research
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::GrokBuild
    }

    fn description_template(&self) -> &str {
        "Run a complete deep-research flow in one tool call: plan focused subqueries, search external providers and the local repository, synthesize a cited answer, and optionally export artifacts."
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

impl xai_tool_runtime::Tool for ResearchTool {
    type Args = ResearchInput;
    type Output = ToolOutput;

    fn id(&self) -> xai_tool_protocol::ToolId {
        xai_tool_protocol::ToolId::new("research").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &xai_tool_runtime::ListToolsContext,
    ) -> xai_tool_types::ToolDescription {
        xai_tool_types::ToolDescription::new(
            "research",
            crate::types::tool_metadata::ToolMetadata::description_template(self),
        )
    }

    fn capabilities(&self) -> xai_tool_protocol::ToolCapabilities {
        xai_tool_protocol::ToolCapabilities {
            is_read_only: true,
            tool_scope: Some(xai_tool_protocol::ToolScope::Read),
            ..Default::default()
        }
    }

    #[tracing::instrument(name = "tool.research", skip_all, fields(question = %input.question.chars().take(80).collect::<String>()))]
    async fn run(
        &self,
        ctx: xai_tool_runtime::ToolCallContext,
        input: ResearchInput,
    ) -> Result<ToolOutput, xai_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;
        let (cwd, backend, parent_session_id, parent_prompt_id) =
            research_resources(&resources).await;
        let report = run_research_flow(
            &input.question,
            input.max_steps.unwrap_or(6).clamp(1, 8),
            input.results_per_step.unwrap_or(5).clamp(1, 10),
            cwd.as_deref(),
            backend.as_ref(),
            parent_session_id,
            parent_prompt_id,
        )
        .await;

        let mut markdown = report.to_markdown();
        if input.export.unwrap_or(false)
            && let Some(root) = cwd.as_deref()
        {
            match export_report(root, &report) {
                Ok((md, json)) => markdown.push_str(&format!(
                    "\n\n_Artifacts saved: `{}` and `{}`_",
                    md.display(),
                    json.display()
                )),
                Err(error) => {
                    markdown.push_str(&format!("\n\n_(artifact export failed: {error})_"))
                }
            }
        }
        if let Some(root) = cwd.as_deref()
            && let Err(error) = persist_report_to_brain_default(root, &report)
        {
            markdown.push_str(&format!("\n\n_(Brain persistence failed: {error})_"));
        }

        Ok(ToolOutput::Text(TextOutput::from(markdown)))
    }
}

async fn research_resources(
    resources: &crate::types::resources::SharedResources,
) -> (
    Option<PathBuf>,
    Option<SubagentBackendResource>,
    String,
    Option<String>,
) {
    let res = resources.lock().await;
    let cwd = res.get::<Cwd>().map(|cwd| cwd.0.clone());
    let backend = res.get::<SubagentBackendResource>().cloned();
    let parent_session_id = res
        .get::<SessionIdResource>()
        .map(|s| s.0.clone())
        .unwrap_or_default();
    let parent_prompt_id = res.get::<CurrentPromptIdResource>().map(|p| p.0.clone());
    (cwd, backend, parent_session_id, parent_prompt_id)
}

#[derive(Debug, Clone)]
struct ResearchReport {
    question: String,
    plan: Vec<String>,
    steps: Vec<ResearchStep>,
    synthesis: String,
    followups: Vec<String>,
}

impl ResearchReport {
    fn successful_steps(&self) -> usize {
        self.steps.iter().filter(|step| step.succeeded()).count()
    }

    fn to_markdown(&self) -> String {
        let mut out = String::from("## Research\n\n");
        out.push_str(&self.synthesis);
        out.push_str("\n\n---\n");
        out.push_str(&format!(
            "_Plan of {} step(s), {} answered:_\n",
            self.plan.len(),
            self.successful_steps()
        ));
        let mut n = 0usize;
        for step in &self.steps {
            match &step.outcome {
                StepOutcome::Found(_) => {
                    n += 1;
                    out.push_str(&format!("- [{n}] ✅ {}\n", step.sub_query));
                    out.push_str(&format!("  - searched: `{}`\n", step.search_query));
                }
                StepOutcome::Failed(reason) => {
                    out.push_str(&format!("- ⚠️ {} — {}\n", step.sub_query, reason));
                }
            }
        }
        if !self.followups.is_empty() {
            out.push_str("\n**Follow-up questions:**\n");
            for followup in &self.followups {
                out.push_str(&format!("- {followup}\n"));
            }
        }
        out
    }

    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "question": self.question,
            "plan": self.plan,
            "synthesis": self.synthesis,
            "followups": self.followups,
            "steps": self.steps.iter().map(|step| serde_json::json!({
                "sub_query": step.sub_query,
                "search_query": step.search_query,
                "ok": step.succeeded(),
                "evidence": step.evidence(),
            })).collect::<Vec<_>>(),
        })
    }
}

async fn run_research_flow(
    question: &str,
    max_steps: usize,
    results_per_step: usize,
    cwd: Option<&Path>,
    backend: Option<&SubagentBackendResource>,
    parent_session_id: String,
    parent_prompt_id: Option<String>,
) -> ResearchReport {
    let question = question.trim().to_owned();
    if question.is_empty() {
        return ResearchReport {
            question,
            plan: Vec::new(),
            steps: Vec::new(),
            synthesis: "Research requires a non-empty question.".to_owned(),
            followups: Vec::new(),
        };
    }

    let fallback_plan = plan_subqueries(&question, max_steps);
    let mut plan = Vec::new();
    let mut steps = Vec::with_capacity(max_steps);
    for idx in 0..max_steps {
        let sub_query = if let Some(backend) = backend {
            planner_next_query(
                backend,
                &question,
                &steps,
                fallback_plan.get(idx).map(String::as_str),
                parent_session_id.clone(),
                parent_prompt_id.clone(),
            )
            .await
            .or_else(|| fallback_plan.get(idx).cloned())
        } else {
            fallback_plan.get(idx).cloned()
        };
        let Some(sub_query) = sub_query else {
            break;
        };
        if plan
            .iter()
            .any(|q: &String| q.eq_ignore_ascii_case(&sub_query))
        {
            break;
        }
        plan.push(sub_query.clone());
        let search_query = reformulate_query(&sub_query);
        let mut sections = Vec::new();
        match crate::implementations::web_search::providers::search(
            &search_query,
            results_per_step,
            None,
        )
        .await
        {
            Ok(result) => sections.push(format!("## Web\n{}", result.content)),
            Err(error) => sections.push(format!("## Web failed\n{error}")),
        }
        if let Some(root) = cwd {
            match local_codebase_search(root, &search_query, results_per_step).await {
                Ok(local) if !local.contains("0 matches") => {
                    sections.push(format!("## Local codebase\n{local}"));
                }
                _ => {}
            }
        }
        let joined = sections.join("\n\n");
        let outcome =
            if joined.trim().is_empty() || joined.trim_start().starts_with("## Web failed") {
                StepOutcome::Failed(joined.trim().to_owned())
            } else {
                StepOutcome::Found(joined)
            };
        steps.push(ResearchStep {
            sub_query,
            search_query,
            outcome,
        });
    }
    if steps.is_empty() {
        for sub_query in fallback_plan.iter().take(max_steps) {
            let search_query = reformulate_query(sub_query);
            let outcome = match crate::implementations::web_search::providers::search(
                &search_query,
                results_per_step,
                None,
            )
            .await
            {
                Ok(result) => StepOutcome::Found(format!("## Web\n{}", result.content)),
                Err(error) => StepOutcome::Failed(error),
            };
            plan.push(sub_query.clone());
            steps.push(ResearchStep {
                sub_query: sub_query.clone(),
                search_query,
                outcome,
            });
        }
    }

    let synthesis = if let Some(backend) = backend {
        synthesize_with_subagent(
            backend,
            &question,
            &steps,
            parent_session_id,
            parent_prompt_id,
        )
        .await
        .unwrap_or_else(|| local_synthesis(&question, &steps))
    } else {
        local_synthesis(&question, &steps)
    };
    let followups = generate_followups(&question, &plan);
    ResearchReport {
        question,
        plan,
        steps,
        synthesis,
        followups,
    }
}

fn plan_subqueries(question: &str, max_steps: usize) -> Vec<String> {
    let base = question.trim();
    let mut out = Vec::new();
    push_unique(&mut out, base.to_owned());
    push_unique(&mut out, format!("{base} latest developments"));
    push_unique(&mut out, format!("{base} how it works"));
    push_unique(&mut out, format!("{base} limitations criticism"));
    push_unique(&mut out, format!("{base} alternatives comparison"));
    out.truncate(max_steps.max(1));
    out
}

fn push_unique(out: &mut Vec<String>, query: String) {
    let query = query.trim();
    if !query.is_empty()
        && !out
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(query))
    {
        out.push(query.to_owned());
    }
}

fn reformulate_query(query: &str) -> String {
    if let Some((backend, body)) =
        crate::implementations::web_search::providers::split_backend_prefix(query)
    {
        let body = reformulate_plain(body);
        return if body.is_empty() {
            query.trim().to_owned()
        } else {
            format!("{}: {body}", backend.prefix())
        };
    }
    reformulate_plain(query)
}

fn reformulate_plain(query: &str) -> String {
    let trimmed = query.trim().trim_end_matches(['?', '.', '!', ' ']);
    let lower = trimmed.to_ascii_lowercase();
    for lead in [
        "can you tell me about",
        "please tell me about",
        "tell me about",
        "please explain",
        "can you explain",
        "what can you tell me about",
        "please find",
        "can you find",
    ] {
        if let Some(rest) = lower.strip_prefix(lead) {
            let cut = trimmed.len() - rest.len();
            return trimmed[cut..]
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
        }
    }
    trimmed.split_whitespace().collect::<Vec<_>>().join(" ")
}

async fn planner_next_query(
    backend: &SubagentBackendResource,
    question: &str,
    steps: &[ResearchStep],
    fallback: Option<&str>,
    parent_session_id: String,
    parent_prompt_id: Option<String>,
) -> Option<String> {
    let evidence = numbered_evidence(steps);
    let fallback_line = fallback
        .map(|q| format!("\nFallback query if no better gap is visible: {q}"))
        .unwrap_or_default();
    let prompt = format!(
        "You are the planning step of a deep-research loop. Reply with ONLY the next concise search query, or DONE if the evidence is enough.\n\n\
         Use key-free backend prefixes when useful: papers:, arxiv:, scholar:, openalex:, crossref:, pubmed:, doaj:, dblp:, unpaywall:, wiki:, ddg:, searxng:, millionshort:, 4get:, uni:.\n\n\
         Question:\n{question}\n\nEvidence so far:\n{evidence}{fallback_line}"
    );
    let result = spawn_research_subagent(
        backend,
        "plan next deep-research query",
        prompt,
        parent_session_id,
        parent_prompt_id,
    )
    .await?;
    let query = clean_planner_query(&result);
    if query.is_empty() || query.eq_ignore_ascii_case("done") {
        None
    } else {
        Some(query)
    }
}

fn clean_planner_query(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("```"))
        .unwrap_or_default()
        .trim_matches(['"', '`'])
        .trim()
        .to_owned()
}

async fn local_codebase_search(
    root: &Path,
    query: &str,
    max_results: usize,
) -> Result<String, String> {
    const STOP: &[&str] = &[
        "the",
        "and",
        "for",
        "with",
        "that",
        "this",
        "what",
        "how",
        "does",
        "latest",
        "developments",
        "works",
        "limitations",
        "criticism",
        "alternatives",
        "comparison",
    ];
    let terms = query
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|word| word.len() >= 3 && !STOP.contains(&word.to_ascii_lowercase().as_str()))
        .take(8)
        .map(regex_escape)
        .collect::<Vec<_>>();
    if terms.is_empty() {
        return Err(format!("no searchable terms in query: {query}"));
    }
    let pattern = terms.join("|");
    let output = tokio::process::Command::new("rg")
        .arg("--no-heading")
        .arg("--line-number")
        .arg("--ignore-case")
        .arg("--max-count")
        .arg("3")
        .arg("--max-columns")
        .arg("200")
        .arg("-e")
        .arg(&pattern)
        .arg(root)
        .output()
        .await
        .map_err(|e| format!("ripgrep unavailable: {e}"))?;
    let text = String::from_utf8_lossy(&output.stdout);
    let root_str = root.to_string_lossy();
    let mut lines = Vec::new();
    for line in text.lines().take(max_results.saturating_mul(3)) {
        let shown = line
            .strip_prefix(root_str.as_ref())
            .map(|s| s.trim_start_matches('/'))
            .unwrap_or(line);
        lines.push(shown.to_owned());
        if lines.len() >= max_results {
            break;
        }
    }
    if lines.is_empty() {
        Ok(format!(
            "Local codebase search for \"{pattern}\" — 0 matches in {root_str}"
        ))
    } else {
        Ok(format!(
            "Local codebase matches for \"{pattern}\" ({} shown):\n{}",
            lines.len(),
            lines.join("\n")
        ))
    }
}

fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if "\\.+*?()|[]{}^$".contains(ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

async fn synthesize_with_subagent(
    backend: &SubagentBackendResource,
    question: &str,
    steps: &[ResearchStep],
    parent_session_id: String,
    parent_prompt_id: Option<String>,
) -> Option<String> {
    if steps.iter().all(|step| !step.succeeded()) {
        return None;
    }
    let evidence = numbered_evidence(steps);
    let prompt = format!(
        "Synthesize this deep-research evidence into a direct cited answer. \
         Use only citation numbers that appear below. If evidence is thin, say so.\n\n\
         Question:\n{question}\n\nEvidence:\n{evidence}"
    );
    spawn_research_subagent(
        backend,
        "synthesize deep research evidence",
        prompt,
        parent_session_id,
        parent_prompt_id,
    )
    .await
    .filter(|text| !text.trim().is_empty())
}

async fn spawn_research_subagent(
    backend: &SubagentBackendResource,
    description: &str,
    prompt: String,
    parent_session_id: String,
    parent_prompt_id: Option<String>,
) -> Option<String> {
    let params = SubagentSpawnParams {
        id: None,
        prompt,
        description: description.to_owned(),
        subagent_type: "deep-research".to_owned(),
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
        surface_completion: false,
        fork_context: false,
        advisor_gate_prevalidated: false,
    };
    let result = spawn_and_await(backend, params).await.ok()?;
    result.success.then(|| result.output.trim().to_owned())
}

fn numbered_evidence(steps: &[ResearchStep]) -> String {
    let mut out = String::new();
    let mut n = 0usize;
    for step in steps.iter().filter(|step| step.succeeded()) {
        n += 1;
        out.push_str(&format!(
            "\n[{n}] query: {}\n{}\n",
            step.sub_query,
            step.evidence().unwrap_or_default()
        ));
    }
    out
}

fn local_synthesis(question: &str, steps: &[ResearchStep]) -> String {
    let mut out = format!("Research summary for: {question}");
    let mut n = 0usize;
    for step in steps.iter().filter(|step| step.succeeded()) {
        n += 1;
        out.push_str(&format!(
            "\n\n[{n}] {}\n{}",
            step.sub_query,
            step.evidence().unwrap_or_default()
        ));
    }
    if n == 0 {
        out.push_str("\n\nNo successful evidence steps were gathered.");
    }
    out
}

fn generate_followups(question: &str, plan: &[String]) -> Vec<String> {
    let base = question.trim().trim_end_matches(['?', '.', '!']);
    let covered = |needle: &str| {
        plan.iter()
            .any(|query| query.to_ascii_lowercase().contains(needle))
    };
    let mut out = Vec::new();
    if !covered("limitation") && !covered("criticism") {
        out.push(format!("What are the limitations or criticisms of {base}?"));
    }
    if !covered("alternative") && !covered("compare") {
        out.push(format!("What are the main alternatives to {base}?"));
    }
    if !covered("latest") && !covered("recent") {
        out.push(format!("What are the latest developments in {base}?"));
    }
    out.truncate(3);
    out
}

fn export_report(root: &Path, report: &ResearchReport) -> std::io::Result<(PathBuf, PathBuf)> {
    let slug = export_slug(&report.question);
    let dir = root.join(".grok").join("research");
    std::fs::create_dir_all(&dir)?;
    let md = dir.join(format!("{slug}.md"));
    let json = dir.join(format!("{slug}.json"));
    std::fs::write(&md, report.to_markdown())?;
    std::fs::write(&json, serde_json::to_vec_pretty(&report.to_json())?)?;
    Ok((md, json))
}

fn persist_report_to_brain_default(
    workspace_scope: &Path,
    report: &ResearchReport,
) -> xai_grok_brain::Result<xai_grok_brain::MemoryPage> {
    persist_report_to_brain_at(
        &xai_grok_brain::default_store_path(),
        workspace_scope,
        report,
    )
}

fn persist_report_to_brain_at(
    store_path: &Path,
    workspace_scope: &Path,
    report: &ResearchReport,
) -> xai_grok_brain::Result<xai_grok_brain::MemoryPage> {
    use xai_grok_brain::{MemoryCategory, MemorySourceType, NewPage};

    let service = xai_grok_brain::BrainService::open(store_path)?;
    if !service.store().settings_initialized()? {
        service.update_settings(xai_grok_brain::BrainSettingsUpdate {
            enabled: Some(true),
            ..Default::default()
        })?;
    }
    let title = format!(
        "Research: {}",
        report.question.chars().take(120).collect::<String>()
    );
    let body = format!(
        "Question: {}\n\nSynthesis:\n{}\n\nPlan:\n{}\n\nSuccessful evidence steps: {}/{}",
        report.question,
        report.synthesis,
        report
            .plan
            .iter()
            .map(|item| format!("- {item}"))
            .collect::<Vec<_>>()
            .join("\n"),
        report.successful_steps(),
        report.plan.len()
    );
    let scope = workspace_scope.display().to_string();
    let page = service.store().create_or_update_page_by_title_scoped(
        NewPage {
            title: Some(title),
            memory_text: body,
            category: MemoryCategory::Concepts,
            source: Some("research_tool".to_owned()),
        },
        Some(&scope),
    )?;
    service.store().add_source_if_missing(
        page.id,
        MemorySourceType::Manual,
        &format!("Grok Build research: {}", report.question),
        Some(&format!("research:{}", export_slug(&report.question))),
        None,
    )?;
    Ok(page)
}

fn export_slug(question: &str) -> String {
    let mut slug = String::new();
    let mut dash = false;
    for ch in question.chars().take(80) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            dash = false;
        } else if !dash {
            slug.push('-');
            dash = true;
        }
    }
    let slug = slug.trim_matches('-').to_owned();
    if slug.is_empty() {
        "research".to_owned()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_and_reformulation_are_boring_and_deduped() {
        let plan = plan_subqueries("Can you explain Rust async?", 4);
        assert_eq!(plan.len(), 4);
        assert_eq!(
            reformulate_query("arxiv: can you explain graph attention?"),
            "arxiv: graph attention"
        );
    }

    #[test]
    fn markdown_has_citation_anchors() {
        let report = ResearchReport {
            question: "Q".into(),
            plan: vec!["Q".into()],
            steps: vec![ResearchStep {
                sub_query: "Q".into(),
                search_query: "Q".into(),
                outcome: StepOutcome::Found("Evidence".into()),
            }],
            synthesis: "Answer [1]".into(),
            followups: vec![],
        };
        assert!(report.to_markdown().contains("- [1] ✅ Q"));
        assert_eq!(report.to_json()["steps"][0]["ok"], true);
    }

    #[test]
    fn research_report_persists_to_brain_scope() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("brain.sqlite");
        let workspace = dir.path().join("repo");
        std::fs::create_dir_all(&workspace).unwrap();
        let report = ResearchReport {
            question: "Q".into(),
            plan: vec!["Q".into()],
            steps: vec![ResearchStep {
                sub_query: "Q".into(),
                search_query: "Q".into(),
                outcome: StepOutcome::Found("Evidence".into()),
            }],
            synthesis: "Answer [1]".into(),
            followups: vec![],
        };

        let page = persist_report_to_brain_at(&db, &workspace, &report).unwrap();

        assert_eq!(page.category, xai_grok_brain::MemoryCategory::Concepts);
        assert_eq!(page.scope_id.as_deref(), Some(workspace.to_str().unwrap()));
        let sources = xai_grok_brain::BrainService::open(&db)
            .unwrap()
            .sources(page.id)
            .unwrap();
        assert_eq!(sources.len(), 1);
    }
}
