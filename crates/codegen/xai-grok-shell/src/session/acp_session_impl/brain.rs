//! Brain integration for the real session prompt path.
//!
//! The heavy lifting lives in `xai-grok-brain`; this module is the thin shell
//! seam that supplies session/prompt ids and hands the returned context block to
//! `ChatStateActor::build_request` as a normal memory reminder.

use std::future::Future;
use std::pin::Pin;

use anyhow::Context as _;
use serde::Deserialize;

use super::*;

const BRAIN_BACKFILL_MIN_INTERVAL: chrono::Duration = chrono::Duration::hours(24);
const BRAIN_EXTRACT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

type BrainRunResult = anyhow::Result<Option<(usize, xai_grok_brain::engine::RunOutcome)>>;
type BrainRunFuture<'a> = Pin<Box<dyn Future<Output = BrainRunResult> + 'a>>;

const BRAIN_EXTRACT_PROMPT: &str = r#"You maintain a user's long-term "Brain": a small, curated graph of durable memory pages derived from their work. Read the recent source material and produce the memory pages worth keeping for future tasks.

Rules:
- Only keep durable, reusable facts, decisions, preferences, entities, and ongoing initiatives. Ignore one-off chatter and transient details.
- Each page has a category:
  - "entities": people, organizations, products, or systems (e.g. "Acme Corp").
  - "concepts": reusable ideas, artifacts, or definitions (e.g. "Brand Kit").
  - "workstreams": ongoing initiatives or projects (e.g. "Product Launches").
  - "notes": simple durable details that do not fit the above.
- Prefer updating an existing page (reuse its exact title) over creating a near-duplicate. Existing page titles are listed below.
- "related" lists the titles of other pages (from this response or the existing list) that this page is meaningfully connected to.
- "sources" lists the source ids (the bracketed [S#]/[D#] tags below) that this specific page was actually derived from. Only cite sources you used.
{focus}
Existing pages (titles):
{existing_titles}

Source material:
{transcript}

Respond with ONLY a JSON object of the form:
{"pages": [{"title": "...", "category": "entities|concepts|workstreams|notes", "content": "one short paragraph", "related": ["Other Title", ...], "sources": ["S1", "D2", ...]}]}
Return at most {max_pages} pages. If nothing is worth keeping, return {"pages": []}.
"#;

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
        workspace_scope: None,
    })
}

/// Choose the memory reminder for prompt injection.
///
/// Brain is canonical; legacy markdown/index memory is fallback-only when
/// Brain has no recalled context.
pub(crate) fn combine_memory_reminders(
    existing: Option<String>,
    brain: Option<String>,
) -> Option<String> {
    // Brain is the canonical memory product. Legacy Markdown/index memory is
    // retained as a fallback when Brain has no recalled context, but we avoid
    // injecting competing `<memory-context>` and `<brain_context>` blocks into
    // the same model request.
    brain.or(existing)
}

fn should_run_brain_backfill(settings: &xai_grok_brain::BrainSettings) -> bool {
    if !settings.enabled {
        return false;
    }
    if std::env::var_os("GROK_BRAIN_BACKFILL_EVERY_TURN").is_some() {
        return true;
    }
    let Some(last_run_at) = settings.last_run_at else {
        return true;
    };
    chrono::Utc::now() - last_run_at >= BRAIN_BACKFILL_MIN_INTERVAL
}

fn brain_extraction_prompt(input: &xai_grok_brain::engine::ExtractionInput) -> String {
    let focus = input
        .focus_instructions
        .as_deref()
        .filter(|focus| !focus.trim().is_empty())
        .map(|focus| format!("- User focus for this run: {}\n", focus.trim()))
        .unwrap_or_default();
    let existing_titles = if input.existing_titles.is_empty() {
        "(none yet)".to_owned()
    } else {
        input
            .existing_titles
            .iter()
            .map(|title| format!("- {title}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    BRAIN_EXTRACT_PROMPT
        .replace("{focus}", &focus)
        .replace("{existing_titles}", &existing_titles)
        .replace("{transcript}", &input.transcript)
        .replace("{max_pages}", &input.max_pages.to_string())
}

fn record_current_git_state(
    service: &xai_grok_brain::BrainService,
    cwd: &str,
) -> anyhow::Result<()> {
    let Some(root) = git_output(cwd, &["rev-parse", "--show-toplevel"]) else {
        return Ok(());
    };
    let root = root.trim();
    if root.is_empty() {
        return Ok(());
    }
    let repo_name = std::path::Path::new(root)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "workspace".to_owned());
    let branch =
        git_output(root, &["branch", "--show-current"]).unwrap_or_else(|| "detached".to_owned());
    let head = git_output(root, &["rev-parse", "--short", "HEAD"]).unwrap_or_default();
    let subject = git_output(root, &["log", "-1", "--format=%s"]).unwrap_or_default();
    let tracking = git_output(
        root,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    )
    .unwrap_or_else(|| "<none>".to_owned());
    let status = git_output(root, &["status", "--short", "--branch"]).unwrap_or_default();
    let remotes = git_output(root, &["remote", "-v"]).unwrap_or_default();

    let status_lines = status
        .lines()
        .take(12)
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n  ");
    let remote_lines = remotes
        .lines()
        .filter(|line| line.contains("(fetch)"))
        .take(8)
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n  ");

    let content = format!(
        "Current git state for `{repo_name}`.\n\n\
         - Workspace: `{root}`\n\
         - Branch: `{branch}`\n\
         - HEAD: `{head}` {subject}\n\
         - Tracking: `{tracking}`\n\
         - Status:\n  {status_lines}\n\
         - Remotes:\n  {remote_lines}",
        branch = branch.trim(),
        head = head.trim(),
        subject = subject.trim(),
        tracking = tracking.trim(),
    );
    service.record_current_state(xai_grok_brain::CurrentStateMemory {
        title: format!("{repo_name} Current Git State"),
        content,
        workspace_scope: Some(root.to_owned()),
        source_label: format!("Current git state for {repo_name}"),
        source_url: Some(format!("file://{root}")),
    })?;
    Ok(())
}

fn git_output(cwd: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn brain_extraction_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "pages": {
                "type": "array",
                // No `maxItems`: Anthropic's structured-output validator rejects
                // that keyword for arrays with a 400. The page cap is enforced by
                // the run prompt (`{max_pages}`) and hard-enforced when applying
                // pages (`BRAIN_MAX_PAGES_PER_RUN`).
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "title": { "type": "string" },
                        "category": {
                            "type": "string",
                            "enum": ["entities", "concepts", "workstreams", "notes"]
                        },
                        "content": { "type": "string" },
                        "related": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "sources": {
                            "type": "array",
                            "items": { "type": "string" }
                        }
                    },
                    "required": ["title", "category", "content", "related", "sources"]
                }
            }
        },
        "required": ["pages"]
    })
}

