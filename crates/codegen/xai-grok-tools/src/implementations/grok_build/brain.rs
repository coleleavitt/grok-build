//! Brain memory tools.
//!
//! These are read-only counterparts to the `/brain` slash command. They expose
//! the populated SQLite Brain store directly to the model, while the older
//! `memory_search` / `memory_get` tools continue to target Markdown/indexed
//! memory files.

use crate::types::output::{TextOutput, ToolOutput};
use crate::types::resources::Cwd;
use crate::types::tool::{ToolKind, ToolNamespace};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct BrainSearchInput {
    #[schemars(description = "Natural-language query to rank Brain memories against.")]
    pub query: String,
    #[serde(default)]
    #[schemars(description = "Maximum Brain pages to return. Defaults to 8, capped at 20.")]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct BrainGetInput {
    #[serde(default)]
    #[schemars(description = "Brain memory page id. Preferred when known.")]
    pub id: Option<i64>,
    #[serde(default)]
    #[schemars(description = "Brain memory title to read when id is not supplied.")]
    pub title: Option<String>,
}

#[derive(Debug, Default)]
pub struct BrainSearchTool;

impl crate::types::tool_metadata::ToolMetadata for BrainSearchTool {
    fn kind(&self) -> ToolKind {
        ToolKind::MemorySearch
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::GrokBuild
    }

