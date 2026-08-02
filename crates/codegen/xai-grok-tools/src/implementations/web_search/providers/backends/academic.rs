use serde_json::Value;

use super::super::types::{BackendId, SearchResult};
use super::super::util::{encode, env, get_json, text, text_array, truncate};

pub async fn arxiv(query: &str, max_results: usize) -> Result<Vec<SearchResult>, String> {
    let url = format!(
        "https://export.arxiv.org/api/query?search_query=all:{}&start=0&max_results={}",
        encode(query),
        max_results.min(50)
    );
    let body = super::super::util::http_client()?
        .get(&url)
        .send()
        .await
        .map_err(|err| format!("arXiv request failed: {err}"))?
        .text()
        .await
        .map_err(|err| format!("arXiv response read failed: {err}"))?;
    Ok(parse_arxiv_feed(&body, max_results))
}

fn parse_arxiv_feed(feed: &str, max_results: usize) -> Vec<SearchResult> {
    let mut out = Vec::new();
    for (idx, entry) in feed.split("<entry>").skip(1).enumerate() {
        if idx >= max_results {
            break;
        }
        let title = xml_text(entry, "title");
        let id = xml_text(entry, "id");
        let summary = xml_text(entry, "summary");
        let arxiv_id = id.rsplit('/').next().map(str::to_owned);
        out.push(
            SearchResult::new(
                BackendId::ArXiv,
                idx + 1,
                title,
                id,
                truncate(&summary, 600),
            )
            .with_arxiv_id(arxiv_id),
        );
    }
    out
}

