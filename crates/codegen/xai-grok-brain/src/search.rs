//! Brain search engine.
//!
//! This is the canonical Brain retrieval seam. Tooling may provide an optional
//! embedding provider (Nomic/OpenAI-compatible or deterministic tests), but all
//! page/source collection, lexical fallback, semantic ordering, freshness, and
//! workspace visibility live here instead of inside tool wrappers.

use std::future::Future;
use std::pin::Pin;

use crate::{BrainStore, RecalledMemoryPage, Result};

/// Embedding provider used by Brain semantic search.
///
/// Implementations live outside the core Brain crate (for example, a Nomic
/// HTTP provider in `xai-grok-tools`) so the store/engine stay independent of
/// network and credential concerns.
pub trait BrainEmbeddingProvider: Send + Sync {
    /// Embed every input in order.
    fn embed<'a>(
        &'a self,
        inputs: &'a [&'a str],
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Vec<Vec<f32>>>> + Send + 'a>>;

    /// Human-readable provider/model label used for evidence and telemetry.
    fn model_name(&self) -> &str;

    /// Expected embedding dimensions.
    fn dimensions(&self) -> usize;
}

/// Search request for Brain pages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrainSearchOptions {
    /// Natural-language query.
    pub query: String,
    /// Active workspace/repo scope id.
    pub workspace_scope: Option<String>,
    /// Maximum pages to return. `0` maps to the store default of 20.
    pub limit: usize,
}

/// Which branch produced search results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrainSearchMode {
    /// Semantic embedding ranking succeeded.
    Semantic,
    /// No provider was supplied.
    LexicalOnly,
    /// A provider was supplied but failed; lexical fallback was used.
    LexicalFallback,
}

/// Search result bundle.
#[derive(Debug, Clone, PartialEq)]
pub struct BrainSearchOutcome {
    /// Search result pages.
    pub pages: Vec<RecalledMemoryPage>,
    /// Branch used to produce these pages.
    pub mode: BrainSearchMode,
    /// Embedding model/provider label, when semantic was attempted.
    pub embedding_model: Option<String>,
}

/// Prepared search material collected from a Brain store before any async
/// provider call. This avoids holding rusqlite-backed store values across
/// `.await` in tool futures.
#[derive(Debug, Clone)]
pub struct PreparedBrainSearch {
    options: BrainSearchOptions,
    candidates: Vec<RecalledMemoryPage>,
}

impl PreparedBrainSearch {
    /// Inputs passed to the embedding provider: query first, then candidate pages.
    pub fn embedding_inputs(&self) -> Vec<String> {
        let mut inputs = Vec::with_capacity(self.candidates.len() + 1);
        inputs.push(self.options.query.clone());
        inputs.extend(self.candidates.iter().map(|candidate| {
            let page = &candidate.page;
            format!(
                "{}\ncategory: {}\nfreshness: {}\nscope: {}{}\n{}\nSources: {}",
                page.title,
                page.category.as_str(),
                page.freshness.as_str(),
                page.scope_kind.as_str(),
                page.scope_id
                    .as_deref()
                    .map(|scope| format!(":{scope}"))
                    .unwrap_or_default(),
                page.memory_text,
                candidate.source_labels.join(", ")
            )
        }));
        inputs
    }

    /// Complete semantic search from precomputed embeddings.
    ///
    /// `embeddings[0]` must be the query vector and the rest must align with
    /// [`Self::embedding_inputs`]' candidate order.
    pub fn complete_semantic(
        mut self,
        embeddings: Vec<Vec<f32>>,
        provider: &dyn BrainEmbeddingProvider,
    ) -> anyhow::Result<BrainSearchOutcome> {
        if embeddings.len() != self.candidates.len() + 1
            || embeddings.first().is_none_or(Vec::is_empty)
        {
            anyhow::bail!("unexpected embedding count/dimensions for Brain search");
        }
        let query_vec = &embeddings[0];
        for (idx, candidate) in self.candidates.iter_mut().enumerate() {
            let semantic = cosine_similarity(query_vec, &embeddings[idx + 1]);
            candidate.score = (semantic * 10_000.0).round() as i64;
        }
        sort_recalled(&mut self.candidates);
        let limit = normalize_limit(self.options.limit);
        self.candidates.truncate(limit);
        Ok(BrainSearchOutcome {
            pages: self.candidates,
            mode: BrainSearchMode::Semantic,
            embedding_model: Some(provider.model_name().to_owned()),
        })
    }

