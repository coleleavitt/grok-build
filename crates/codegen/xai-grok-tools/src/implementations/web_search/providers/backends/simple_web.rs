use reqwest::header::{ACCEPT, USER_AGENT};
use serde::Deserialize;
use serde_json::Value;
use std::path::PathBuf;

use super::super::types::{BackendId, SearchResult};
use super::super::util::{encode, env, get_json, text, truncate};

pub async fn duckduckgo(query: &str, max_results: usize) -> Result<Vec<SearchResult>, String> {
    let url = format!(
        "https://api.duckduckgo.com/?q={}&format=json&no_redirect=1&no_html=1",
        encode(query)
    );
    let value = get_json(&url).await?;
    let mut results = Vec::new();
    let abstract_text = text(&value, &["AbstractText"]);
    let abstract_url = text(&value, &["AbstractURL"]);
    if !abstract_text.is_empty() || !abstract_url.is_empty() {
        results.push(SearchResult::new(
            BackendId::DuckDuckGo,
            1,
            text(&value, &["Heading"]),
            abstract_url,
            abstract_text,
        ));
    }
    collect_related_topics(
        value.get("RelatedTopics").unwrap_or(&Value::Null),
        &mut results,
        max_results,
    );
    Ok(results)
}

fn collect_related_topics(value: &Value, out: &mut Vec<SearchResult>, max_results: usize) {
    let Some(items) = value.as_array() else {
        return;
    };
    for item in items {
        if out.len() >= max_results {
            return;
        }
        if let Some(topics) = item.get("Topics") {
            collect_related_topics(topics, out, max_results);
            continue;
        }
        let text = item.get("Text").and_then(Value::as_str).unwrap_or_default();
        let url = item
            .get("FirstURL")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !text.is_empty() || !url.is_empty() {
            out.push(SearchResult::new(
                BackendId::DuckDuckGo,
                out.len() + 1,
                text.split('-').next().unwrap_or(text).trim(),
                url,
                text,
            ));
        }
    }
}

pub async fn gscholar(query: &str, max_results: usize) -> Result<Vec<SearchResult>, String> {
    let url = format!(
        "https://scholar.google.com/scholar_complete?q={}",
        encode(query)
    );
    let value = get_json(&url).await?;
    let suggestions = value
        .get("l")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(suggestions
        .iter()
        .take(max_results)
        .enumerate()
        .filter_map(|(idx, item)| {
            let suggestion = item
                .as_str()
                .or_else(|| item.get("t").and_then(Value::as_str))?;
            Some(SearchResult::new(
                BackendId::GoogleScholar,
                idx + 1,
                suggestion,
                format!(
                    "https://scholar.google.com/scholar?q={}",
                    encode(suggestion)
                ),
                "Google Scholar query suggestion",
            ))
        })
        .collect())
}

pub async fn wikipedia(query: &str, max_results: usize) -> Result<Vec<SearchResult>, String> {
    let url = format!(
        "https://en.wikipedia.org/w/api.php?action=query&list=search&format=json&srlimit={}&srsearch={}",
        max_results.min(50),
        encode(query)
    );
    let value = get_json(&url).await?;
    let items = value
        .pointer("/query/search")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(items
        .iter()
        .take(max_results)
        .enumerate()
        .map(|(idx, item)| {
            let title = text(item, &["title"]);
            SearchResult::new(
                BackendId::Wikipedia,
                idx + 1,
                title.clone(),
                format!(
                    "https://en.wikipedia.org/wiki/{}",
                    encode(&title).replace('+', "_")
                ),
                strip_html(&text(item, &["snippet"])),
            )
        })
        .collect())
}

pub async fn searxng(query: &str, max_results: usize) -> Result<Vec<SearchResult>, String> {
    let base = env("SEARXNG_URL")
        .ok_or_else(|| "searxng: set SEARXNG_URL to use a SearXNG instance".to_string())?;
    let url = format!(
        "{}/search?q={}&format=json",
        base.trim_end_matches('/'),
        encode(query)
    );
    let value = get_json(&url).await?;
    json_results(
        value.get("results").unwrap_or(&Value::Null),
        BackendId::SearXNG,
        max_results,
    )
}

