use std::sync::{Arc, Mutex};

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use xai_grok_tools::implementations::grok_build::{BrainGetTool, BrainSearchTool};
use xai_grok_tools::implementations::memory::types::{MemoryGetInput, MemorySearchInput};
use xai_grok_tools::implementations::memory::{MemoryGetImpl, MemorySearchImpl};
use xai_grok_tools::types::memory_backend::{MemoryBackend, MemorySearchResult};
use xai_grok_tools::types::output::ToolOutput;
use xai_grok_tools::types::resources::{Cwd, Resources};
use xai_grok_tools::types::tool_metadata::test_ctx;

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvRestore {
    vars: Vec<(&'static str, Option<String>)>,
}

impl EnvRestore {
    fn set(vars: &[(&'static str, String)]) -> Self {
        let restore = Self {
            vars: vars
                .iter()
                .map(|(key, _)| (*key, std::env::var(key).ok()))
                .collect(),
        };
        for (key, value) in vars {
            // SAFETY: tests hold ENV_LOCK and restore on drop.
            unsafe { std::env::set_var(key, value) };
        }
        restore
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        for (key, prior) in self.vars.drain(..) {
            match prior {
                Some(value) => unsafe { std::env::set_var(key, value) },
                None => unsafe { std::env::remove_var(key) },
            }
        }
    }
}

fn resources(cwd: &std::path::Path) -> Resources {
    let mut resources = Resources::new();
    resources.insert(Cwd(cwd.to_path_buf()));
    resources
}

fn seed_brain(db_path: &std::path::Path, workspace_scope: &str) -> (i64, i64) {
    let service = xai_grok_brain::BrainService::open(db_path).unwrap();
    service
        .update_settings(xai_grok_brain::BrainSettingsUpdate {
            enabled: Some(true),
            ..Default::default()
        })
        .unwrap();
    let branch = service
        .record_current_state(xai_grok_brain::CurrentStateMemory {
            title: "Branch State".to_owned(),
            content: "The custom fork now uses dev for active Grok Build work.".to_owned(),
            workspace_scope: Some(workspace_scope.to_owned()),
            source_label: "branch migration test".to_owned(),
            source_url: Some("grok://session/test-branch".to_owned()),
        })
        .unwrap()
        .unwrap();
    let safety = service
        .store()
        .create_page(xai_grok_brain::NewPage {
            title: Some("Install Safety".to_owned()),
            memory_text: "Avoid curl pipe bash installers.".to_owned(),
            category: xai_grok_brain::MemoryCategory::Notes,
            source: Some("manual".to_owned()),
        })
        .unwrap();
    (branch.id, safety.id)
}

fn text(output: ToolOutput) -> String {
    match output {
        ToolOutput::Text(text) => text.text,
        other => panic!("expected text output, got {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn brain_search_uses_local_embeddings_without_api_key_then_brain_get_reads_page() {
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = tempfile::TempDir::new().unwrap();
    let db_path = tmp.path().join("brain.sqlite");
    let workspace = tmp.path().join("repo");
    std::fs::create_dir_all(&workspace).unwrap();
    let (branch_id, _safety_id) = seed_brain(&db_path, &workspace.to_string_lossy());

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [
                {"embedding": [1.0, 0.0]},
                {"embedding": [1.0, 0.0]},
                {"embedding": [0.0, 1.0]}
            ]
        })))
        .mount(&server)
        .await;

    let _env = EnvRestore::set(&[
        ("GROK_BRAIN_DB", db_path.display().to_string()),
        ("GROK_BRAIN_EMBED_API_KEY", String::new()),
        ("GROK_BRAIN_EMBED_BASE_URL", server.uri()),
        ("GROK_BRAIN_EMBED_MODEL", "mxbai-embed-large".to_owned()),
        ("GROK_BRAIN_EMBED_DIMENSIONS", "2".to_owned()),
        ("NOMIC_API_KEY", String::new()),
        ("NOMIC_API_BASE", String::new()),
        ("NOMIC_EMBED_MODEL", String::new()),
        ("NOMIC_EMBED_DIMENSIONS", String::new()),
    ]);
    let ctx = test_ctx(resources(&workspace).into_shared());
    let result = xai_tool_runtime::Tool::run(
        &BrainSearchTool,
        ctx.clone(),
        xai_grok_tools::implementations::grok_build::brain::BrainSearchInput {
            query: "dev branch state".to_owned(),
            limit: Some(2),
        },
    )
    .await
    .unwrap();
    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        requests.len(),
        1,
        "brain_search must call the local OpenAI-compatible embeddings endpoint exactly once"
    );
    assert!(
        requests[0].headers.get("authorization").is_none(),
        "local embeddings should not require or send an API key"
    );
    let body: serde_json::Value = requests[0].body_json().unwrap();
    assert_eq!(body["model"], "mxbai-embed-large");
    assert_eq!(body["dimensions"], 2);
    let inputs = body["input"].as_array().expect("input array");
    assert_eq!(inputs.len(), 3, "query plus two Brain candidate pages");
    assert_eq!(inputs[0], "dev branch state");
    assert!(inputs[1].as_str().unwrap().contains("Branch State"));
    assert!(inputs[2].as_str().unwrap().contains("Install Safety"));

    let output = text(result);
    assert!(output.contains("[workstreams] Branch State"), "{output}");
    assert!(
        output.find("Branch State").unwrap() < output.find("Install Safety").unwrap(),
        "local semantic ranking should put Branch State first: {output}"
    );
    assert!(output.contains("time-sensitive"), "{output}");

    let result = xai_tool_runtime::Tool::run(
        &BrainGetTool,
        ctx,
        xai_grok_tools::implementations::grok_build::brain::BrainGetInput {
            id: Some(branch_id),
            title: None,
        },
    )
    .await
    .unwrap();
    let output = text(result);
    assert!(output.contains("Branch State"), "{output}");
    assert!(output.contains("freshness: time_sensitive"), "{output}");
    assert!(output.contains("Sources:"), "{output}");
}