    fn complete_lexical(
        mut self,
        mode: BrainSearchMode,
        model: Option<String>,
    ) -> BrainSearchOutcome {
        let limit = normalize_limit(self.options.limit);
        self.candidates.truncate(limit);
        BrainSearchOutcome {
            pages: self.candidates,
            mode,
            embedding_model: model,
        }
    }
}

/// Canonical Brain search engine.
pub struct BrainSearchEngine;

impl BrainSearchEngine {
    /// Prepare visible candidate pages from the store using the store's lexical
    /// recall as the fallback ordering.
    pub fn prepare(store: &BrainStore, options: BrainSearchOptions) -> Result<PreparedBrainSearch> {
        let candidates = store.recall_pages(crate::RecallOptions {
            query: options.query.clone(),
            workspace_scope: options.workspace_scope.clone(),
            limit: normalize_limit(options.limit).max(20),
        })?;
        Ok(PreparedBrainSearch {
            options,
            candidates,
        })
    }

    /// Search with optional semantic provider. Provider failures are explicit in
    /// [`BrainSearchMode::LexicalFallback`].
    pub async fn search(
        store: &BrainStore,
        options: BrainSearchOptions,
        provider: Option<&dyn BrainEmbeddingProvider>,
    ) -> Result<BrainSearchOutcome> {
        let prepared = Self::prepare(store, options)?;
        Ok(Self::search_prepared(prepared, provider).await)
    }

    /// Search already-prepared candidates. Safe for tool futures because no
    /// SQLite-backed store is held across `.await`.
    pub async fn search_prepared(
        prepared: PreparedBrainSearch,
        provider: Option<&dyn BrainEmbeddingProvider>,
    ) -> BrainSearchOutcome {
        let Some(provider) = provider else {
            return prepared.complete_lexical(BrainSearchMode::LexicalOnly, None);
        };
        let model = Some(provider.model_name().to_owned());
        let inputs = prepared.embedding_inputs();
        let refs = inputs.iter().map(String::as_str).collect::<Vec<_>>();
        match provider.embed(&refs).await {
            Ok(embeddings) => {
                let lexical_fallback = prepared.clone();
                prepared
                    .complete_semantic(embeddings, provider)
                    .unwrap_or_else(|_| {
                        // A malformed provider response is treated like a provider
                        // failure: return the prepared lexical candidates, not an
                        // empty result set.
                        lexical_fallback.complete_lexical(BrainSearchMode::LexicalFallback, model)
                    })
            }
            Err(_) => prepared.complete_lexical(BrainSearchMode::LexicalFallback, model),
        }
    }
}

fn normalize_limit(limit: usize) -> usize {
    if limit == 0 { 20 } else { limit.min(20) }
}

