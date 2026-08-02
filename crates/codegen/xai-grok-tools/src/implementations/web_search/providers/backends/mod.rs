mod academic;
mod api_web;
mod simple_web;

pub use academic::{
    arxiv, crossref, dblp, doaj, openalex, papers, pubmed, semantic_scholar, unpaywall,
};
pub use api_web::{brave, core, exa, google, tavily};
pub use simple_web::{
    china_academic, duckduckgo, edu, fourget, gov, gscholar, millionshort, primo, searxng,
    university, wikipedia,
};

use super::types::{BackendId, SearchBackend, SearchResult};

pub struct Backend {
    id: BackendId,
}

impl Backend {
    pub fn new(id: BackendId) -> Self {
        Self { id }
    }
}

#[async_trait::async_trait]
impl SearchBackend for Backend {
    fn id(&self) -> BackendId {
        self.id
    }

    fn is_available(&self) -> bool {
        match self.id {
            BackendId::Brave => super::util::env("BRAVE_API_KEY").is_some(),
            BackendId::Tavily => super::util::env("TAVILY_API_KEY").is_some(),
            BackendId::Exa => super::util::env("EXA_API_KEY").is_some(),
            BackendId::CORE => super::util::env("CORE_API_KEY").is_some(),
            BackendId::Google | BackendId::Edu | BackendId::Gov | BackendId::ChinaAcademic => {
                super::util::env("GOOGLE_CSE_API_KEY").is_some()
                    && super::util::env("GOOGLE_CSE_CX").is_some()
            }
            BackendId::SearXNG => super::util::env("SEARXNG_URL").is_some(),
            _ => true,
        }
    }

    async fn search(&self, query: &str, max_results: usize) -> Result<Vec<SearchResult>, String> {
        match self.id {
            BackendId::Google => google(query, max_results).await,
            BackendId::Brave => brave(query, max_results).await,
            BackendId::SearXNG => searxng(query, max_results).await,
            BackendId::DuckDuckGo => duckduckgo(query, max_results).await,
            BackendId::MillionShort => millionshort(query, max_results).await,
            BackendId::FourGet => fourget(query, max_results).await,
            BackendId::Tavily => tavily(query, max_results).await,
            BackendId::Exa => exa(query, max_results).await,
            BackendId::ArXiv => arxiv(query, max_results).await,
            BackendId::SemanticScholar => semantic_scholar(query, max_results).await,
            BackendId::GoogleScholar => gscholar(query, max_results).await,
            BackendId::OpenAlex => openalex(query, max_results).await,
            BackendId::DBLP => dblp(query, max_results).await,
            BackendId::Crossref => crossref(query, max_results).await,
            BackendId::PubMed => pubmed(query, max_results).await,
            BackendId::DOAJ => doaj(query, max_results).await,
            BackendId::CORE => core(query, max_results).await,
            BackendId::Wikipedia => wikipedia(query, max_results).await,
            BackendId::Primo => primo(query, max_results).await,
            BackendId::University => university(query, max_results).await,
            BackendId::Edu => edu(query, max_results).await,
            BackendId::Gov => gov(query, max_results).await,
            BackendId::ChinaAcademic => china_academic(query, max_results).await,
            BackendId::Unpaywall => unpaywall(query, max_results).await,
            BackendId::Papers => papers(query, max_results).await,
        }
    }
}