pub async fn millionshort(query: &str, max_results: usize) -> Result<Vec<SearchResult>, String> {
    const BASE: &str = "https://millionshort.com";
    const UA: &str =
        "Mozilla/5.0 (compatible; grok-build/0.1; +https://github.com/coleleavitt/grok-build)";

    let client = reqwest::Client::builder()
        .user_agent(UA)
        .timeout(std::time::Duration::from_secs(15))
        .cookie_store(true)
        .build()
        .map_err(|err| format!("Million Short client init failed: {err}"))?;
    let search_path = format!("/search?keywords={}&remove=", encode(query));
    if let Some(creds) = load_millionshort_credentials() {
        let _ = client
            .post(format!("{BASE}/_login"))
            .header(ACCEPT, "text/html,application/xhtml+xml")
            .form(&[
                ("email", creds.username.as_str()),
                ("password", creds.password.as_str()),
                ("redirect", search_path.as_str()),
            ])
            .send()
            .await;
    }

    let response = client
        .get(format!("{BASE}{search_path}"))
        .header(USER_AGENT, UA)
        .header(ACCEPT, "text/html,application/xhtml+xml")
        .send()
        .await
        .map_err(|err| format!("Million Short request failed: {err}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|err| format!("Million Short response read failed: {err}"))?;
    if !status.is_success() {
        return Err(format!("Million Short returned HTTP {status}"));
    }
    if is_millionshort_login_page(&body) {
        return Err(
            "Million Short returned its login page; verify ~/.grok/credentials.toml [millionshort] credentials"
                .to_owned(),
        );
    }
    Ok(parse_millionshort_results(&body, max_results))
}

#[derive(Deserialize)]
struct CredentialsFile {
    millionshort: Option<MillionShortCredentials>,
}

#[derive(Deserialize)]
struct MillionShortCredentials {
    username: String,
    password: String,
}

fn load_millionshort_credentials() -> Option<MillionShortCredentials> {
    let home = std::env::var("HOME").ok()?;
    for path in [
        PathBuf::from(&home).join(".grok/credentials.toml"),
        PathBuf::from(&home).join(".config/jfc/credentials.toml"),
    ] {
        if let Ok(content) = std::fs::read_to_string(path)
            && let Ok(parsed) = toml::from_str::<CredentialsFile>(&content)
            && let Some(creds) = parsed
                .millionshort
                .filter(|c| !c.username.trim().is_empty() && !c.password.trim().is_empty())
        {
            return Some(creds);
        }
    }
    None
}

fn is_millionshort_login_page(html: &str) -> bool {
    html.contains("Login | Million Short") || html.contains("Login to continue")
}

fn parse_millionshort_results(html: &str, max_results: usize) -> Vec<SearchResult> {
    if is_millionshort_login_page(html) {
        return Vec::new();
    }
    let mut results = Vec::new();
    let mut rest = html;
    while results.len() < max_results.clamp(1, 20) {
        let Some(title_class) = rest.find("class=\"resultsTitle\"") else {
            break;
        };
        let title_segment = &rest[title_class..];
        let Some(anchor_start) = title_segment.find("<a") else {
            break;
        };
        let anchor = &title_segment[anchor_start..];
        let Some(body_start) = anchor.find('>') else {
            break;
        };
        let body = &anchor[body_start + 1..];
        let Some(body_end) = body.find("</a>") else {
            break;
        };
        let title = html_fragment_to_text(&body[..body_end]);
        let url = attr_value(anchor, "href").unwrap_or_default().to_owned();
        let after_title = &body[body_end + "</a>".len()..];
        let snippet = millionshort_description_text(after_title);
        if !title.is_empty() && !url.is_empty() {
            results.push(SearchResult::new(
                BackendId::MillionShort,
                results.len() + 1,
                title,
                url,
                snippet,
            ));
        }
        rest = after_title;
    }
    results
}

fn millionshort_description_text(segment_after_title: &str) -> String {
    let Some(class_idx) = segment_after_title.find("class=\"resultsDescription\"") else {
        return String::new();
    };
    let description = &segment_after_title[class_idx..];
    let Some(body_start) = description.find('>') else {
        return String::new();
    };
    let body = &description[body_start + 1..];
    let body_end = body.find("</div>").unwrap_or(body.len());
    html_fragment_to_text(&body[..body_end])
}

fn attr_value<'a>(html: &'a str, attr: &str) -> Option<&'a str> {
    let needle = format!("{attr}=\"");
    let value = html.split_once(&needle)?.1;
    let end = value.find('"')?;
    Some(&value[..end])
}