#[derive(Debug, Deserialize)]
struct BrainExtractionResponse {
    pages: Vec<BrainExtractionPage>,
}

#[derive(Debug, Deserialize)]
struct BrainExtractionPage {
    title: String,
    category: String,
    content: String,
    #[serde(default)]
    related: Vec<String>,
    #[serde(default)]
    sources: Vec<String>,
}

impl From<BrainExtractionPage> for xai_grok_brain::engine::ExtractedPage {
    fn from(page: BrainExtractionPage) -> Self {
        Self {
            title: page.title.trim().to_owned(),
            category: page.category.trim().to_owned(),
            content: page.content.trim().to_owned(),
            related: page
                .related
                .into_iter()
                .map(|item| item.trim().to_owned())
                .filter(|item| !item.is_empty())
                .collect(),
            sources: page
                .sources
                .into_iter()
                .map(|item| item.trim().to_owned())
                .filter(|item| !item.is_empty())
                .collect(),
        }
    }
}

fn parse_brain_extraction_response(
    content: &str,
) -> anyhow::Result<Vec<xai_grok_brain::engine::ExtractedPage>> {
    let trimmed = content.trim();
    let parsed = match serde_json::from_str::<BrainExtractionResponse>(trimmed) {
        Ok(parsed) => parsed,
        Err(_) => {
            let start = trimmed
                .find('{')
                .context("brain extraction response did not contain a JSON object")?;
            let end = trimmed
                .rfind('}')
                .context("brain extraction response did not contain a complete JSON object")?;
            serde_json::from_str::<BrainExtractionResponse>(&trimmed[start..=end])?
        }
    };
    Ok(parsed
        .pages
        .into_iter()
        .take(xai_grok_brain::engine::BRAIN_MAX_PAGES_PER_RUN)
        .map(Into::into)
        .filter(|page: &xai_grok_brain::engine::ExtractedPage| {
            !page.title.is_empty() && !page.content.is_empty()
        })
        .collect())
}

