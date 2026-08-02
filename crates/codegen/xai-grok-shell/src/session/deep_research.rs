//! Deep research orchestration core.
//!
//! The model-facing path is the built-in `deep-research` subagent profile.  The
//! pure core here mirrors JFC's shape: plan focused subqueries, execute search
//! steps through an injected backend, tolerate partial failures, synthesize a
//! cited report, and render durable human/machine-readable artifacts.

#[async_trait::async_trait]
pub(crate) trait ResearchSearcher: Send + Sync {
    async fn search(&self, query: &str, max_results: usize) -> Result<String, String>;
}

/// Production search adapter for deep research.
///
/// It uses Grok's native JFC-style web-search provider router first, so backend
/// selectors like `arxiv:`, `papers:`, `pubmed:`, `openalex:`, `wiki:`,
/// `brave:`, and `exa:` route through the same shipped search surface the
/// `web_search` tool uses.
pub(crate) struct ProviderResearchSearcher;

#[async_trait::async_trait]
impl ResearchSearcher for ProviderResearchSearcher {
    async fn search(&self, query: &str, max_results: usize) -> Result<String, String> {
        xai_grok_tools::implementations::web_search::providers::search(query, max_results, None)
            .await
            .map(|output| output.content)
    }
}

#[async_trait::async_trait]
pub(crate) trait ResearchSynthesizer: Send + Sync {
    async fn synthesize(&self, question: &str, steps: &[ResearchStep]) -> Result<String, String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResearchRequest {
    pub question: String,
    pub clarifications: Vec<String>,
    pub max_steps: usize,
    pub results_per_step: usize,
    pub reformulate: bool,
}

impl ResearchRequest {
    pub(crate) fn new(question: impl Into<String>) -> Self {
        Self {
            question: question.into(),
            clarifications: Vec::new(),
            max_steps: 4,
            results_per_step: 5,
            reformulate: true,
        }
    }

