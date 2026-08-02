use serde_json::{Value, json};

use super::super::types::{BackendId, SearchResult};
use super::super::util::{encode, env, get_json, missing_key, post_json, text, truncate};

pub async fn google(query: &str, max_results: usize) -> Result<Vec<SearchResult>, String> {
    let key = env("GOOGLE_CSE_API_KEY")
        .ok_or_else(|| missing_key(BackendId::Google, "GOOGLE_CSE_API_KEY"))?;
    let cx = env("GOOGLE_CSE_CX").ok_or_else(|| missing_key(BackendId::Google, "GOOGLE_CSE_CX"))?;
    let url = format!(
        "https://www.googleapis.com/customsearch/v1?key={}&cx={}&q={}&num={}",
        encode(&key),
        encode(&cx),
        encode(query),
        max_results.clamp(1, 10)
    );
    let value = get_json(&url).await?;
    parse_google_items(
        value.get("items").unwrap_or(&Value::Null),
        BackendId::Google,
        max_results,
    )
}

fn parse_google_items(
    value: &Value,
    source: BackendId,
    max_results: usize,
) -> Result<Vec<SearchResult>, String> {
    let Some(items) = value.as_array() else {
        return Ok(Vec::new());
    };
    Ok(items
        .iter()
        .take(max_results)
        .enumerate()
        .map(|(idx, item)| {
            SearchResult::new(
                source,
                idx + 1,
                text(item, &["title"]),
                text(item, &["link"]),
                text(item, &["snippet"]),
            )
        })
        .collect())
}

pub async fn brave(query: &str, max_results: usize) -> Result<Vec<SearchResult>, String> {
    let key = env("BRAVE_API_KEY").ok_or_else(|| missing_key(BackendId::Brave, "BRAVE_API_KEY"))?;
    let url = format!(
        "https://api.search.brave.com/res/v1/web/search?q={}&count={}",
        encode(query),
        max_results.clamp(1, 20)
    );
    let value: Value = super::super::util::http_client()?
        .get(&url)
        .header("X-Subscription-Token", key)
        .send()
        .await
        .map_err(|err| format!("Brave request failed: {err}"))?
        .json()
        .await
        .map_err(|err| format!("Brave JSON parse failed: {err}"))?;
    let items = value
        .pointer("/web/results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(items
        .iter()
        .take(max_results)
        .enumerate()
        .map(|(idx, item)| {
            SearchResult::new(
                BackendId::Brave,
                idx + 1,
                text(item, &["title"]),
                text(item, &["url"]),
                text(item, &["description"]),
            )
        })
        .collect())
}

pub async fn tavily(query: &str, max_results: usize) -> Result<Vec<SearchResult>, String> {
    let key =
        env("TAVILY_API_KEY").ok_or_else(|| missing_key(BackendId::Tavily, "TAVILY_API_KEY"))?;
    let body = json!({
        "api_key": key,
        "query": query,
        "max_results": max_results.min(20),
        "search_depth": "advanced",
        "include_answer": false,
    });
    let value = post_json("https://api.tavily.com/search", None, body).await?;
    parse_tavily_results(value.get("results").unwrap_or(&Value::Null), max_results)
}

fn parse_tavily_results(value: &Value, max_results: usize) -> Result<Vec<SearchResult>, String> {
    let Some(items) = value.as_array() else {
        return Ok(Vec::new());
    };
    Ok(items
        .iter()
        .take(max_results)
        .enumerate()
        .map(|(idx, item)| {
            SearchResult::new(
                BackendId::Tavily,
                idx + 1,
                text(item, &["title"]),
                text(item, &["url"]),
                text(item, &["content"]),
            )
        })
        .collect())
}

pub async fn exa(query: &str, max_results: usize) -> Result<Vec<SearchResult>, String> {
    let key = env("EXA_API_KEY").ok_or_else(|| missing_key(BackendId::Exa, "EXA_API_KEY"))?;
    let body = json!({
        "query": query,
        "numResults": max_results.min(25),
        "contents": { "text": { "maxCharacters": 500 } }
    });
    let value = post_json("https://api.exa.ai/search", Some(&key), body).await?;
    let items = value
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(items
        .iter()
        .take(max_results)
        .enumerate()
        .map(|(idx, item)| {
            SearchResult::new(
                BackendId::Exa,
                idx + 1,
                text(item, &["title"]),
                text(item, &["url"]),
                truncate(&text(item, &["text"]), 500),
            )
        })
        .collect())
}

pub async fn core(query: &str, max_results: usize) -> Result<Vec<SearchResult>, String> {
    super::academic::core(query, max_results).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_google_items_maps_title_link_snippet() {
        let value =
            json!([{ "title": "Rust", "link": "https://rust-lang.org", "snippet": "Language" }]);
        let results = parse_google_items(&value, BackendId::Google, 5).unwrap();
        assert_eq!(results[0].title, "Rust");
        assert_eq!(results[0].url, "https://rust-lang.org");
    }

    #[test]
    fn parse_tavily_results_maps_content() {
        let value = json!([{ "title": "A", "url": "https://a", "content": "body" }]);
        let results = parse_tavily_results(&value, 5).unwrap();
        assert_eq!(results[0].snippet, "body");
    }
}