impl SessionActor {
    /// Production prompt-path hook: open the stable Grok Brain store, run the
    /// daily self-improvement backfill when due, and process this real user
    /// prompt. Failures are logged and fail-open so Brain cannot break normal
    /// inference.
    pub(super) async fn process_brain_request_for_prompt(
        &self,
        prompt_id: &str,
        user_text: &str,
    ) -> Option<String> {
        match self
            .process_brain_request_for_prompt_inner(prompt_id, user_text)
            .await
        {
            Ok(outcome) => {
                if let Some(page) = &outcome.remembered_page {
                    self.send_slash_command_output(&format_brain_remembered_feedback(page))
                        .await;
                }
                outcome.injected_context
            }
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

    async fn process_brain_request_for_prompt_inner(
        &self,
        prompt_id: &str,
        user_text: &str,
    ) -> anyhow::Result<xai_grok_brain::BrainRequestOutcome> {
        let service = xai_grok_brain::BrainService::open(&xai_grok_brain::default_store_path())?;
        if !service.store().settings_initialized()? {
            service.update_settings(xai_grok_brain::BrainSettingsUpdate {
                enabled: Some(true),
                ..Default::default()
            })?;
        }
        if let Err(err) = record_current_git_state(&service, self.session_info.cwd.as_str()) {
            tracing::debug!(
                target: xai_grok_telemetry::memory_log::TARGET,
                error = %err,
                "BRAIN_CURRENT_STATE: skipped git state capture"
            );
        }
        self.run_brain_backfill_if_due(&service).await;
        Ok(service.process_request(xai_grok_brain::BrainRequest {
            session_id: self.session_info.id.0.as_ref(),
            prompt_id,
            user_text,
            workspace_scope: Some(self.session_info.cwd.as_str()),
        })?)
    }

    async fn run_brain_backfill_if_due(&self, service: &xai_grok_brain::BrainService) {
        let result = self.run_brain_backfill(service, false).await;

        match result {
            Ok(Some((sessions, outcome))) => {
                tracing::info!(
                    target: xai_grok_telemetry::memory_log::TARGET,
                    sessions,
                    outcome = ?outcome,
                    "BRAIN_BACKFILL: completed self-improvement run"
                );
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(
                    target: xai_grok_telemetry::memory_log::TARGET,
                    error = %err,
                    "BRAIN_BACKFILL: skipped self-improvement run"
                );
            }
        }
    }

    async fn run_brain_backfill(
        &self,
        service: &xai_grok_brain::BrainService,
        force: bool,
    ) -> anyhow::Result<Option<(usize, xai_grok_brain::engine::RunOutcome)>> {
        let settings = service.settings()?;
        if !force && !should_run_brain_backfill(&settings) {
            return Ok(None);
        }
        let selection = xai_grok_brain::read_bounded_run_context(
            &crate::util::grok_home::grok_home(),
            &settings,
            chrono::Utc::now(),
        )?;
        let prepared = match xai_grok_brain::engine::prepare_self_improvement(
            service.store(),
            &selection.context,
        )? {
            xai_grok_brain::engine::PreparedRunOutcome::Disabled => return Ok(None),
            xai_grok_brain::engine::PreparedRunOutcome::NoPages => {
                return Ok(Some((
                    selection.sessions.len(),
                    xai_grok_brain::engine::RunOutcome::NoPages,
                )));
            }
            xai_grok_brain::engine::PreparedRunOutcome::Ready(prepared) => prepared,
        };
        let pages = self.extract_brain_pages(prepared.input()).await?;
        let outcome =
            xai_grok_brain::engine::complete_self_improvement(service.store(), prepared, &pages)?;
        Ok(Some((selection.sessions.len(), outcome)))
    }

    pub(super) async fn execute_brain_slash_command(
        self: &Arc<Self>,
        args: String,
    ) -> PromptTurnResult {
        let service = match xai_grok_brain::BrainService::open_grok_default() {
            Ok(service) => service,
            Err(err) => {
                self.send_slash_command_output(&format!("Brain error: {err}"))
                    .await;
                return ok_end_turn(0, None);
            }
        };
        let workspace = self.session_info.cwd.as_str();
        let text =
            execute_brain_slash_command_text(&service, &args, Some(workspace), |service, force| {
                Box::pin(async move { self.run_brain_backfill(service, force).await })
            })
            .await;
        self.send_slash_command_output(&text).await;
        ok_end_turn(0, None)
    }

    async fn extract_brain_pages(
        &self,
        input: &xai_grok_brain::engine::ExtractionInput,
    ) -> anyhow::Result<Vec<xai_grok_brain::engine::ExtractedPage>> {
        let result = async {
            let sampling_client = self.prepare_chat_completion(false).await?;
            let model = self
                .chat_state_handle
                .get_sampling_config()
                .await
                .map(|config| config.model)
                .unwrap_or_default();
            let session_id = self.session_info.id.to_string();
            let request = ConversationRequest {
                items: vec![ConversationItem::user(brain_extraction_prompt(input))],
                tools: vec![],
                hosted_tools: vec![],
                tool_choice: None,
                model: Some(model),
                // Leave temperature unset: newer Anthropic models reject an
                // explicit `temperature` ("deprecated for this model") with a
                // 400, which would silently turn every backfill into an empty
                // run. Determinism here comes from the schema + prompt.
                temperature: None,
                max_output_tokens: Some(2048),
                json_schema: Some(brain_extraction_schema()),
                reasoning_effort: Some(xai_grok_sampling_types::ReasoningEffort::None),
                x_grok_conv_id: Some(session_id.clone()),
                x_grok_req_id: Some(format!("xai-brain-backfill-{}", uuid::Uuid::new_v4())),
                x_grok_session_id: Some(session_id),
                x_grok_agent_id: Some(xai_grok_telemetry::id::agent_id()),
                ..ConversationRequest::default()
            };
            let response = tokio::time::timeout(
                BRAIN_EXTRACT_TIMEOUT,
                sampling_client.conversation_collect(request),
            )
            .await
            .context("brain extraction timed out")??;
            parse_brain_extraction_response(&response.assistant_text())
        }
        .await;

        result.map_err(|err| {
            tracing::warn!(
                target: xai_grok_telemetry::memory_log::TARGET,
                error = %err,
                "BRAIN_BACKFILL: extraction failed; run timestamp left unchanged"
            );
            err
        })
    }
}

async fn execute_brain_slash_command_text<'a, RunBackfill>(
    service: &'a xai_grok_brain::BrainService,
    args: &'a str,
    workspace: Option<&'a str>,
    run_backfill: RunBackfill,
) -> String
where
    RunBackfill: FnOnce(&'a xai_grok_brain::BrainService, bool) -> BrainRunFuture<'a>,
{
    let parts = args.split_whitespace().collect::<Vec<_>>();
    let cmd = parts
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    let rest = args
        .trim()
        .strip_prefix(parts.first().copied().unwrap_or(""))
        .unwrap_or("")
        .trim();
    match cmd.as_str() {
        "status" | "" => format_brain_status(service),
        "on" | "enable" => update_brain_setting(service, Some(true), None, None),
        "off" | "disable" => update_brain_setting(service, Some(false), None, None),
        "connectors" => format_brain_connectors(service, rest),
        "focus" => format_brain_focus(service, rest),
        "list" => format_brain_list(service, rest, workspace),
        "show" => format_brain_show(service, rest, workspace),
        "sources" => format_brain_sources(service, rest),
        "related" => format_brain_related(service, rest),
        "graph" => format_brain_graph(service),
        "history" | "revisions" => format_brain_history(service, rest),
        "restore" => format_brain_restore(service, rest),
        "forget" | "delete" => format_brain_forget(service, rest, workspace),
        "run" => format_brain_run_result(run_backfill(service, rest.contains("--force")).await),
        _ => brain_usage(),
    }
}

fn format_brain_run_result(result: BrainRunResult) -> String {
    match result {
        Ok(Some((sessions, outcome))) => {
            format!("Brain run complete: sessions={sessions}, outcome={outcome:?}")
        }
        Ok(None) => {
            "Brain run skipped: disabled or not due. Use `/brain run --force` to force.".to_owned()
        }
        Err(err) => format!("Brain run failed: {err}"),
    }
}

fn brain_usage() -> String {
    "Usage: /brain status | list [category] | show <id|title> | sources <id> | related <id> | graph | run [--force] | forget <id|title> | on|off | connectors on|off | focus set <text>|clear|show | history <id> | restore <revision_id>".to_owned()
}

fn format_brain_remembered_feedback(page: &xai_grok_brain::MemoryPage) -> String {
    format!(
        "Brain updated: remembered #{} [{}] {}.",
        page.id,
        page.category.as_str(),
        page.title
    )
}

fn format_brain_status(service: &xai_grok_brain::BrainService) -> String {
    match service.status() {
        Ok(status) => {
            let categories = status
                .category_counts
                .iter()
                .map(|(category, count)| format!("  {}: {count}", category.as_str()))
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "Brain status\nenabled: {}\nconnectors: {}\nlast_run_at: {}\npages: {} (global {}, workspace {})\nsources: {}\nrelations: {}\nrevisions: {}\ncategories:\n{}",
                status.settings.enabled,
                status.settings.use_connectors,
                status
                    .settings
                    .last_run_at
                    .map(|ts| ts.to_rfc3339())
                    .unwrap_or_else(|| "never".to_owned()),
                status.page_count,
                status.global_count,
                status.workspace_count,
                status.source_count,
                status.relation_count,
                status.revision_count,
                categories,
            )
        }
        Err(err) => format!("Brain status failed: {err}"),
    }
}

fn update_brain_setting(
    service: &xai_grok_brain::BrainService,
    enabled: Option<bool>,
    connectors: Option<bool>,
    focus: Option<Option<String>>,
) -> String {
    match service.update_settings(xai_grok_brain::BrainSettingsUpdate {
        enabled,
        use_connectors: connectors,
        focus_instructions: focus,
    }) {
        Ok(settings) => format!(
            "Brain settings updated: enabled={}, connectors={}, focus={}",
            settings.enabled,
            settings.use_connectors,
            settings
                .focus_instructions
                .as_deref()
                .filter(|focus| !focus.is_empty())
                .unwrap_or("<none>")
        ),
        Err(err) => format!("Brain settings update failed: {err}"),
    }
}

fn format_brain_connectors(service: &xai_grok_brain::BrainService, rest: &str) -> String {
    match rest.trim().to_ascii_lowercase().as_str() {
        "on" | "enable" => update_brain_setting(service, None, Some(true), None),
        "off" | "disable" => update_brain_setting(service, None, Some(false), None),
        _ => "Usage: /brain connectors on|off".to_owned(),
    }
}

fn format_brain_focus(service: &xai_grok_brain::BrainService, rest: &str) -> String {
    let trimmed = rest.trim();
    if trimmed.eq_ignore_ascii_case("clear") {
        return update_brain_setting(service, None, None, Some(None));
    }
    if trimmed.eq_ignore_ascii_case("show") || trimmed.is_empty() {
        return match service.settings() {
            Ok(settings) => format!(
                "Brain focus: {}",
                settings.focus_instructions.as_deref().unwrap_or("<none>")
            ),
            Err(err) => format!("Brain focus failed: {err}"),
        };
    }
    let value = trimmed.strip_prefix("set ").unwrap_or(trimmed).trim();
    if value.is_empty() {
        "Usage: /brain focus set <instructions> | clear | show".to_owned()
    } else {
        update_brain_setting(service, None, None, Some(Some(value.to_owned())))
    }
}

fn parse_category(value: &str) -> Option<xai_grok_brain::MemoryCategory> {
    xai_grok_brain::MemoryCategory::parse(value)
}

fn format_brain_list(
    service: &xai_grok_brain::BrainService,
    rest: &str,
    workspace: Option<&str>,
) -> String {
    let category = rest.split_whitespace().next().and_then(parse_category);
    match service.list_pages(workspace) {
        Ok(mut pages) => {
            if let Some(category) = category {
                pages.retain(|page| page.category == category);
            }
            if pages.is_empty() {
                return "No Brain memories found.".to_owned();
            }
            let mut out = format!("Brain memories ({}):", pages.len());
            for page in pages.iter().take(50) {
                let scope = match page.scope_kind {
                    xai_grok_brain::MemoryScopeKind::Global => "global".to_owned(),
                    xai_grok_brain::MemoryScopeKind::Workspace => {
                        format!("workspace:{}", page.scope_id.as_deref().unwrap_or("?"))
                    }
                };
                out.push_str(&format!(
                    "\n  #{} [{}] {} ({scope})",
                    page.id,
                    page.category.as_str(),
                    page.title
                ));
            }
            out
        }
        Err(err) => format!("Brain list failed: {err}"),
    }
}

fn resolve_page(
    service: &xai_grok_brain::BrainService,
    arg: &str,
    workspace: Option<&str>,
) -> xai_grok_brain::Result<Option<xai_grok_brain::MemoryPage>> {
    if let Ok(id) = arg.trim().parse::<i64>() {
        return service.store().get_page(id);
    }
    service
        .store()
        .get_page_by_title_scoped(arg, workspace, false)
}

fn format_brain_show(
    service: &xai_grok_brain::BrainService,
    rest: &str,
    workspace: Option<&str>,
) -> String {
    if rest.trim().is_empty() {
        return "Usage: /brain show <id|title>".to_owned();
    }
    match resolve_page(service, rest.trim(), workspace) {
        Ok(Some(page)) => {
            let sources = service.sources(page.id).unwrap_or_default();
            let related = service
                .store()
                .related_page_ids(page.id)
                .unwrap_or_default();
            let revisions = service.revisions(page.id).unwrap_or_default();
            format!(
                "#{} [{}] {}\nscope: {}{}\nupdated: {}\nsources: {} | related: {} | revisions: {}\n\n{}",
                page.id,
                page.category.as_str(),
                page.title,
                page.scope_kind.as_str(),
                page.scope_id
                    .as_deref()
                    .map(|s| format!(":{s}"))
                    .unwrap_or_default(),
                page.updated_at.to_rfc3339(),
                sources.len(),
                related.len(),
                revisions.len(),
                page.memory_text
            )
        }
        Ok(None) => "Brain memory not found.".to_owned(),
        Err(err) => format!("Brain show failed: {err}"),
    }
}

fn parse_id_arg(rest: &str, usage: &str) -> Result<i64, String> {
    rest.trim().parse::<i64>().map_err(|_| usage.to_owned())
}

fn format_brain_sources(service: &xai_grok_brain::BrainService, rest: &str) -> String {
    let id = match parse_id_arg(rest, "Usage: /brain sources <id>") {
        Ok(id) => id,
        Err(msg) => return msg,
    };
    match service.sources(id) {
        Ok(sources) if sources.is_empty() => "No sources for this Brain memory.".to_owned(),
        Ok(sources) => {
            let mut out = format!("Sources for memory #{id}:");
            for source in sources {
                out.push_str(&format!(
                    "\n  #{} [{}] {}{}",
                    source.id,
                    source.source_type.as_str(),
                    source.label,
                    source
                        .url
                        .as_deref()
                        .map(|u| format!(" <{u}>"))
                        .unwrap_or_default()
                ));
            }
            out
        }
        Err(err) => format!("Brain sources failed: {err}"),
    }
}

fn format_brain_related(service: &xai_grok_brain::BrainService, rest: &str) -> String {
    let id = match parse_id_arg(rest, "Usage: /brain related <id>") {
        Ok(id) => id,
        Err(msg) => return msg,
    };
    match service.store().related_pages(id) {
        Ok(related) if related.is_empty() => "No related Brain memories.".to_owned(),
        Ok(related) => {
            let mut out = format!("Related memories for #{id}:");
            for (label, pages) in [
                ("notes", related.notes),
                ("concepts", related.concepts),
                ("entities", related.entities),
                ("workstreams", related.workstreams),
            ] {
                if pages.is_empty() {
                    continue;
                }
                out.push_str(&format!("\n{label}:"));
                for page in pages {
                    out.push_str(&format!("\n  #{} {}", page.id, page.title));
                }
            }
            out
        }
        Err(err) => format!("Brain related failed: {err}"),
    }
}

fn format_brain_graph(service: &xai_grok_brain::BrainService) -> String {
    match service.graph() {
        Ok(graph) => {
            let mut out = format!(
                "Brain graph: {} nodes, {} edges",
                graph.nodes.len(),
                graph.edges.len()
            );
            for node in graph.nodes.iter().take(40) {
                out.push_str(&format!(
                    "\n  #{} [{}] {} degree={}",
                    node.id,
                    node.category.as_str(),
                    node.title,
                    node.degree
                ));
            }
            out
        }
        Err(err) => format!("Brain graph failed: {err}"),
    }
}

fn format_brain_history(service: &xai_grok_brain::BrainService, rest: &str) -> String {
    let id = match parse_id_arg(rest, "Usage: /brain history <id>") {
        Ok(id) => id,
        Err(msg) => return msg,
    };
    match service.revisions(id) {
        Ok(revisions) if revisions.is_empty() => "No revisions for this Brain memory.".to_owned(),
        Ok(revisions) => {
            let mut out = format!("Revisions for memory #{id}:");
            for revision in revisions.iter().take(20) {
                out.push_str(&format!(
                    "\n  rev #{} [{}] {} at {} ({})",
                    revision.id,
                    revision.category.as_str(),
                    revision.title,
                    revision.created_at.to_rfc3339(),
                    revision.revision_source
                ));
            }
            out
        }
        Err(err) => format!("Brain history failed: {err}"),
    }
}

fn format_brain_restore(service: &xai_grok_brain::BrainService, rest: &str) -> String {
    let id = match parse_id_arg(rest, "Usage: /brain restore <revision_id>") {
        Ok(id) => id,
        Err(msg) => return msg,
    };
    match service.restore_revision(id) {
        Ok(page) => format!(
            "Restored memory #{} from revision #{id}: {}",
            page.id, page.title
        ),
        Err(err) => format!("Brain restore failed: {err}"),
    }
}

/// Delete a page by id or title. Recalled pages are injected into every
/// request, so a wrong memory has to be removable from the shell.
fn format_brain_forget(
    service: &xai_grok_brain::BrainService,
    rest: &str,
    workspace: Option<&str>,
) -> String {
    let arg = rest.trim();
    if arg.is_empty() {
        return "Usage: /brain forget <id|title>".to_owned();
    }
    let page = match resolve_page(service, arg, workspace) {
        Ok(Some(page)) => page,
        Ok(None) => return format!("Brain memory not found: {arg}"),
        Err(err) => return format!("Brain forget failed: {err}"),
    };
    match service.forget_page(page.id) {
        Ok(true) => format!(
            "Brain forgot #{} [{}] {}.",
            page.id,
            page.category.as_str(),
            page.title
        ),
        Ok(false) => format!("Brain memory not found: {arg}"),
        Err(err) => format!("Brain forget failed: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xai_chat_state::{ChatStateActor, NullChatPersistence};
    use xai_grok_sampling_types::{ConversationItem, SamplingConfig};

    /// The Anthropic structured-output validator rejects array `maxItems` with
    /// a 400, which silently turned every backfill into an empty run. The
    /// schema must stay free of unsupported array keywords; the page cap lives
    /// in the prompt and in `apply_pages`.
    #[test]
    fn extraction_schema_has_no_unsupported_array_keywords() {
        let schema = brain_extraction_schema();
        let mut stack = vec![&schema];
        while let Some(node) = stack.pop() {
            match node {
                serde_json::Value::Object(map) => {
                    for key in ["maxItems", "minItems", "maxLength", "minLength"] {
                        assert!(
                            !map.contains_key(key),
                            "brain extraction schema must not use `{key}`: {schema}"
                        );
                    }
                    stack.extend(map.values());
                }
                serde_json::Value::Array(items) => stack.extend(items.iter()),
                _ => {}
            }
        }
        assert_eq!(schema["properties"]["pages"]["type"], "array");
        assert_eq!(schema["required"][0], "pages");
    }

    /// The run prompt must still carry the page cap, since the schema no longer
    /// encodes it.
    #[test]
    fn extraction_prompt_states_the_page_cap() {
        let input = xai_grok_brain::engine::ExtractionInput {
            transcript: "[S1] User: ship it\n".to_owned(),
            existing_titles: vec![],
            focus_instructions: None,
            max_pages: xai_grok_brain::engine::BRAIN_MAX_PAGES_PER_RUN,
        };
        let prompt = brain_extraction_prompt(&input);
        assert!(!prompt.contains("{max_pages}"), "prompt: {prompt}");
        assert!(
            prompt.contains(&xai_grok_brain::engine::BRAIN_MAX_PAGES_PER_RUN.to_string()),
            "prompt must state the page cap: {prompt}"
        );
    }

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
    fn brain_slash_formatters_drive_store_status_details_graph_and_revisions() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = tmp.path().join("brain.sqlite");
        let service = xai_grok_brain::BrainService::open(&db).unwrap();
        service
            .update_settings(xai_grok_brain::BrainSettingsUpdate {
                enabled: Some(true),
                use_connectors: Some(true),
                focus_instructions: Some(Some("Track Brain commands".to_owned())),
            })
            .unwrap();
        let hub = service
            .store()
            .create_page_scoped(
                xai_grok_brain::NewPage {
                    title: Some("Grok Brain Commands".to_owned()),
                    memory_text:
                        "Brain commands expose status, list, show, sources, graph, and history."
                            .to_owned(),
                    category: xai_grok_brain::MemoryCategory::Workstreams,
                    source: Some("test".to_owned()),
                },
                Some("/tmp"),
            )
            .unwrap();
        let note = service
            .store()
            .create_page(xai_grok_brain::NewPage {
                title: Some("User Source Labels".to_owned()),
                memory_text: "The user wants source labels in recalled context.".to_owned(),
                category: xai_grok_brain::MemoryCategory::Notes,
                source: Some("test".to_owned()),
            })
            .unwrap();
        service.store().add_relation(hub.id, note.id).unwrap();
        service
            .store()
            .add_source(
                hub.id,
                xai_grok_brain::MemorySourceType::ChatSession,
                "ERS API Request Signing",
                Some("session-ers"),
                Some("grok://session/session-ers"),
            )
            .unwrap();
        let updated = service
            .store()
            .update_page(
                hub.id,
                xai_grok_brain::PageUpdate {
                    memory_text: Some("Updated Brain command memory.".to_owned()),
                    ..Default::default()
                },
            )
            .unwrap();

        let status = format_brain_status(&service);
        assert!(status.contains("Brain status"));
        assert!(status.contains("enabled: true"));
        assert!(status.contains("pages: 2"));
        assert!(status.contains("sources: 1"));
        assert!(status.contains("revisions:"));

        let list = format_brain_list(&service, "workstreams", Some("/tmp"));
        assert!(list.contains("Grok Brain Commands"));
        assert!(!list.contains("User Source Labels"));

        let show = format_brain_show(&service, &updated.id.to_string(), Some("/tmp"));
        assert!(show.contains("sources: 1"));
        assert!(show.contains("related: 1"));
        assert!(show.contains("Updated Brain command memory"));

        let sources = format_brain_sources(&service, &hub.id.to_string());
        assert!(sources.contains("ERS API Request Signing"));
        assert!(sources.contains("grok://session/session-ers"));

        let related = format_brain_related(&service, &hub.id.to_string());
        assert!(related.contains("User Source Labels"));

        let graph = format_brain_graph(&service);
        assert!(graph.contains("Brain graph: 2 nodes, 1 edges"));
        assert!(graph.contains("Grok Brain Commands"));

        let history = format_brain_history(&service, &hub.id.to_string());
        assert!(history.contains("Revisions for memory"));
        assert!(history.contains("rev #"));

        let original_revision = service
            .revisions(hub.id)
            .unwrap()
            .into_iter()
            .find(|revision| revision.memory_text.contains("status, list, show"))
            .unwrap();
        let restored = format_brain_restore(&service, &original_revision.id.to_string());
        assert!(restored.contains("Restored memory"));
        let restored_page = service.store().get_page(hub.id).unwrap().unwrap();
        assert!(restored_page.memory_text.contains("status, list, show"));
    }

    /// A junk page has to be removable from the shell: recall injects stored
    /// pages into every request, so a bad memory is load-bearing until deleted.
    #[test]
    fn brain_forget_removes_a_page_by_id_and_by_title() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = tmp.path().join("brain.sqlite");
        let service = xai_grok_brain::BrainService::open(&db).unwrap();
        service
            .update_settings(xai_grok_brain::BrainSettingsUpdate {
                enabled: Some(true),
                ..Default::default()
            })
            .unwrap();
        let junk = service
            .store()
            .create_page(xai_grok_brain::NewPage {
                title: Some("Junk Paste".to_owned()),
                memory_text: "The user asked to remember: a pasted work item.".to_owned(),
                category: xai_grok_brain::MemoryCategory::Notes,
                source: Some("request".to_owned()),
            })
            .unwrap();
        let keeper = service
            .store()
            .create_page(xai_grok_brain::NewPage {
                title: Some("Deploy Script".to_owned()),
                memory_text: "The deploy script lives in bin/deploy.".to_owned(),
                category: xai_grok_brain::MemoryCategory::Workstreams,
                source: Some("request".to_owned()),
            })
            .unwrap();

        assert_eq!(
            format_brain_forget(&service, "", None),
            "Usage: /brain forget <id|title>"
        );
        assert!(
            format_brain_forget(&service, "12345", None).contains("not found"),
            "unknown ids must not report success"
        );

        let by_id = format_brain_forget(&service, &junk.id.to_string(), None);
        assert!(
            by_id.contains(&format!("Brain forgot #{}", junk.id)),
            "{by_id}"
        );
        assert!(service.store().get_page(junk.id).unwrap().is_none());

        let by_title = format_brain_forget(&service, "Deploy Script", None);
        assert!(by_title.contains("Deploy Script"), "{by_title}");
        assert!(service.store().get_page(keeper.id).unwrap().is_none());
        assert!(service.list_pages(None).unwrap().is_empty());
    }

    /// The command must be reachable through the shared slash executor.
    #[tokio::test]
    async fn brain_forget_is_reachable_through_the_slash_executor() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = tmp.path().join("brain.sqlite");
        let service = xai_grok_brain::BrainService::open(&db).unwrap();
        service
            .update_settings(xai_grok_brain::BrainSettingsUpdate {
                enabled: Some(true),
                ..Default::default()
            })
            .unwrap();
        let page = service
            .store()
            .create_page(xai_grok_brain::NewPage {
                title: Some("Stale Fact".to_owned()),
                memory_text: "Stale fact that must be removable.".to_owned(),
                category: xai_grok_brain::MemoryCategory::Notes,
                source: Some("request".to_owned()),
            })
            .unwrap();

        let text = execute_brain_slash_command_text(
            &service,
            &format!("forget {}", page.id),
            None,
            |_, _| Box::pin(async { Ok(None) }),
        )
        .await;
        assert!(text.contains("Brain forgot"), "{text}");
        assert!(service.store().get_page(page.id).unwrap().is_none());
        assert!(
            brain_usage().contains("forget <id|title>"),
            "usage must advertise the command: {}",
            brain_usage()
        );
    }