fn sort_recalled(pages: &mut [RecalledMemoryPage]) {
    pages.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| b.page.updated_at.cmp(&a.page.updated_at))
            .then_with(|| b.page.id.cmp(&a.page.id))
    });
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BrainSettingsUpdate, MemoryCategory, NewPage};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct DeterministicProvider {
        calls: Arc<AtomicUsize>,
    }

    impl BrainEmbeddingProvider for DeterministicProvider {
        fn embed<'a>(
            &'a self,
            inputs: &'a [&'a str],
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<Vec<Vec<f32>>>> + Send + 'a>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                Ok(inputs
                    .iter()
                    .map(|input| {
                        if input.contains("dev") || input.contains("Branch State") {
                            vec![1.0, 0.0]
                        } else {
                            vec![0.0, 1.0]
                        }
                    })
                    .collect())
            })
        }

        fn model_name(&self) -> &str {
            "deterministic-nomic-compatible"
        }

        fn dimensions(&self) -> usize {
            2
        }
    }

    struct MalformedProvider {
        calls: Arc<AtomicUsize>,
    }

    impl BrainEmbeddingProvider for MalformedProvider {
        fn embed<'a>(
            &'a self,
            _inputs: &'a [&'a str],
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<Vec<Vec<f32>>>> + Send + 'a>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(vec![vec![1.0, 0.0]]) })
        }

        fn model_name(&self) -> &str {
            "malformed-nomic-compatible"
        }

        fn dimensions(&self) -> usize {
            2
        }
    }

    #[tokio::test]
    async fn semantic_provider_can_override_lexical_ordering() {
        let store = BrainStore::open_in_memory().unwrap();
        store
            .update_settings(BrainSettingsUpdate {
                enabled: Some(true),
                ..Default::default()
            })
            .unwrap();
        store
            .create_page(NewPage {
                title: Some("Install Safety".to_owned()),
                memory_text: "Avoid curl pipe bash installers. dev branch mention.".to_owned(),
                category: MemoryCategory::Notes,
                source: Some("manual".to_owned()),
            })
            .unwrap();
        store
            .create_or_update_page_by_title_scoped(
                NewPage {
                    title: Some("Branch State".to_owned()),
                    memory_text: "The custom fork now uses dev for active work.".to_owned(),
                    category: MemoryCategory::Workstreams,
                    source: Some("current_state".to_owned()),
                },
                Some("/repo/grok-build"),
            )
            .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = DeterministicProvider {
            calls: Arc::clone(&calls),
        };
        let outcome = BrainSearchEngine::search(
            &store,
            BrainSearchOptions {
                query: "current dev branch".to_owned(),
                workspace_scope: Some("/repo/grok-build".to_owned()),
                limit: 2,
            },
            Some(&provider),
        )
        .await
        .unwrap();
        assert_eq!(outcome.mode, BrainSearchMode::Semantic);
        assert_eq!(
            outcome.embedding_model.as_deref(),
            Some(provider.model_name())
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(outcome.pages[0].page.title, "Branch State");
    }

    #[tokio::test]
    async fn malformed_embedding_response_falls_back_to_lexical_candidates() {
        let store = BrainStore::open_in_memory().unwrap();
        store
            .create_page(NewPage {
                title: Some("Install Safety".to_owned()),
                memory_text: "Avoid curl pipe bash installers.".to_owned(),
                category: MemoryCategory::Notes,
                source: Some("manual".to_owned()),
            })
            .unwrap();
        store
            .create_page(NewPage {
                title: Some("Branch State".to_owned()),
                memory_text: "The custom fork now uses dev for active work.".to_owned(),
                category: MemoryCategory::Workstreams,
                source: Some("current_state".to_owned()),
            })
            .unwrap();

        let calls = Arc::new(AtomicUsize::new(0));
        let provider = MalformedProvider {
            calls: Arc::clone(&calls),
        };
        let outcome = BrainSearchEngine::search(
            &store,
            BrainSearchOptions {
                query: "install safety".to_owned(),
                workspace_scope: None,
                limit: 2,
            },
            Some(&provider),
        )
        .await
        .unwrap();

        assert_eq!(outcome.mode, BrainSearchMode::LexicalFallback);
        assert_eq!(
            outcome.embedding_model.as_deref(),
            Some(provider.model_name())
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(outcome.pages.len(), 2);
        assert_eq!(outcome.pages[0].page.title, "Install Safety");
        assert_eq!(outcome.pages[1].page.title, "Branch State");
    }

    #[tokio::test]
    async fn missing_provider_reports_lexical_branch() {
        let store = BrainStore::open_in_memory().unwrap();
        store
            .create_page(NewPage {
                title: Some("Install Safety".to_owned()),
                memory_text: "Avoid curl pipe bash installers.".to_owned(),
                category: MemoryCategory::Notes,
                source: Some("manual".to_owned()),
            })
            .unwrap();
        let outcome = BrainSearchEngine::search(
            &store,
            BrainSearchOptions {
                query: "install".to_owned(),
                workspace_scope: None,
                limit: 1,
            },
            None,
        )
        .await
        .unwrap();
        assert_eq!(outcome.mode, BrainSearchMode::LexicalOnly);
        assert_eq!(outcome.pages[0].page.title, "Install Safety");
    }
}