fn html_fragment_to_text(html: &str) -> String {
    strip_html(html)
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub async fn fourget(query: &str, max_results: usize) -> Result<Vec<SearchResult>, String> {
    let base = env("FOURGET_URL").unwrap_or_else(|| "https://4get.ca".to_string());
    let url = format!(
        "{}/api/v1/web?s={}&npt=1",
        base.trim_end_matches('/'),
        encode(query)
    );
    let value = get_json(&url).await?;
    json_results(
        value
            .get("web")
            .or_else(|| value.get("results"))
            .unwrap_or(&value),
        BackendId::FourGet,
        max_results,
    )
}

pub async fn primo(_query: &str, _max_results: usize) -> Result<Vec<SearchResult>, String> {
    Err("primo: configure an institution-specific Primo endpoint before use; use uni:, openalex:, arxiv:, crossref:, pubmed:, doaj:, dblp:, or papers: for key-free academic discovery. Use edu: only when Google CSE credentials are configured.".to_string())
}

pub async fn university(query: &str, max_results: usize) -> Result<Vec<SearchResult>, String> {
    let (institution, topic) = query
        .split_once(':')
        .map(|(institution, topic)| (institution.trim(), topic.trim()))
        .unwrap_or((query.trim(), ""));
    if institution.is_empty() {
        return Err("uni: expects `uni: <University>: <topic>`".into());
    }
    let search = if topic.is_empty() {
        institution.to_string()
    } else {
        format!("{institution} {topic}")
    };
    let mut results = super::academic::openalex(&search, max_results).await?;
    for result in &mut results {
        result.source = BackendId::University;
    }
    Ok(results)
}

const EDU_TLDS: &[&str] = &[
    ".edu", ".ac.uk", ".ac.jp", ".edu.cn", ".ac.cn", ".edu.au", ".ac.in", ".ac.kr", ".edu.hk",
    ".edu.tw", ".edu.sg", ".ac.nz", ".ac.za", ".edu.br",
];
const CN_TLDS: &[&str] = &[".edu.cn", ".ac.cn", ".edu.hk", ".edu.mo", ".edu.tw"];

pub async fn edu(query: &str, max_results: usize) -> Result<Vec<SearchResult>, String> {
    scoped_google(query, EDU_TLDS, BackendId::Edu, max_results).await
}

pub async fn china_academic(query: &str, max_results: usize) -> Result<Vec<SearchResult>, String> {
    scoped_google(query, CN_TLDS, BackendId::ChinaAcademic, max_results).await
}

pub async fn gov(query: &str, max_results: usize) -> Result<Vec<SearchResult>, String> {
    let gov_query = format!(
        "{query} (site:usa.gov OR site:nih.gov OR site:cdc.gov OR site:nsf.gov OR site:energy.gov OR site:nasa.gov OR site:gov.uk OR site:gc.ca OR site:europa.eu)"
    );
    let mut results = super::api_web::google(&gov_query, max_results).await?;
    for result in &mut results {
        result.source = BackendId::Gov;
    }
    Ok(results)
}

async fn scoped_google(
    query: &str,
    tlds: &[&str],
    source: BackendId,
    max_results: usize,
) -> Result<Vec<SearchResult>, String> {
    let group = tlds
        .iter()
        .map(|tld| format!("site:{tld}"))
        .collect::<Vec<_>>()
        .join(" OR ");
    let mut results = super::api_web::google(&format!("{query} ({group})"), max_results).await?;
    for result in &mut results {
        result.source = source;
    }
    Ok(results)
}

fn json_results(
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
                first_text(item, &["title", "name", "heading"]),
                first_text(item, &["url", "href", "link"]),
                first_text(item, &["snippet", "description", "body", "content"]),
            )
        })
        .collect())
}

fn first_text(value: &Value, names: &[&str]) -> String {
    names
        .iter()
        .map(|name| text(value, &[*name]))
        .find(|s| !s.is_empty())
        .unwrap_or_default()
}

fn strip_html(input: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for ch in input.chars() {
        match ch {
            '<' => inside = true,
            '>' => inside = false,
            _ if !inside => out.push(ch),
            _ => {}
        }
    }
    truncate(&out, 500)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_results_accept_common_shapes() {
        let value = json!([{ "title": "A", "url": "https://a", "snippet": "S" }]);
        let results = json_results(&value, BackendId::SearXNG, 5).unwrap();
        assert_eq!(results[0].title, "A");
        assert_eq!(results[0].url, "https://a");
    }

    #[test]
    fn parse_millionshort_results_reads_result_cards() {
        let html = r#"
            <div class="resultsTitle"><a href="https://example.com/article">Example &amp; Result</a></div>
            <div class="resultsDescription">Useful <b>snippet</b> text.</div>
        "#;
        let results = parse_millionshort_results(html, 5);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Example & Result");
        assert_eq!(results[0].url, "https://example.com/article");
        assert_eq!(results[0].snippet, "Useful snippet text.");
    }

    #[test]
    fn parse_millionshort_login_page_returns_empty() {
        let results = parse_millionshort_results(
            r#"<html><title>Login | Million Short</title><h6>Login to continue</h6></html>"#,
            5,
        );
        assert!(results.is_empty());
    }
}
