use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendId {
    Google,
    Brave,
    SearXNG,
    DuckDuckGo,
    MillionShort,
    FourGet,
    Tavily,
    Exa,
    ArXiv,
    SemanticScholar,
    GoogleScholar,
    OpenAlex,
    DBLP,
    Crossref,
    PubMed,
    DOAJ,
    CORE,
    Wikipedia,
    Primo,
    University,
    Edu,
    Gov,
    ChinaAcademic,
    Unpaywall,
    Papers,
}

impl BackendId {
    pub fn name(self) -> &'static str {
        match self {
            Self::Google => "Google CSE",
            Self::Brave => "Brave",
            Self::SearXNG => "SearXNG",
            Self::DuckDuckGo => "DuckDuckGo",
            Self::MillionShort => "Million Short",
            Self::FourGet => "4get",
            Self::Tavily => "Tavily",
            Self::Exa => "Exa",
            Self::ArXiv => "arXiv",
            Self::SemanticScholar => "Semantic Scholar",
            Self::GoogleScholar => "Google Scholar suggest",
            Self::OpenAlex => "OpenAlex",
            Self::DBLP => "DBLP",
            Self::Crossref => "Crossref",
            Self::PubMed => "PubMed",
            Self::DOAJ => "DOAJ",
            Self::CORE => "CORE",
            Self::Wikipedia => "Wikipedia",
            Self::Primo => "Primo",
            Self::University => "University research",
            Self::Edu => "Academic web",
            Self::Gov => "Government web",
            Self::ChinaAcademic => "Chinese academic web",
            Self::Unpaywall => "Unpaywall",
            Self::Papers => "Papers",
        }
    }

    pub fn prefix(self) -> &'static str {
        match self {
            Self::Google => "google",
            Self::Brave => "brave",
            Self::SearXNG => "searxng",
            Self::DuckDuckGo => "ddg",
            Self::MillionShort => "millionshort",
            Self::FourGet => "4get",
            Self::Tavily => "tavily",
            Self::Exa => "exa",
            Self::ArXiv => "arxiv",
            Self::SemanticScholar => "scholar",
            Self::GoogleScholar => "gscholar",
            Self::OpenAlex => "openalex",
            Self::DBLP => "dblp",
            Self::Crossref => "crossref",
            Self::PubMed => "pubmed",
            Self::DOAJ => "doaj",
            Self::CORE => "core",
            Self::Wikipedia => "wiki",
            Self::Primo => "primo",
            Self::University => "uni",
            Self::Edu => "edu",
            Self::Gov => "gov",
            Self::ChinaAcademic => "cn",
            Self::Unpaywall => "unpaywall",
            Self::Papers => "papers",
        }
    }

    pub fn requires_key(self) -> bool {
        matches!(
            self,
            Self::Brave
                | Self::Tavily
                | Self::Exa
                | Self::CORE
                | Self::Google
                | Self::Edu
                | Self::Gov
                | Self::ChinaAcademic
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
    pub doi: Option<String>,
    pub arxiv_id: Option<String>,
    pub source: BackendId,
    pub rank: usize,
}

impl SearchResult {
    pub fn new(
        source: BackendId,
        rank: usize,
        title: impl Into<String>,
        url: impl Into<String>,
        snippet: impl Into<String>,
    ) -> Self {
        Self {
            title: title.into(),
            url: url.into(),
            snippet: snippet.into(),
            doi: None,
            arxiv_id: None,
            source,
            rank,
        }
    }

    pub fn with_doi(mut self, doi: Option<String>) -> Self {
        self.doi = doi.filter(|d| !d.trim().is_empty());
        self
    }

    pub fn with_arxiv_id(mut self, arxiv_id: Option<String>) -> Self {
        self.arxiv_id = arxiv_id.filter(|id| !id.trim().is_empty());
        self
    }

    pub fn dedup_keys(&self) -> Vec<String> {
        let mut keys = Vec::new();
        if let Some(doi) = &self.doi {
            keys.push(format!("doi:{}", doi.trim().to_ascii_lowercase()));
        }
        if let Some(arxiv) = &self.arxiv_id {
            keys.push(format!("arxiv:{}", arxiv.trim().to_ascii_lowercase()));
        }
        let url = normalize_url_key(&self.url);
        if !url.is_empty() {
            keys.push(format!("url:{url}"));
        }
        let title = normalize_title_key(&self.title);
        if !title.is_empty() {
            keys.push(format!("title:{title}"));
        }
        keys
    }
}

fn normalize_url_key(url: &str) -> String {
    url.trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("www.")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

fn normalize_title_key(title: &str) -> String {
    title
        .to_ascii_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSearchOutput {
    pub content: String,
    pub citations: Vec<String>,
}

#[async_trait::async_trait]
pub trait SearchBackend: Send + Sync {
    fn id(&self) -> BackendId;
    fn is_available(&self) -> bool {
        true
    }
    fn timeout(&self) -> Duration {
        Duration::from_secs(12)
    }
    async fn search(&self, query: &str, max_results: usize) -> Result<Vec<SearchResult>, String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryClass {
    General,
    Academic,
    Code,
    Reference,
    News,
}

impl QueryClass {
    pub fn classify(query: &str) -> Self {
        let q = query.to_ascii_lowercase();
        let scored = [
            (Self::Academic, score(&q, ACADEMIC_TERMS)),
            (Self::Code, score(&q, CODE_TERMS)),
            (Self::Reference, score(&q, REFERENCE_TERMS)),
            (Self::News, score(&q, NEWS_TERMS)),
        ];
        scored
            .into_iter()
            .filter(|(_, score)| *score > 0)
            .max_by_key(|(_, score)| *score)
            .map(|(class, _)| class)
            .unwrap_or(Self::General)
    }

    pub fn backend_ids(self) -> &'static [BackendId] {
        match self {
            Self::Academic => &[
                BackendId::ArXiv,
                BackendId::SemanticScholar,
                BackendId::GoogleScholar,
                BackendId::OpenAlex,
                BackendId::Crossref,
                BackendId::PubMed,
                BackendId::DBLP,
                BackendId::DOAJ,
                #[cfg(feature = "web-search-credentialed-providers")]
                BackendId::Google,
            ],
            Self::Code => &[
                #[cfg(feature = "web-search-credentialed-providers")]
                BackendId::Google,
                BackendId::OpenAlex,
                BackendId::DBLP,
                BackendId::DuckDuckGo,
                BackendId::Wikipedia,
            ],
            Self::Reference => &[
                BackendId::Wikipedia,
                BackendId::DuckDuckGo,
                #[cfg(feature = "web-search-credentialed-providers")]
                BackendId::Google,
                BackendId::OpenAlex,
            ],
            #[cfg(feature = "web-search-credentialed-providers")]
            Self::News => &[BackendId::Google, BackendId::Brave, BackendId::Tavily],
            #[cfg(not(feature = "web-search-credentialed-providers"))]
            Self::News => &[
                BackendId::DuckDuckGo,
                BackendId::Wikipedia,
                BackendId::OpenAlex,
            ],
            Self::General => &[
                #[cfg(feature = "web-search-credentialed-providers")]
                BackendId::Google,
                BackendId::DuckDuckGo,
                BackendId::Wikipedia,
                BackendId::OpenAlex,
            ],
        }
    }
}

const ACADEMIC_TERMS: &[&str] = &[
    "paper",
    "papers",
    "arxiv",
    "research",
    "study",
    "journal",
    "citation",
    "preprint",
    "publication",
    "literature",
    "survey",
    "benchmark",
    "dataset",
    "doi:",
    "pubmed",
];
const CODE_TERMS: &[&str] = &[
    "rust",
    "python",
    "javascript",
    "typescript",
    "crate",
    "npm",
    "cargo",
    "library",
    "framework",
    "api",
    "sdk",
    "implementation",
    "docs",
    "github",
    "bug",
    "async",
];
const REFERENCE_TERMS: &[&str] = &[
    "what is",
    "who is",
    "when was",
    "where is",
    "definition",
    "meaning",
    "wikipedia",
    "history of",
];
const NEWS_TERMS: &[&str] = &[
    "news",
    "latest",
    "today",
    "2024",
    "2025",
    "2026",
    "announcement",
    "release",
    "breaking",
];

fn score(query: &str, terms: &[&str]) -> usize {
    terms.iter().filter(|term| query.contains(**term)).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unprefixed_classification_selects_provider_backend_families() {
        let academic = QueryClass::classify("retrieval augmented generation survey papers");
        assert_eq!(academic, QueryClass::Academic);
        let academic_backends = academic.backend_ids();
        assert!(academic_backends.contains(&BackendId::ArXiv));
        assert!(academic_backends.contains(&BackendId::SemanticScholar));
        assert!(academic_backends.contains(&BackendId::OpenAlex));
        assert!(academic_backends.contains(&BackendId::Crossref));
        assert!(academic_backends.contains(&BackendId::PubMed));

        let reference = QueryClass::classify("what is retrieval augmented generation wikipedia");
        assert_eq!(reference, QueryClass::Reference);
        assert!(reference.backend_ids().contains(&BackendId::Wikipedia));

        let general = QueryClass::classify("best note taking apps");
        assert_eq!(general, QueryClass::General);
        assert!(general.backend_ids().contains(&BackendId::DuckDuckGo));
    }

    #[test]
    fn search_result_dedup_keys_include_doi_arxiv_url_and_title() {
        let result = SearchResult::new(
            BackendId::ArXiv,
            1,
            "Graph Attention Networks!",
            "https://www.example.com/paper/",
            "",
        )
        .with_doi(Some("10.1000/XYZ".into()))
        .with_arxiv_id(Some("1706.03762".into()));
        let keys = result.dedup_keys();
        assert!(keys.contains(&"doi:10.1000/xyz".to_string()));
        assert!(keys.contains(&"arxiv:1706.03762".to_string()));
        assert!(keys.contains(&"url:example.com/paper".to_string()));
        assert!(keys.contains(&"title:graph attention networks".to_string()));
    }

    #[test]
    fn scoped_google_prefixes_are_marked_key_gated() {
        assert!(BackendId::Google.requires_key());
        assert!(BackendId::Edu.requires_key());
        assert!(BackendId::Gov.requires_key());
        assert!(BackendId::ChinaAcademic.requires_key());
        assert!(!BackendId::Wikipedia.requires_key());
        assert!(!BackendId::DuckDuckGo.requires_key());
        assert!(!BackendId::ArXiv.requires_key());
    }
}