#[tokio::test(flavor = "current_thread")]
async fn brain_search_falls_back_to_lexical_when_embedding_response_is_malformed() {
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = tempfile::TempDir::new().unwrap();
    let db_path = tmp.path().join("brain.sqlite");
    let workspace = tmp.path().join("repo");
    std::fs::create_dir_all(&workspace).unwrap();
    seed_brain(&db_path, &workspace.to_string_lossy());

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [
                {"embedding": [1.0, 0.0]}
            ]
        })))
        .mount(&server)
        .await;

    let _env = EnvRestore::set(&[
        ("GROK_BRAIN_DB", db_path.display().to_string()),
        ("GROK_BRAIN_EMBED_API_KEY", String::new()),
        ("GROK_BRAIN_EMBED_BASE_URL", server.uri()),
        ("GROK_BRAIN_EMBED_MODEL", "mxbai-embed-large".to_owned()),
        ("GROK_BRAIN_EMBED_DIMENSIONS", "2".to_owned()),
        ("NOMIC_API_KEY", String::new()),
        ("NOMIC_API_BASE", String::new()),
        ("NOMIC_EMBED_MODEL", String::new()),
        ("NOMIC_EMBED_DIMENSIONS", String::new()),
    ]);
    let ctx = test_ctx(resources(&workspace).into_shared());
    let result = xai_tool_runtime::Tool::run(
        &BrainSearchTool,
        ctx,
        xai_grok_tools::implementations::grok_build::brain::BrainSearchInput {
            query: "dev branch state".to_owned(),
            limit: Some(2),
        },
    )
    .await
    .unwrap();

    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        requests.len(),
        1,
        "brain_search should attempt embeddings before falling back"
    );
    assert!(
        requests[0].headers.get("authorization").is_none(),
        "local embedding fallback should not require or send an API key"
    );

    let output = text(result);
    assert!(
        output.contains("Found 2 Brain memory result(s):"),
        "{output}"
    );
    assert!(output.contains("Branch State"), "{output}");
    assert!(output.contains("Install Safety"), "{output}");
    assert!(
        output.find("Branch State").unwrap() < output.find("Install Safety").unwrap(),
        "lexical fallback should preserve prepared lexical ordering: {output}"
    );
    assert!(!output.contains("No Brain memories found"), "{output}");
}

struct LegacyBackendShouldNotRun;

#[async_trait::async_trait]
impl MemoryBackend for LegacyBackendShouldNotRun {
    async fn search(
        &self,
        _query: &str,
        _max_results: usize,
        _min_score: f64,
    ) -> Result<Vec<MemorySearchResult>, Box<dyn std::error::Error + Send + Sync>> {
        Ok(vec![MemorySearchResult {
            chunk_id: "legacy".to_owned(),
            path: "legacy.md".to_owned(),
            start_line: 0,
            end_line: 1,
            score: 1.0,
            snippet: "LEGACY_BACKEND_SHOULD_NOT_APPEAR".to_owned(),
            source: "workspace".to_owned(),
            created_at: None,
        }])
    }

    fn get(
        &self,
        _path: &str,
        _from: Option<usize>,
        _lines: Option<usize>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok("LEGACY_BACKEND_SHOULD_NOT_APPEAR".to_owned())
    }

    fn total_chunks(&self) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        Ok(1)
    }
}

#[tokio::test(flavor = "current_thread")]
async fn legacy_memory_tools_alias_to_brain_even_when_backend_is_present() {
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = tempfile::TempDir::new().unwrap();
    let db_path = tmp.path().join("brain.sqlite");
    let workspace = tmp.path().join("repo");
    std::fs::create_dir_all(&workspace).unwrap();
    let (branch_id, _) = seed_brain(&db_path, &workspace.to_string_lossy());
    let _env = EnvRestore::set(&[("GROK_BRAIN_DB", db_path.display().to_string())]);

    let mut resources = resources(&workspace);
    resources.insert(Arc::new(LegacyBackendShouldNotRun) as Arc<dyn MemoryBackend>);
    let ctx = test_ctx(resources.into_shared());

    let result = xai_tool_runtime::Tool::run(
        &MemorySearchImpl,
        ctx.clone(),
        MemorySearchInput {
            query: "dev branch".to_owned(),
            max_results: Some(5),
            min_score: None,
        },
    )
    .await
    .unwrap();
    let output = text(result);
    assert!(
        output.contains("searched durable Brain instead"),
        "{output}"
    );
    assert!(output.contains("Branch State"), "{output}");
    assert!(
        !output.contains("LEGACY_BACKEND_SHOULD_NOT_APPEAR"),
        "{output}"
    );

    let result = xai_tool_runtime::Tool::run(
        &MemoryGetImpl,
        ctx,
        MemoryGetInput {
            path: format!("brain://{branch_id}"),
            from: None,
            lines: None,
        },
    )
    .await
    .unwrap();
    let output = text(result);
    assert!(output.contains("read durable Brain instead"), "{output}");
    assert!(output.contains("Branch State"), "{output}");
    assert!(
        !output.contains("LEGACY_BACKEND_SHOULD_NOT_APPEAR"),
        "{output}"
    );
}
