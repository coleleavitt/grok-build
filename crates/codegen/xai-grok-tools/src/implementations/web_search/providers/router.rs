use tokio::time::timeout;

use super::backends::Backend;
use super::types::{BackendId, ProviderSearchOutput, QueryClass, SearchBackend, SearchResult};
use super::util::{citations, format_results};

pub const BACKEND_PREFIXES: &[(&str, BackendId)] = &[
    #[cfg(feature = "web-search-credentialed-providers")]
    ("google", BackendId::Google),
    #[cfg(feature = "web-search-credentialed-providers")]
    ("brave", BackendId::Brave),
    ("searxng", BackendId::SearXNG),
    ("ddg", BackendId::DuckDuckGo),
    ("duckduckgo", BackendId::DuckDuckGo),
    ("millionshort", BackendId::MillionShort),
    ("million", BackendId::MillionShort),
    ("4get", BackendId::FourGet),
    ("fourget", BackendId::FourGet),
    #[cfg(feature = "web-search-credentialed-providers")]
    ("tavily", BackendId::Tavily),
    #[cfg(feature = "web-search-credentialed-providers")]
    ("exa", BackendId::Exa),
    ("arxiv", BackendId::ArXiv),
    ("scholar", BackendId::SemanticScholar),
    ("semantic", BackendId::SemanticScholar),
    ("gscholar", BackendId::GoogleScholar),
    ("openalex", BackendId::OpenAlex),
    ("dblp", BackendId::DBLP),
    ("crossref", BackendId::Crossref),
    ("pubmed", BackendId::PubMed),
    ("doaj", BackendId::DOAJ),
    #[cfg(feature = "web-search-credentialed-providers")]
    ("core", BackendId::CORE),
    ("wiki", BackendId::Wikipedia),
    ("wikipedia", BackendId::Wikipedia),
    ("primo", BackendId::Primo),
    ("uni", BackendId::University),
    #[cfg(feature = "web-search-credentialed-providers")]
    ("edu", BackendId::Edu),
    #[cfg(feature = "web-search-credentialed-providers")]
    ("gov", BackendId::Gov),
    #[cfg(feature = "web-search-credentialed-providers")]
    ("cn", BackendId::ChinaAcademic),
    ("unpaywall", BackendId::Unpaywall),
    ("papers", BackendId::Papers),
];

pub fn split_backend_prefix(query: &str) -> Option<(BackendId, &str)> {
    let trimmed = query.trim_start();
    let lower = trimmed.to_ascii_lowercase();
    for (prefix, backend) in BACKEND_PREFIXES {
        if let Some(rest) = lower
            .strip_prefix(&format!("{prefix}:"))
            .or_else(|| lower.strip_prefix(&format!("{prefix} ")))
        {
            let cut = trimmed.len() - rest.len();
            return Some((*backend, trimmed[cut..].trim_start()));
        }
    }
    None
}

pub fn has_backend_prefix(query: &str) -> bool {
    split_backend_prefix(query).is_some()
}

pub async fn search(
    query: &str,
    max_results: usize,
    allowed_domains: Option<&[String]>,
) -> Result<ProviderSearchOutput, String> {
    let max_results = max_results.max(1);
    let (display_query, mut results) = if let Some((backend, rest)) = split_backend_prefix(query) {
        let backend_impl = Backend::new(backend);
        let results = backend_impl.search(rest, max_results).await?;
        (rest.trim().to_owned(), results)
    } else {
        let class = QueryClass::classify(query);
        let groups = search_backends(query, max_results, class.backend_ids()).await;
        let results = merge_rrf(groups, 60.0, max_results);
        (query.trim().to_owned(), results)
    };

    if let Some(domains) = allowed_domains {
        filter_domains(&mut results, domains);
    }
    if results.is_empty() {
        return Err(format!(
            "provider search returned no results for: {display_query}"
        ));
    }
    let sources = unique_sources(&results);
    Ok(ProviderSearchOutput {
        content: format_results(&display_query, &results, &sources),
        citations: citations(&results),
    })
}