    pub(crate) fn with_clarifications(mut self, clarifications: Vec<String>) -> Self {
        self.clarifications = clarifications;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ResearchStep {
    pub sub_query: String,
    pub search_query: String,
    pub outcome: ResearchStepOutcome,
}

impl ResearchStep {
    pub(crate) fn succeeded(&self) -> bool {
        matches!(self.outcome, ResearchStepOutcome::Found { .. })
    }

    pub(crate) fn evidence(&self) -> Option<&str> {
        match &self.outcome {
            ResearchStepOutcome::Found { content } => Some(content.as_str()),
            ResearchStepOutcome::Failed { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum ResearchStepOutcome {
    Found { content: String },
    Failed { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ResearchCitation {
    pub number: usize,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ResearchReport {
    pub question: String,
    pub plan: Vec<String>,
    pub steps: Vec<ResearchStep>,
    pub synthesis: String,
    pub followups: Vec<String>,
}

impl ResearchReport {
    pub(crate) fn successful_steps(&self) -> usize {
        self.steps.iter().filter(|step| step.succeeded()).count()
    }

    pub(crate) fn citations(&self) -> Vec<ResearchCitation> {
        self.steps
            .iter()
            .filter(|step| step.succeeded())
            .enumerate()
            .map(|(idx, step)| ResearchCitation {
                number: idx + 1,
                source: step.sub_query.clone(),
            })
            .collect()
    }

    pub(crate) fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# Deep Research\n\n");
        out.push_str("## Answer\n\n");
        out.push_str(&self.synthesis);
        out.push_str("\n\n## Evidence\n\n");
        let mut n = 0usize;
        for step in &self.steps {
            match &step.outcome {
                ResearchStepOutcome::Found { content } => {
                    n += 1;
                    out.push_str(&format!("- [{n}] {}\n", step.sub_query));
                    out.push_str(&format!("  - searched: `{}`\n", step.search_query));
                    out.push_str(&format!("  - evidence: {}\n", one_line(content)));
                }
                ResearchStepOutcome::Failed { reason } => {
                    out.push_str(&format!("- failed: {} — {}\n", step.sub_query, reason));
                }
            }
        }
        if !self.followups.is_empty() {
            out.push_str("\n## Follow-ups\n\n");
            for followup in &self.followups {
                out.push_str(&format!("- {followup}\n"));
            }
        }
        out
    }

    pub(crate) fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "question": self.question,
            "plan": self.plan,
            "synthesis": self.synthesis,
            "followups": self.followups,
            "citations": self.citations(),
            "steps": self.steps,
        })
    }

    pub(crate) fn artifacts(&self) -> ResearchArtifacts {
        ResearchArtifacts {
            markdown: self.to_markdown(),
            json: self.to_json(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ResearchArtifacts {
    pub markdown: String,
    pub json: serde_json::Value,
}

pub(crate) fn wants_deep_research(query: &str) -> bool {
    let lower = query.to_ascii_lowercase();
    [
        "research",
        "investigate",
        "compare",
        "deep dive",
        "look into",
        "what is",
        "how does",
        "why does",
        "analyze",
        "analyse",
    ]
    .iter()
    .any(|cue| lower.contains(cue))
}

pub(crate) fn plan_subqueries(request: &ResearchRequest) -> Vec<String> {
    let base = request.question.trim();
    let mut out = Vec::new();
    push_unique(&mut out, base.to_owned());
    for clarification in &request.clarifications {
        push_unique(&mut out, format!("{base} {clarification}"));
    }
    push_unique(&mut out, format!("{base} latest developments"));
    push_unique(&mut out, format!("{base} how it works"));
    push_unique(&mut out, format!("{base} limitations criticism"));
    out.truncate(request.max_steps.max(1));
    out
}

fn push_unique(out: &mut Vec<String>, query: String) {
    let query = query.trim();
    if !query.is_empty()
        && !out
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(query))
    {
        out.push(query.to_owned());
    }
}

pub(crate) const BACKEND_PREFIXES: &[&str] = &[
    "arxiv",
    "scholar",
    "openalex",
    "crossref",
    "pubmed",
    "doaj",
    "core",
    "unpaywall",
    "papers",
    "brave",
    "tavily",
    "exa",
    "ddg",
    "duckduckgo",
    "wiki",
    "wikipedia",
    "dblp",
    "gscholar",
    "google",
    "millionshort",
    "million",
    "4get",
    "fourget",
    "searxng",
    "primo",
    "uni",
    "edu",
    "gov",
    "cn",
];

pub(crate) fn split_backend_prefix(query: &str) -> (Option<&'static str>, &str) {
    let trimmed = query.trim_start();
    let lower = trimmed.to_ascii_lowercase();
    for &prefix in BACKEND_PREFIXES {
        if let Some(rest) = lower
            .strip_prefix(&format!("{prefix}:"))
            .or_else(|| lower.strip_prefix(&format!("{prefix} ")))
        {
            let cut = trimmed.len() - rest.len();
            return (Some(prefix), trimmed[cut..].trim_start());
        }
    }
    (None, query)
}

pub(crate) fn reformulate_query(query: &str) -> String {
    let (prefix, body) = split_backend_prefix(query);
    let body = reformulate_plain(body);
    match prefix {
        Some(prefix) if !body.is_empty() => format!("{prefix}: {body}"),
        Some(_) => query.trim().to_owned(),
        None => body,
    }
}

fn reformulate_plain(query: &str) -> String {
    let mut working = query
        .trim()
        .trim_end_matches(['?', '.', '!', ' '])
        .to_owned();
    let lower = working.to_ascii_lowercase();
    for lead in [
        "can you tell me about",
        "please tell me about",
        "tell me about",
        "please explain",
        "can you explain",
        "what can you tell me about",
        "please find",
        "can you find",
    ] {
        if let Some(rest) = lower.strip_prefix(lead) {
            let cut = working.len() - rest.len();
            working = working[cut..].trim_start().to_owned();
            break;
        }
    }
    working.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) async fn run_deep_research(
    request: ResearchRequest,
    searcher: &dyn ResearchSearcher,
    synthesizer: &dyn ResearchSynthesizer,
) -> Result<ResearchReport, String> {
    let question = request.question.trim().to_owned();
    if question.is_empty() {
        return Err("research question is empty".to_owned());
    }
    let plan = plan_subqueries(&request);
    let mut steps = Vec::with_capacity(plan.len());
    for sub_query in &plan {
        let search_query = if request.reformulate {
            reformulate_query(sub_query)
        } else {
            sub_query.clone()
        };
        let outcome = match searcher
            .search(&search_query, request.results_per_step)
            .await
        {
            Ok(text) => ResearchStepOutcome::Found { content: text },
            Err(err) => ResearchStepOutcome::Failed { reason: err },
        };
        steps.push(ResearchStep {
            sub_query: sub_query.clone(),
            search_query,
            outcome,
        });
    }
    if steps.iter().all(|step| !step.succeeded()) {
        return Err(format!("all {} research steps failed", steps.len()));
    }
    let synthesis = synthesizer
        .synthesize(&question, &steps)
        .await
        .unwrap_or_else(|_| local_synthesis(&question, &steps));
    let followups = generate_followups(&question, &plan);
    Ok(ResearchReport {
        question,
        plan,
        steps,
        synthesis,
        followups,
    })
}

fn local_synthesis(question: &str, steps: &[ResearchStep]) -> String {
    let mut out = format!("Research summary for: {question}");
    let mut n = 0;
    for step in steps.iter().filter(|step| step.succeeded()) {
        n += 1;
        out.push_str(&format!(
            "\n\n[{n}] {}\n{}",
            step.sub_query,
            step.evidence().unwrap_or_default()
        ));
    }
    out
}

fn generate_followups(question: &str, plan: &[String]) -> Vec<String> {
    let base = question.trim().trim_end_matches(['?', '.', '!']);
    if base.is_empty() {
        return Vec::new();
    }
    let covered = |needle: &str| {
        plan.iter()
            .any(|query| query.to_ascii_lowercase().contains(needle))
    };
    let mut out = Vec::new();
    if !covered("limitation") && !covered("criticism") {
        out.push(format!("What are the limitations or criticisms of {base}?"));
    }
    if !covered("alternative") && !covered("compare") {
        out.push(format!("What are the main alternatives to {base}?"));
    }
    if !covered("latest") && !covered("recent") {
        out.push(format!("What are the latest developments in {base}?"));
    }
    out.truncate(3);
    out
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct MockSearcher {
        calls: Mutex<Vec<String>>,
        fail_marker: &'static str,
    }

    #[async_trait::async_trait]
    impl ResearchSearcher for MockSearcher {
        async fn search(&self, query: &str, _max_results: usize) -> Result<String, String> {
            self.calls.lock().unwrap().push(query.to_owned());
            if query.contains(self.fail_marker) {
                Err(format!("failed {query}"))
            } else {
                Ok(format!("evidence for {query}"))
            }
        }
    }

    struct MockSynthesizer;

    #[async_trait::async_trait]
    impl ResearchSynthesizer for MockSynthesizer {
        async fn synthesize(
            &self,
            question: &str,
            evidence: &[ResearchStep],
        ) -> Result<String, String> {
            Ok(format!(
                "Answer to {question} using {} cited steps [1]",
                evidence.iter().filter(|s| s.succeeded()).count()
            ))
        }
    }

    #[test]
    fn deep_research_plans_deduped_subqueries_and_reformulates_prefixes() {
        let req = ResearchRequest::new("Can you explain Rust async?")
            .with_clarifications(vec!["runtime internals".into()]);
        let plan = plan_subqueries(&req);
        assert!(plan.len() >= 3);
        assert_eq!(plan[0], "Can you explain Rust async?");
        assert!(plan.iter().any(|q| q.contains("runtime internals")));
        assert_eq!(
            reformulate_query("arxiv: can you explain graph attention?"),
            "arxiv: graph attention"
        );
        assert_eq!(
            reformulate_query("please tell me about wasm GC?"),
            "wasm GC"
        );
    }

    #[tokio::test]
    async fn deep_research_tolerates_partial_failures_and_synthesizes_citations() {
        let searcher = MockSearcher {
            calls: Mutex::new(Vec::new()),
            fail_marker: "limitations",
        };
        let report = run_deep_research(
            ResearchRequest::new("Rust async cancellation"),
            &searcher,
            &MockSynthesizer,
        )
        .await
        .unwrap();

        assert!(report.plan.len() >= 3);
        assert!(report.steps.iter().any(|step| !step.succeeded()));
        assert!(report.successful_steps() > 0);
        assert!(report.synthesis.contains("[1]"));
        assert_eq!(report.citations()[0].number, 1);
        assert!(!report.followups.is_empty());
    }

    #[tokio::test]
    async fn deep_research_errors_when_all_searches_fail() {
        let searcher = MockSearcher {
            calls: Mutex::new(Vec::new()),
            fail_marker: "",
        };
        let err = run_deep_research(
            ResearchRequest::new("distributed tracing").with_clarifications(vec![]),
            &searcher,
            &MockSynthesizer,
        )
        .await
        .unwrap_err();
        assert!(err.contains("all"));
    }

    #[test]
    fn deep_research_artifacts_include_markdown_and_json_sidecar() {
        let report = ResearchReport {
            question: "Q".into(),
            plan: vec!["Q".into()],
            steps: vec![ResearchStep {
                sub_query: "Q".into(),
                search_query: "Q".into(),
                outcome: ResearchStepOutcome::Found {
                    content: "Evidence".into(),
                },
            }],
            synthesis: "Answer [1]".into(),
            followups: vec!["Next?".into()],
        };
        let artifacts = report.artifacts();
        assert!(artifacts.markdown.contains("# Deep Research"));
        assert_eq!(artifacts.json["question"], "Q");
        assert_eq!(artifacts.json["citations"][0]["number"], 1);
    }
}