    fn description_template(&self) -> &str {
        "Search the durable Grok Brain SQLite memory graph for relevant long-term pages. Use this when you need remembered user preferences, project state, workstreams, or source-cited Brain memories."
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

impl xai_tool_runtime::Tool for BrainSearchTool {
    type Args = BrainSearchInput;
    type Output = ToolOutput;

    fn id(&self) -> xai_tool_protocol::ToolId {
        xai_tool_protocol::ToolId::new("brain_search").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &xai_tool_runtime::ListToolsContext,
    ) -> xai_tool_types::ToolDescription {
        xai_tool_types::ToolDescription::new(
            "brain_search",
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

    async fn run(
        &self,
        ctx: xai_tool_runtime::ToolCallContext,
        input: BrainSearchInput,
    ) -> Result<ToolOutput, xai_tool_runtime::ToolError> {
        let service = open_brain_service("brain_search")?;
        let workspace_scope = cwd_from_context(&ctx).await;
        let limit = input.limit.unwrap_or(8).clamp(1, 20);
        drop(service);
        let pages = match try_nomic_semantic_search(&input.query, workspace_scope.as_deref(), limit)
            .await
        {
            Ok(Some(pages)) => pages,
            Ok(None) => lexical_brain_search(&input.query, workspace_scope.clone(), limit)?,
            Err(err) => {
                tracing::warn!(error = %err, "brain_search Nomic embeddings failed; falling back to lexical Brain recall");
                lexical_brain_search(&input.query, workspace_scope.clone(), limit)?
            }
        };
        if pages.is_empty() {
            return Ok(ToolOutput::Text(TextOutput::from(
                "No Brain memories found for query.",
            )));
        }
        let mut out = format!("Found {} Brain memory result(s):", pages.len());
        for recalled in pages {
            let page = recalled.page;
            let freshness = match page.freshness {
                xai_grok_brain::MemoryFreshness::Durable => "",
                xai_grok_brain::MemoryFreshness::TimeSensitive => {
                    " — time-sensitive, verify current state"
                }
            };
            out.push_str(&format!(
                "\n\n### #{} [{}] {}{} (score: {})\n{}",
                page.id,
                page.category.as_str(),
                page.title,
                freshness,
                recalled.score,
                page.memory_text.trim()
            ));
            if !recalled.source_labels.is_empty() {
                out.push_str(&format!(
                    "\nSources: {}",
                    recalled
                        .source_labels
                        .iter()
                        .take(3)
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        Ok(ToolOutput::Text(TextOutput::from(out)))
    }
}

#[derive(Debug, Default)]
pub struct BrainGetTool;

impl crate::types::tool_metadata::ToolMetadata for BrainGetTool {
    fn kind(&self) -> ToolKind {
        ToolKind::MemoryGet
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::GrokBuild
    }

    fn description_template(&self) -> &str {
        "Read a durable Grok Brain memory page by id or exact title, including freshness, scope, sources, related pages, and revision count."
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

impl xai_tool_runtime::Tool for BrainGetTool {
    type Args = BrainGetInput;
    type Output = ToolOutput;

    fn id(&self) -> xai_tool_protocol::ToolId {
        xai_tool_protocol::ToolId::new("brain_get").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &xai_tool_runtime::ListToolsContext,
    ) -> xai_tool_types::ToolDescription {
        xai_tool_types::ToolDescription::new(
            "brain_get",
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

    async fn run(
        &self,
        ctx: xai_tool_runtime::ToolCallContext,
        input: BrainGetInput,
    ) -> Result<ToolOutput, xai_tool_runtime::ToolError> {
        let service = open_brain_service("brain_get")?;
        let workspace_scope = cwd_from_context(&ctx).await;
        let page = match (input.id, input.title.as_deref()) {
            (Some(id), _) => service
                .store()
                .get_page(id)
                .map_err(|err| tool_error("brain_get", err))?,
            (None, Some(title)) => service
                .store()
                .get_page_by_title_scoped(title, workspace_scope.as_deref(), false)
                .map_err(|err| tool_error("brain_get", err))?,
            (None, None) => {
                return Ok(ToolOutput::Text(TextOutput::from(
                    "Usage: brain_get requires either {\"id\": ...} or {\"title\": ...}.",
                )));
            }
        };
        let Some(page) = page else {
            return Ok(ToolOutput::Text(TextOutput::from(
                "Brain memory not found.",
            )));
        };
        let sources = service
            .sources(page.id)
            .map_err(|err| tool_error("brain_get", err))?;
        let related = service
            .store()
            .related_page_ids(page.id)
            .map_err(|err| tool_error("brain_get", err))?;
        let revisions = service
            .revisions(page.id)
            .map_err(|err| tool_error("brain_get", err))?;
        let freshness = match page.freshness {
            xai_grok_brain::MemoryFreshness::Durable => "durable",
            xai_grok_brain::MemoryFreshness::TimeSensitive => "time_sensitive",
        };
        let mut out = format!(
            "#{} [{}] {}\nfreshness: {}\nscope: {}{}\nupdated: {}\nsources: {} | related: {} | revisions: {}\n\n{}",
            page.id,
            page.category.as_str(),
            page.title,
            freshness,
            page.scope_kind.as_str(),
            page.scope_id
                .as_deref()
                .map(|scope| format!(":{scope}"))
                .unwrap_or_default(),
            page.updated_at.to_rfc3339(),
            sources.len(),
            related.len(),
            revisions.len(),
            page.memory_text
        );
        if !sources.is_empty() {
            out.push_str("\n\nSources:");
            for source in sources.iter().take(10) {
                out.push_str(&format!(
                    "\n- [{}] {}{}",
                    source.source_type.as_str(),
                    source.label,
                    source
                        .url
                        .as_deref()
                        .map(|url| format!(" <{url}>"))
                        .unwrap_or_default()
                ));
            }
        }
        Ok(ToolOutput::Text(TextOutput::from(out)))
    }
}

fn lexical_brain_search(
    query: &str,
    workspace_scope: Option<String>,
    limit: usize,
) -> Result<Vec<xai_grok_brain::RecalledMemoryPage>, xai_tool_runtime::ToolError> {
    let service = open_brain_service("brain_search")?;
    service
        .store()
        .recall_pages(xai_grok_brain::RecallOptions {
            query: query.to_owned(),
            workspace_scope,
            limit,
        })
        .map_err(|err| tool_error("brain_search", err))
}

async fn try_nomic_semantic_search(
    query: &str,
    workspace_scope: Option<&str>,
    limit: usize,
) -> anyhow::Result<Option<Vec<xai_grok_brain::RecalledMemoryPage>>> {
    let Some(api_key) = std::env::var("NOMIC_API_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty())
    else {
        return Ok(None);
    };
    let pages_and_sources = {
        let service = xai_grok_brain::BrainService::open_grok_default()?;
        service
            .list_pages(workspace_scope)?
            .into_iter()
            .map(|page| {
                let sources = service
                    .sources(page.id)?
                    .into_iter()
                    .map(|source| source.label)
                    .filter(|label| !label.trim().is_empty())
                    .collect::<Vec<_>>();
                Ok::<_, xai_grok_brain::BrainError>((page, sources))
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    if pages_and_sources.is_empty() {
        return Ok(Some(Vec::new()));
    }

    let mut inputs = Vec::with_capacity(pages_and_sources.len() + 1);
    inputs.push(query.to_owned());
    inputs.extend(pages_and_sources.iter().map(|(page, _)| {
        format!(
            "{}\ncategory: {}\nfreshness: {}\n{}",
            page.title,
            page.category.as_str(),
            page.freshness.as_str(),
            page.memory_text
        )
    }));
    let input_refs = inputs.iter().map(String::as_str).collect::<Vec<_>>();
    let embeddings = nomic_embed(&api_key, &input_refs).await?;
    if embeddings.len() != inputs.len() || embeddings[0].is_empty() {
        anyhow::bail!("unexpected Nomic embedding count/dimensions");
    }
    let query_vec = &embeddings[0];
    let mut scored = Vec::new();
    for (idx, (page, source_labels)) in pages_and_sources.into_iter().enumerate() {
        let semantic = cosine_similarity(query_vec, &embeddings[idx + 1]);
        scored.push(xai_grok_brain::RecalledMemoryPage {
            page,
            score: (semantic * 10_000.0).round() as i64,
            source_labels,
        });
    }
    scored.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| b.page.updated_at.cmp(&a.page.updated_at))
            .then_with(|| b.page.id.cmp(&a.page.id))
    });
    scored.truncate(limit);
    Ok(Some(scored))
}

async fn nomic_embed(api_key: &str, inputs: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
    let base =
        std::env::var("NOMIC_API_BASE").unwrap_or_else(|_| "https://api.nomic.ai/v1".to_owned());
    let model =
        std::env::var("NOMIC_EMBED_MODEL").unwrap_or_else(|_| "nomic-embed-text-v1.5".to_owned());
    let dimensions = std::env::var("NOMIC_EMBED_DIMENSIONS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(768);
    let body = serde_json::json!({
        "model": model,
        "input": inputs,
        "dimensions": dimensions,
    });
    let response = reqwest::Client::new()
        .post(format!("{}/embeddings", base.trim_end_matches('/')))
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("Nomic embeddings API error {status}: {body}");
    }
    let json: serde_json::Value = response.json().await?;
    let data = json
        .get("data")
        .and_then(|value| value.as_array())
        .ok_or_else(|| anyhow::anyhow!("Nomic response missing data array"))?;
    let mut out = Vec::with_capacity(data.len());
    for item in data {
        let embedding = item
            .get("embedding")
            .and_then(|value| value.as_array())
            .ok_or_else(|| anyhow::anyhow!("Nomic response item missing embedding"))?
            .iter()
            .map(|value| {
                value
                    .as_f64()
                    .map(|num| num as f32)
                    .ok_or_else(|| anyhow::anyhow!("non-numeric embedding component"))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        out.push(embedding);
    }
    Ok(out)
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f64;
    let mut an = 0.0f64;
    let mut bn = 0.0f64;
    for (&x, &y) in a.iter().zip(b) {
        let x = x as f64;
        let y = y as f64;
        dot += x * y;
        an += x * x;
        bn += y * y;
    }
    if an == 0.0 || bn == 0.0 {
        0.0
    } else {
        dot / (an.sqrt() * bn.sqrt())
    }
}

fn open_brain_service(
    tool_id: &str,
) -> Result<xai_grok_brain::BrainService, xai_tool_runtime::ToolError> {
    xai_grok_brain::BrainService::open_grok_default().map_err(|err| tool_error(tool_id, err))
}

fn tool_error(tool_id: &str, err: impl std::fmt::Display) -> xai_tool_runtime::ToolError {
    xai_tool_runtime::ToolError::execution(
        xai_tool_protocol::ToolId::new(tool_id).expect("valid tool id"),
        err.to_string(),
    )
}

async fn cwd_from_context(ctx: &xai_tool_runtime::ToolCallContext) -> Option<String> {
    use crate::types::tool_metadata::shared_resources;
    let resources = shared_resources(ctx).ok()?;
    resources
        .lock()
        .await
        .get::<Cwd>()
        .map(|cwd| cwd.0.to_string_lossy().into_owned())
}