    fn resolve_brain_args(input: &str) -> String {
        use crate::session::slash_commands::{
            BuiltinAction, CommandAvailability, SkillSlashRewrite, SlashCommandOutcome, resolve,
        };
        use xai_grok_tools::implementations::grok_build::LoopFireMode;
        let blocks = vec![agent_client_protocol::ContentBlock::Text(
            agent_client_protocol::TextContent::new(input.to_owned()),
        )];
        match resolve(
            blocks,
            &[],
            CommandAvailability::all_enabled(),
            SkillSlashRewrite::default(),
            &[],
            LoopFireMode::Detached,
        ) {
            Err(SlashCommandOutcome::Builtin(BuiltinAction::Brain { args })) => args,
            Ok(_) => panic!("expected {input:?} to resolve as /brain"),
            Err(_) => panic!("expected {input:?} to resolve as BuiltinAction::Brain"),
        }
    }

    fn unexpected_brain_run<'a>(
        _: &'a xai_grok_brain::BrainService,
        _: bool,
    ) -> BrainRunFuture<'a> {
        Box::pin(async { panic!("non-run /brain subcommand invoked manual run") })
    }

    fn fixed_force_brain_run<'a>(
        _: &'a xai_grok_brain::BrainService,
        force: bool,
    ) -> BrainRunFuture<'a> {
        Box::pin(async move {
            assert!(force, "/brain run --force should request a forced run");
            Ok(Some((3, xai_grok_brain::engine::RunOutcome::NoPages)))
        })
    }

    async fn execute_resolved_brain(
        service: &xai_grok_brain::BrainService,
        input: &str,
        workspace: Option<&str>,
    ) -> String {
        let args = resolve_brain_args(input);
        execute_brain_slash_command_text(service, &args, workspace, unexpected_brain_run).await
    }

    #[tokio::test]
    async fn brain_slash_resolver_and_shared_executor_drive_seeded_store() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = tmp.path().join("brain.sqlite");
        let service = xai_grok_brain::BrainService::open(&db).unwrap();
        service
            .update_settings(xai_grok_brain::BrainSettingsUpdate {
                enabled: Some(true),
                ..Default::default()
            })
            .unwrap();
        let workspace = Some("/tmp/brain-handler-path");
        let hub = service
            .store()
            .create_page_scoped(
                xai_grok_brain::NewPage {
                    title: Some("Handler Path Brain Command".to_owned()),
                    memory_text: "The shipped /brain handler can inspect seeded durable state."
                        .to_owned(),
                    category: xai_grok_brain::MemoryCategory::Workstreams,
                    source: Some("test".to_owned()),
                },
                workspace,
            )
            .unwrap();
        let note = service
            .store()
            .create_page(xai_grok_brain::NewPage {
                title: Some("Handler Source Label".to_owned()),
                memory_text: "The command output should show attached source labels.".to_owned(),
                category: xai_grok_brain::MemoryCategory::Notes,
                source: Some("test".to_owned()),
            })
            .unwrap();
        service.store().add_relation(hub.id, note.id).unwrap();
        service
            .store()
            .add_source(
                hub.id,
                xai_grok_brain::MemorySourceType::ChatSession,
                "session \"Brain handler path\"",
                Some("handler-session"),
                Some("grok://session/handler-session"),
            )
            .unwrap();

        let status = execute_resolved_brain(&service, "/brain status", workspace).await;
        assert!(status.contains("Brain status"));
        assert!(status.contains("enabled: true"));
        assert!(status.contains("pages: 2"));

        let list = execute_resolved_brain(&service, "/brain list workstreams", workspace).await;
        assert!(list.contains("Handler Path Brain Command"));
        assert!(!list.contains("Handler Source Label"));

        let show =
            execute_resolved_brain(&service, &format!("/brain show {}", hub.id), workspace).await;
        assert!(show.contains("sources: 1"));
        assert!(show.contains("related: 1"));

        let sources =
            execute_resolved_brain(&service, &format!("/brain sources {}", hub.id), workspace)
                .await;
        assert!(sources.contains("session \"Brain handler path\""));
        assert!(sources.contains("grok://session/handler-session"));

        let related =
            execute_resolved_brain(&service, &format!("/brain related {}", hub.id), workspace)
                .await;
        assert!(related.contains("Handler Source Label"));

        let graph = execute_resolved_brain(&service, "/brain graph", workspace).await;
        assert!(graph.contains("Brain graph: 2 nodes, 1 edges"));
        assert!(graph.contains("Handler Path Brain Command"));

        let focus =
            execute_resolved_brain(&service, "/brain focus set Track handler path", workspace)
                .await;
        assert!(focus.contains("Track handler path"));
        let clear = execute_resolved_brain(&service, "/brain focus clear", workspace).await;
        assert!(clear.contains("focus=<none>"));

        let run_args = resolve_brain_args("/brain run --force");
        let run =
            execute_brain_slash_command_text(&service, &run_args, workspace, fixed_force_brain_run)
                .await;
        assert_eq!(run, "Brain run complete: sessions=3, outcome=NoPages");
    }

    #[test]
    fn brain_slash_formatters_control_focus_connectors_and_feedback_text() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = tmp.path().join("brain.sqlite");
        let service = xai_grok_brain::BrainService::open(&db).unwrap();
        service
            .update_settings(xai_grok_brain::BrainSettingsUpdate {
                enabled: Some(true),
                ..Default::default()
            })
            .unwrap();

        let connectors = format_brain_connectors(&service, "on");
        assert!(connectors.contains("enabled=true"));
        assert!(connectors.contains("connectors=true"));

        let focus = format_brain_focus(&service, "set Track ERS deploys");
        assert!(focus.contains("Track ERS deploys"));
        assert!(format_brain_focus(&service, "show").contains("Track ERS deploys"));
        assert!(format_brain_focus(&service, "clear").contains("focus=<none>"));

        let page = service
            .store()
            .create_page(xai_grok_brain::NewPage {
                title: Some("Visible Feedback".to_owned()),
                memory_text: "Brain should tell the user when memory updates.".to_owned(),
                category: xai_grok_brain::MemoryCategory::Notes,
                source: None,
            })
            .unwrap();
        let feedback = format_brain_remembered_feedback(&page);
        assert_eq!(
            feedback,
            format!(
                "Brain updated: remembered #{} [notes] Visible Feedback.",
                page.id
            )
        );
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

    #[test]
    fn backfill_due_when_enabled_and_never_run_or_stale() {
        let mut settings = xai_grok_brain::BrainSettings {
            enabled: true,
            ..Default::default()
        };
        assert!(should_run_brain_backfill(&settings));
        settings.last_run_at = Some(chrono::Utc::now() - chrono::Duration::hours(25));
        assert!(should_run_brain_backfill(&settings));
        settings.last_run_at = Some(chrono::Utc::now());
        assert!(!should_run_brain_backfill(&settings));
        settings.enabled = false;
        settings.last_run_at = None;
        assert!(!should_run_brain_backfill(&settings));
    }

    #[test]
    fn parses_brain_extraction_json_and_filters_empty_pages() {
        let pages = parse_brain_extraction_response(
            r#"```json
            {"pages":[
              {"title":" Project Codename ","category":"entities","content":" Zephyr is durable. ","related":[" Launch "],"sources":[" [s1] "]},
              {"title":"","category":"notes","content":"ignored","related":[],"sources":[]}
            ]}
            ```"#,
        )
        .unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].title, "Project Codename");
        assert_eq!(pages[0].content, "Zephyr is durable.");
        assert_eq!(pages[0].related, vec!["Launch"]);
        assert_eq!(pages[0].sources, vec!["[s1]"]);
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
                query_params: Default::default(),
                env_http_headers: Default::default(),
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
    fn brain_reminder_wins_over_legacy_memory_reminder() {
        let combined = combine_memory_reminders(
            Some("<memory_context>old</memory_context>".to_owned()),
            Some("<brain_context>new</brain_context>".to_owned()),
        )
        .unwrap();
        assert!(!combined.contains("<memory_context>old</memory_context>"));
        assert!(combined.contains("<brain_context>new</brain_context>"));

        let fallback = combine_memory_reminders(
            Some("<memory_context>old</memory_context>".to_owned()),
            None,
        )
        .unwrap();
        assert!(fallback.contains("<memory_context>old</memory_context>"));
    }
}