async fn search_backends(
    query: &str,
    max_results: usize,
    backend_ids: &[BackendId],
) -> Vec<Vec<SearchResult>> {
    let futures = backend_ids.iter().copied().map(|id| async move {
        let backend = Backend::new(id);
        if !backend.is_available() {
            return Vec::new();
        }
        match timeout(backend.timeout(), backend.search(query, max_results)).await {
            Ok(Ok(results)) => results,
            Ok(Err(err)) => {
                tracing::debug!(backend = %id.name(), error = %err, "web search provider failed");
                Vec::new()
            }
            Err(_) => {
                tracing::debug!(backend = %id.name(), "web search provider timed out");
                Vec::new()
            }
        }
    });
    futures::future::join_all(futures).await
}

pub fn merge_rrf(groups: Vec<Vec<SearchResult>>, k: f64, max_results: usize) -> Vec<SearchResult> {
    use std::collections::HashMap;
    let mut key_to_group: HashMap<String, usize> = HashMap::new();
    let mut group_results: HashMap<usize, Vec<(SearchResult, f64)>> = HashMap::new();
    let mut next_group = 0usize;

    for group in groups {
        for result in group {
            let keys = result.dedup_keys();
            let score = 1.0 / (k + result.rank as f64);
            let group_id = keys.iter().find_map(|key| key_to_group.get(key).copied());
            let group_id = group_id.unwrap_or_else(|| {
                let id = next_group;
                next_group += 1;
                id
            });
            for key in keys {
                key_to_group.insert(key, group_id);
            }
            group_results
                .entry(group_id)
                .or_default()
                .push((result, score));
        }
    }

    let mut scored = group_results
        .into_values()
        .filter_map(|items| {
            let score = items.iter().map(|(_, score)| *score).sum::<f64>();
            let best = items
                .into_iter()
                .min_by_key(|(result, _)| result.rank)
                .map(|(result, _)| result)?;
            Some((best, score))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored
        .into_iter()
        .take(max_results)
        .map(|(result, _)| result)
        .collect()
}

fn unique_sources(results: &[SearchResult]) -> Vec<BackendId> {
    let mut seen = std::collections::HashSet::new();
    results
        .iter()
        .filter_map(|result| seen.insert(result.source).then_some(result.source))
        .collect()
}

fn filter_domains(results: &mut Vec<SearchResult>, allowed_domains: &[String]) {
    if allowed_domains.is_empty() {
        return;
    }
    results.retain(|result| {
        let Ok(url) = reqwest::Url::parse(&result.url) else {
            return false;
        };
        let Some(host) = url.host_str() else {
            return false;
        };
        allowed_domains.iter().any(|domain| {
            let domain = domain.trim().trim_start_matches('.').to_ascii_lowercase();
            let host = host.to_ascii_lowercase();
            host == domain || host.ends_with(&format!(".{domain}"))
        })
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_backend_prefix_supports_jfc_prefixes() {
        for prefix in [
            "arxiv",
            "scholar",
            "openalex",
            "crossref",
            "pubmed",
            "doaj",
            "unpaywall",
            "papers",
            "ddg",
            "wiki",
            "primo",
            "uni",
            "searxng",
            "millionshort",
            "4get",
            "dblp",
        ] {
            let query = format!("{prefix}: test query");
            assert!(split_backend_prefix(&query).is_some(), "missing {prefix}");
        }
    }

    #[cfg(feature = "web-search-credentialed-providers")]
    #[test]
    fn split_backend_prefix_supports_credentialed_prefixes_when_enabled() {
        for prefix in [
            "google", "brave", "tavily", "exa", "core", "edu", "gov", "cn",
        ] {
            let query = format!("{prefix}: test query");
            assert!(split_backend_prefix(&query).is_some(), "missing {prefix}");
        }
    }

    #[test]
    fn merge_rrf_dedupes_by_url() {
        let a = SearchResult::new(BackendId::Google, 1, "A", "https://example.com/a", "one");
        let b = SearchResult::new(BackendId::Brave, 1, "B", "https://example.com/a", "two");
        let merged = merge_rrf(vec![vec![a], vec![b]], 60.0, 10);
        assert_eq!(merged.len(), 1);
    }

    #[test]
    fn domain_filter_accepts_subdomains() {
        let mut results = vec![SearchResult::new(
            BackendId::Wikipedia,
            1,
            "A",
            "https://docs.example.com/a",
            "",
        )];
        filter_domains(&mut results, &["example.com".into()]);
        assert_eq!(results.len(), 1);
    }
}