fn xml_text(entry: &str, tag: &str) -> String {
    let start = format!("<{tag}>");
    let end = format!("</{tag}>");
    entry
        .split_once(&start)
        .and_then(|(_, rest)| rest.split_once(&end).map(|(value, _)| value))
        .map(html_unescape)
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn html_unescape(input: &str) -> String {
    input
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

pub async fn semantic_scholar(
    query: &str,
    max_results: usize,
) -> Result<Vec<SearchResult>, String> {
    let url = format!(
        "https://api.semanticscholar.org/graph/v1/paper/search?query={}&limit={}&fields=title,url,abstract,authors,year,citationCount,externalIds,venue",
        encode(query),
        max_results.min(20)
    );
    let mut request = super::super::util::http_client()?.get(url);
    if let Some(key) = env("SEMANTIC_SCHOLAR_API_KEY") {
        request = request.header("x-api-key", key);
    }
    let value: Value = request
        .send()
        .await
        .map_err(|err| format!("Semantic Scholar request failed: {err}"))?
        .json()
        .await
        .map_err(|err| format!("Semantic Scholar JSON parse failed: {err}"))?;
    let Some(items) = value.get("data").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    Ok(items
        .iter()
        .take(max_results)
        .enumerate()
        .map(|(idx, item)| {
            let authors = item
                .get("authors")
                .and_then(Value::as_array)
                .map(|authors| {
                    authors
                        .iter()
                        .filter_map(|a| a.get("name").and_then(Value::as_str))
                        .take(4)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            let snippet = [
                item.get("year")
                    .and_then(Value::as_i64)
                    .map(|y| y.to_string()),
                (!authors.is_empty()).then(|| format!("Authors: {authors}")),
                item.get("abstract")
                    .and_then(Value::as_str)
                    .map(|s| truncate(s, 500)),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" — ");
            SearchResult::new(
                BackendId::SemanticScholar,
                idx + 1,
                text(item, &["title"]),
                text(item, &["url"]),
                snippet,
            )
            .with_doi(text(item, &["externalIds", "DOI"]).into())
            .with_arxiv_id(text(item, &["externalIds", "ArXiv"]).into())
        })
        .collect())
}

pub async fn openalex(query: &str, max_results: usize) -> Result<Vec<SearchResult>, String> {
    let mailto = env("OPENALEX_EMAIL")
        .map(|email| format!("&mailto={}", encode(&email)))
        .unwrap_or_default();
    let url = format!(
        "https://api.openalex.org/works?search={}&per-page={}{}",
        encode(query),
        max_results.min(50),
        mailto,
    );
    let value = get_json(&url).await?;
    let Some(items) = value.get("results").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    Ok(items
        .iter()
        .take(max_results)
        .enumerate()
        .map(|(idx, item)| {
            let year = item.get("publication_year").and_then(Value::as_i64);
            let cited = item.get("cited_by_count").and_then(Value::as_i64);
            let snippet = format!(
                "{}{}",
                year.map(|y| format!("Published {y}. ")).unwrap_or_default(),
                cited.map(|c| format!("Cited by {c}. ")).unwrap_or_default(),
            );
            SearchResult::new(
                BackendId::OpenAlex,
                idx + 1,
                text(item, &["display_name"]),
                text(item, &["id"]),
                snippet,
            )
            .with_doi(text(item, &["doi"]).into())
        })
        .collect())
}

pub async fn dblp(query: &str, max_results: usize) -> Result<Vec<SearchResult>, String> {
    let url = format!(
        "https://dblp.org/search/publ/api?q={}&format=json&h={}",
        encode(query),
        max_results.min(100)
    );
    let value = get_json(&url).await?;
    let hits = value
        .pointer("/result/hits/hit")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(hits
        .iter()
        .take(max_results)
        .enumerate()
        .map(|(idx, item)| {
            let info = item.get("info").unwrap_or(item);
            let authors = author_list(info);
            let mut parts = vec![text(info, &["year"]), text(info, &["venue"])];
            if !authors.is_empty() {
                parts.push(format!("Authors: {authors}"));
            }
            let snippet = parts
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" — ");
            SearchResult::new(
                BackendId::DBLP,
                idx + 1,
                text(info, &["title"]),
                text(info, &["url"]),
                snippet,
            )
            .with_doi(text(info, &["doi"]).into())
        })
        .collect())
}

fn author_list(info: &Value) -> String {
    match info.pointer("/authors/author") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .take(5)
            .collect::<Vec<_>>()
            .join(", "),
        Some(Value::String(author)) => author.clone(),
        _ => String::new(),
    }
}

pub async fn crossref(query: &str, max_results: usize) -> Result<Vec<SearchResult>, String> {
    let mailto = env("CROSSREF_EMAIL")
        .map(|email| format!("&mailto={}", encode(&email)))
        .unwrap_or_default();
    let url = format!(
        "https://api.crossref.org/works?query={}&rows={}{}",
        encode(query),
        max_results.min(50),
        mailto,
    );
    let value = get_json(&url).await?;
    let items = value
        .pointer("/message/items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(items
        .iter()
        .take(max_results)
        .enumerate()
        .map(|(idx, item)| {
            let title = text_array(item, &["title"])
                .into_iter()
                .next()
                .unwrap_or_default();
            let container = text_array(item, &["container-title"])
                .into_iter()
                .next()
                .unwrap_or_default();
            SearchResult::new(
                BackendId::Crossref,
                idx + 1,
                title,
                text(item, &["URL"]),
                container,
            )
            .with_doi(text(item, &["DOI"]).into())
        })
        .collect())
}

pub async fn pubmed(query: &str, max_results: usize) -> Result<Vec<SearchResult>, String> {
    let esearch = format!(
        "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/esearch.fcgi?db=pubmed&term={}&retmode=json&retmax={}",
        encode(query),
        max_results.min(50)
    );
    let search = get_json(&esearch).await?;
    let ids = search
        .pointer("/esearchresult/idlist")
        .and_then(Value::as_array)
        .map(|ids| ids.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let summary = format!(
        "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/esummary.fcgi?db=pubmed&id={}&retmode=json",
        ids.join(",")
    );
    let value = get_json(&summary).await?;
    Ok(ids
        .iter()
        .take(max_results)
        .enumerate()
        .filter_map(|(idx, id)| {
            let item = value.pointer(&format!("/result/{id}"))?;
            Some(SearchResult::new(
                BackendId::PubMed,
                idx + 1,
                text(item, &["title"]),
                format!("https://pubmed.ncbi.nlm.nih.gov/{id}/"),
                text(item, &["source"]),
            ))
        })
        .collect())
}

pub async fn doaj(query: &str, max_results: usize) -> Result<Vec<SearchResult>, String> {
    let url = format!(
        "https://doaj.org/api/search/articles/{}?pageSize={}",
        encode(query),
        max_results.min(100)
    );
    let value = get_json(&url).await?;
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
            let bib = item.get("bibjson").unwrap_or(item);
            let link = bib
                .get("link")
                .and_then(Value::as_array)
                .and_then(|links| links.first())
                .and_then(|link| link.get("url"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            SearchResult::new(
                BackendId::DOAJ,
                idx + 1,
                text(bib, &["title"]),
                link,
                text(bib, &["journal", "title"]),
            )
            .with_doi(text(bib, &["identifier", "id"]).into())
        })
        .collect())
}

pub async fn unpaywall(query: &str, _max_results: usize) -> Result<Vec<SearchResult>, String> {
    let doi = query.trim().trim_start_matches("doi:").trim();
    if doi.is_empty() {
        return Err("unpaywall: expects a DOI, e.g. `unpaywall: 10.1038/nature12373`".into());
    }
    let email = env("UNPAYWALL_EMAIL")
        .or_else(|| env("OPENALEX_EMAIL"))
        .unwrap_or_else(|| "team@grok.local".to_string());
    let url = format!(
        "https://api.unpaywall.org/v2/{}?email={}",
        encode(doi),
        encode(&email)
    );
    let value = get_json(&url).await?;
    let oa = value.get("best_oa_location").unwrap_or(&Value::Null);
    let pdf = text(oa, &["url_for_pdf"]);
    let landing = text(oa, &["url"]);
    Ok(vec![
        SearchResult::new(
            BackendId::Unpaywall,
            1,
            text(&value, &["title"]),
            if pdf.is_empty() { landing } else { pdf },
            text(&value, &["journal_name"]),
        )
        .with_doi(Some(doi.to_string())),
    ])
}

pub async fn core(query: &str, max_results: usize) -> Result<Vec<SearchResult>, String> {
    let key = env("CORE_API_KEY")
        .ok_or_else(|| super::super::util::missing_key(BackendId::CORE, "CORE_API_KEY"))?;
    let url = format!(
        "https://api.core.ac.uk/v3/search/works?q={}&limit={}",
        encode(query),
        max_results.min(100)
    );
    let value = super::super::util::http_client()?
        .get(&url)
        .header("Authorization", format!("Bearer {key}"))
        .send()
        .await
        .map_err(|err| format!("CORE request failed: {err}"))?
        .json::<Value>()
        .await
        .map_err(|err| format!("CORE JSON parse failed: {err}"))?;
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
                BackendId::CORE,
                idx + 1,
                text(item, &["title"]),
                text(item, &["downloadUrl"]),
                text(item, &["abstract"]),
            )
            .with_doi(text(item, &["doi"]).into())
        })
        .collect())
}

pub async fn papers(query: &str, max_results: usize) -> Result<Vec<SearchResult>, String> {
    let per_backend = max_results.max(3);
    let (arxiv_results, s2_results, openalex_results) = tokio::join!(
        arxiv(query, per_backend),
        semantic_scholar(query, per_backend),
        openalex(query, per_backend),
    );
    let mut groups = Vec::new();
    for result in [arxiv_results, s2_results, openalex_results] {
        if let Ok(items) = result {
            groups.push(items);
        }
    }
    Ok(super::super::router::merge_rrf(groups, 60.0, max_results))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arxiv_parser_extracts_title_and_id() {
        let feed = r#"<feed><entry><id>http://arxiv.org/abs/1234.5678</id><title> Test Paper </title><summary> Useful abstract. </summary></entry></feed>"#;
        let parsed = parse_arxiv_feed(feed, 5);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].title, "Test Paper");
        assert_eq!(parsed[0].arxiv_id.as_deref(), Some("1234.5678"));
    }
}
