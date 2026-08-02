//! `memory_search` tool — new architecture (`Tool` trait).

use super::types::MemorySearchInput;
use crate::types::output::ToolOutput;
use crate::types::tool::{ToolKind, ToolNamespace};

#[derive(Debug, Default)]
pub struct MemorySearchImpl;

impl crate::types::tool_metadata::ToolMetadata for MemorySearchImpl {
    fn kind(&self) -> ToolKind {
        ToolKind::MemorySearch
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::GrokBuild
    }

    fn description_template(&self) -> &str {
        "Search cross-session memory for relevant knowledge chunks. Returns ranked results \
         from global, workspace, and session memory files.\n\n\
         Use this proactively when:\n\
         - A question references prior work, decisions, or context you don't have\n\
         - You need project conventions, coding patterns, or user preferences\n\
         - The user mentions something discussed or decided in a previous session\n\
         - Starting work in an unfamiliar part of the codebase\n\
         - After compaction when prior context may have been lost"
    }
}

impl xai_tool_runtime::Tool for MemorySearchImpl {
    type Args = MemorySearchInput;
    type Output = ToolOutput;

    fn id(&self) -> xai_tool_protocol::ToolId {
        xai_tool_protocol::ToolId::new("memory_search").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::xai_tool_runtime::ListToolsContext,
    ) -> xai_tool_types::ToolDescription {
        xai_tool_types::ToolDescription::new(
            "memory_search",
            crate::types::tool_metadata::ToolMetadata::sanitized_description_template(self),
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
        input: MemorySearchInput,
    ) -> Result<ToolOutput, xai_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let _resources = shared_resources(&ctx)?;
        brain_search_fallback(&ctx, &input.query, input.max_results).await
    }
}

async fn brain_search_fallback(
    ctx: &xai_tool_runtime::ToolCallContext,
    query: &str,
    max_results: Option<usize>,
) -> Result<ToolOutput, xai_tool_runtime::ToolError> {
    let service = xai_grok_brain::BrainService::open_grok_default().map_err(|err| {
        xai_tool_runtime::ToolError::execution(
            xai_tool_protocol::ToolId::new("memory_search").expect("valid"),
            format!("memory backend disabled and Brain unavailable: {err}"),
        )
    })?;
    let workspace_scope = cwd_from_context(ctx).await;
    let pages = service
        .store()
        .recall_pages(xai_grok_brain::RecallOptions {
            query: query.to_owned(),
            workspace_scope,
            limit: max_results.unwrap_or(8).clamp(1, 20),
        })
        .map_err(|err| {
            xai_tool_runtime::ToolError::execution(
                xai_tool_protocol::ToolId::new("memory_search").expect("valid"),
                format!("Brain fallback search failed: {err}"),
            )
        })?;
    if pages.is_empty() {
        return Ok(ToolOutput::Text(
            "No Brain memories found for query.".into(),
        ));
    }
    let mut output = format!(
        "Legacy memory backend is not enabled; searched durable Brain instead. Found {} result(s):\n",
        pages.len()
    );
    for (i, recalled) in pages.iter().enumerate() {
        let page = &recalled.page;
        output.push_str(&format!(
            "\n### Result {} (Brain #{}, score: {}, category: {}) {}\n{}\n",
            i + 1,
            page.id,
            recalled.score,
            page.category.as_str(),
            page.title,
            page.memory_text.trim()
        ));
    }
    Ok(ToolOutput::Text(output.into()))
}

async fn cwd_from_context(ctx: &xai_tool_runtime::ToolCallContext) -> Option<String> {
    use crate::types::resources::Cwd;
    use crate::types::tool_metadata::shared_resources;
    let resources = shared_resources(ctx).ok()?;
    resources
        .lock()
        .await
        .get::<Cwd>()
        .map(|cwd| cwd.0.to_string_lossy().into_owned())
}
