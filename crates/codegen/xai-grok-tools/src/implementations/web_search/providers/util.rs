use serde_json::Value;

use super::types::{BackendId, SearchResult};

pub fn encode(input: &str) -> String {
    url::form_urlencoded::byte_serialize(input.as_bytes()).collect()
}

pub fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("grok-build/0.1 web-search-providers")
        .build()
        .map_err(|err| format!("HTTP client init failed: {err}"))
}

pub async fn get_json(url: &str) -> Result<Value, String> {
    let response = http_client()?
        .get(url)
        .send()
        .await
        .map_err(|err| format!("HTTP GET failed for {}: {err}", redact_query(url)))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|err| format!("failed reading response body: {err}"))?;
    if !status.is_success() {
        return Err(format!(
            "{} returned {status}: {}",
            host_label(url),
            truncate(&text, 500)
        ));
    }
    serde_json::from_str(&text).map_err(|err| format!("failed parsing JSON from {url}: {err}"))
}

pub async fn post_json(url: &str, bearer: Option<&str>, body: Value) -> Result<Value, String> {
    let mut request = http_client()?.post(url).json(&body);
    if let Some(token) = bearer {
        request = request.bearer_auth(token);
    }
    let response = request
        .send()
        .await
        .map_err(|err| format!("HTTP POST failed for {}: {err}", redact_query(url)))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|err| format!("failed reading response body: {err}"))?;
    if !status.is_success() {
        return Err(format!(
            "{} returned {status}: {}",
            host_label(url),
            truncate(&text, 500)
        ));
    }
    serde_json::from_str(&text).map_err(|err| format!("failed parsing JSON from {url}: {err}"))
}

pub fn text(value: &Value, path: &[&str]) -> String {
    let mut cur = value;
    for segment in path {
        let Some(next) = cur.get(*segment) else {
            return String::new();
        };
        cur = next;
    }
    match cur {
        Value::String(s) => s.trim().to_owned(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

pub fn text_array(value: &Value, path: &[&str]) -> Vec<String> {
    let mut cur = value;
    for segment in path {
        let Some(next) = cur.get(*segment) else {
            return Vec::new();
        };
        cur = next;
    }
    match cur {
        Value::Array(items) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect(),
        Value::String(s) if !s.trim().is_empty() => vec![s.trim().to_owned()],
        _ => Vec::new(),
    }
}

pub fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

pub fn missing_key(backend: BackendId, env_name: &str) -> String {
    format!(
        "{} requires {env_name}; set it or use a key-free backend prefix such as wiki:, ddg:, arxiv:, openalex:, crossref:, pubmed:, doaj:, dblp:, papers:, or unpaywall:. The edu:, gov:, cn:, and google: prefixes use Google CSE and require GOOGLE_CSE_API_KEY plus GOOGLE_CSE_CX.",
        backend.name()
    )
}

pub fn format_results(query: &str, results: &[SearchResult], sources: &[BackendId]) -> String {
    let mut out = format!(
        "Web search providers: \"{query}\" — {} result(s)",
        results.len()
    );
    if !sources.is_empty() {
        out.push_str(" from ");
        out.push_str(
            &sources
                .iter()
                .map(|source| source.name())
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    out.push_str("\n\n");
    for (idx, result) in results.iter().enumerate() {
        out.push_str(&format!(
            "{}. {} [{}]\n",
            idx + 1,
            empty_fallback(&result.title, "Untitled"),
            result.source.name(),
        ));
        if !result.url.trim().is_empty() {
            out.push_str(&format!("   URL: {}\n", result.url.trim()));
        }
        if let Some(doi) = &result.doi {
            out.push_str(&format!("   DOI: {doi}\n"));
        }
        if let Some(arxiv) = &result.arxiv_id {
            out.push_str(&format!("   arXiv: {arxiv}\n"));
        }
        if !result.snippet.trim().is_empty() {
            out.push_str(&format!("   {}\n", one_line(&result.snippet)));
        }
        out.push('\n');
    }
    out
}

pub fn citations(results: &[SearchResult]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    results
        .iter()
        .filter_map(|result| {
            let url = result.url.trim();
            (!url.is_empty() && seen.insert(url.to_owned())).then(|| url.to_owned())
        })
        .collect()
}

pub fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let mut out = text.chars().take(max_chars).collect::<String>();
    out.push('…');
    out
}

pub fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn empty_fallback<'a>(text: &'a str, fallback: &'a str) -> &'a str {
    if text.trim().is_empty() {
        fallback
    } else {
        text.trim()
    }
}

fn host_label(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_else(|| "search backend".to_string())
}

fn redact_query(url: &str) -> String {
    reqwest::Url::parse(url)
        .map(|mut parsed| {
            parsed.set_query(Some("…"));
            parsed.to_string()
        })
        .unwrap_or_else(|_| url.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_results_includes_sources_and_citations() {
        let result = SearchResult::new(
            BackendId::Wikipedia,
            1,
            "Rust",
            "https://example.com/rust",
            "systems language",
        );
        let text = format_results(
            "rust",
            std::slice::from_ref(&result),
            &[BackendId::Wikipedia],
        );
        assert!(text.contains("Wikipedia"));
        assert!(text.contains("systems language"));
        assert_eq!(citations(&[result]), vec!["https://example.com/rust"]);
    }
}
