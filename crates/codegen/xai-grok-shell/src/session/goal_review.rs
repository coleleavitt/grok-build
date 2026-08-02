//! Automatic post-change adversarial review for `/goal` code-change completion.
//!
//! This module is deliberately independent from TUI presentation.  It turns a
//! captured goal diff into a staged review report:
//!
//! Scope → multi-angle find → dedup → independent verify → optional sweep →
//! synthesize/persist.  Production can back the [`ReviewAgent`] trait with real
//! subagents; tests inject deterministic responses at that model boundary while
//! exercising the shipped pipeline logic.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::session::goal_classifier::evidence::{CapturedChanges, ChangesCaptureError};

const REVIEW_DIR_NAME: &str = "review";
pub(crate) const FINDINGS_FILE: &str = "findings.json";
pub(crate) const REFUTED_FILE: &str = "refuted.json";
pub(crate) const STATS_FILE: &str = "stats.json";
pub(crate) const EVIDENCE_FILE: &str = "evidence.md";

/// Runtime rollout gate for the automatic goal review stage.
pub(crate) const GOAL_REVIEW_ENV: &str = "GROK_GOAL_REVIEW";

/// Enabled iff the rollout env var is a truthy value.  Default is off so the
/// feature is safe to land without changing existing goal behavior.
pub(crate) fn goal_review_enabled_from_env() -> bool {
    std::env::var(GOAL_REVIEW_ENV)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "on" | "yes" | "enable" | "enabled"
            )
        })
        .unwrap_or(false)
}

/// Path helper used by goal plumbing and tests.
pub(crate) fn review_dir_for_goal_dir(goal_dir: &Path) -> PathBuf {
    goal_dir.join(REVIEW_DIR_NAME)
}

/// Review effort.  High/max keep recall-biased plausible findings; max also
/// asks for a gap sweep after the first verified pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReviewEffort {
    High,
    Max,
}

impl ReviewEffort {
    fn run_sweep(self) -> bool {
        matches!(self, Self::Max)
    }
}

/// Stable finder angles, adapted from the Anthropic code-review workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReviewAngle {
    DiffLineScan,
    RemovedBehavior,
    CrossFileTrace,
    LanguagePitfalls,
    WrapperProxy,
    CleanupAltitude,
    GapSweep,
}

impl ReviewAngle {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::DiffLineScan => "diff-line-scan",
            Self::RemovedBehavior => "removed-behavior",
            Self::CrossFileTrace => "cross-file-trace",
            Self::LanguagePitfalls => "language-pitfalls",
            Self::WrapperProxy => "wrapper-proxy",
            Self::CleanupAltitude => "cleanup-altitude",
            Self::GapSweep => "gap-sweep",
        }
    }

    fn prompt_text(self) -> &'static str {
        match self {
            Self::DiffLineScan => {
                "Read every diff hunk line by line. Then inspect enclosing functions for changed hunks. Look for inverted conditions, missing await, null/empty dereference, off-by-one, wrong-variable copy-paste, swallowed errors, and boundary inputs that make the changed line wrong."
            }
            Self::RemovedBehavior => {
                "For every deleted or replaced line, identify the invariant or guard it used to enforce. Search the new code for where that invariant is restored. Surface removed guards, validation, error paths, or tests that covered real behavior."
            }
            Self::CrossFileTrace => {
                "For each changed function or public shape, inspect likely callers and callees. Look for changed preconditions, return shapes, timing/ordering dependencies, and parallel changes that make a call unsafe."
            }
            Self::LanguagePitfalls => {
                "Scan for language/framework-specific traps in the diff: JS falsy-zero and missing await, Python mutable defaults/late binding, Go nil-map writes/range capture, Rust error swallowing or wrong ownership assumptions, SQL injection, timezone drift, and similar pitfalls."
            }
            Self::WrapperProxy => {
                "When the diff touches wrappers, caches, proxies, decorators, adapters, or registries, check that methods delegate to the wrapped instance rather than recursively re-entering a global/session/cache, and that all used methods are forwarded."
            }
            Self::CleanupAltitude => {
                "Hunt cleanup and altitude issues in changed code: shallow bandaids, duplicated helpers, special cases that should be generalized, dead code, or maintainability defects with a concrete cost. Correctness bugs outrank cleanup."
            }
            Self::GapSweep => {
                "Re-read the diff looking only for defects not already listed. Focus on gaps the first pass misses: extracted code that dropped guards, lock-scope shrink, setup/teardown asymmetry, predicate methods with side effects, and config default flips. Do not repeat known candidates."
            }
        }
    }

    fn default_angles() -> &'static [Self] {
        &[
            Self::DiffLineScan,
            Self::RemovedBehavior,
            Self::CrossFileTrace,
            Self::LanguagePitfalls,
            Self::WrapperProxy,
            Self::CleanupAltitude,
        ]
    }
}

/// Candidate kind: correctness survives above cleanup when reports are capped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReviewFindingKind {
    Correctness,
    Cleanup,
    Security,
}

impl Default for ReviewFindingKind {
    fn default() -> Self {
        Self::Correctness
    }
}

/// Verifier verdict ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum ReviewVerdictKind {
    Confirmed,
    Plausible,
    Refuted,
}

/// Structured evidence citation.  Verifiers may write prose in `evidence`, but
/// `CONFIRMED` / `REFUTED` verdicts are honored only when at least one
/// citation is mechanically valid for the candidate and review scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ReviewCitation {
    pub file: String,
    pub line: u32,
    pub quote: String,
}

/// Scope established from the real goal diff.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ReviewScope {
    pub changed_files: Vec<String>,
    pub diff: String,
    pub diff_path: Option<String>,
    pub prior_findings: Vec<PersistedReviewFinding>,
}

impl ReviewScope {
    pub(crate) fn from_captured(
        captured: CapturedChanges,
        diff_path: Option<String>,
        prior_findings: Vec<PersistedReviewFinding>,
    ) -> Self {
        Self {
            changed_files: captured.changed_files,
            diff: captured.diff,
            diff_path,
            prior_findings,
        }
    }
}

/// Draft emitted by finder/sweep agents.  `id` is assigned after dedup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CandidateDraft {
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default)]
    pub kind: ReviewFindingKind,
    pub summary: String,
    pub failure_scenario: String,
}

/// Deduplicated candidate with stable id and source angle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ReviewCandidate {
    pub id: String,
    pub angle: ReviewAngle,
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    pub kind: ReviewFindingKind,
    pub summary: String,
    pub failure_scenario: String,
}

/// Candidate plus independent verifier result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct VerifiedReviewFinding {
    pub candidate: ReviewCandidate,
    pub verdict: ReviewVerdictKind,
    pub evidence: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<ReviewCitation>,
}

/// Small persisted representation used as the next review's anti-ratchet anchor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PersistedReviewFinding {
    pub id: String,
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    pub summary: String,
    pub verdict: ReviewVerdictKind,
    pub evidence: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<ReviewCitation>,
}

impl From<&VerifiedReviewFinding> for PersistedReviewFinding {
    fn from(value: &VerifiedReviewFinding) -> Self {
        Self {
            id: value.candidate.id.clone(),
            file: value.candidate.file.clone(),
            line: value.candidate.line,
            summary: value.candidate.summary.clone(),
            verdict: value.verdict,
            evidence: value.evidence.clone(),
            citations: value.citations.clone(),
        }
    }
}

/// Machine-readable review run stats.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ReviewStats {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped_reason: Option<String>,
    pub changed_file_count: usize,
    pub finder_count: usize,
    pub candidate_count: usize,
    pub deduped_candidate_count: usize,
    pub verified_count: usize,
    pub confirmed_count: usize,
    pub plausible_count: usize,
    pub refuted_count: usize,
    pub sweep_candidate_count: usize,
}

/// Full review output.  `findings` contains only non-refuted findings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ReviewReport {
    pub findings: Vec<VerifiedReviewFinding>,
    pub refuted: Vec<VerifiedReviewFinding>,
    pub stats: ReviewStats,
    pub evidence_md: String,
}

impl ReviewReport {
    pub(crate) fn skipped(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self {
            findings: Vec::new(),
            refuted: Vec::new(),
            stats: ReviewStats {
                enabled: false,
                skipped_reason: Some(reason.clone()),
                ..Default::default()
            },
            evidence_md: format!("# Goal review skipped\n\nReason: {reason}\n"),
        }
    }

    pub(crate) fn confirmed_findings(&self) -> Vec<&VerifiedReviewFinding> {
        self.findings
            .iter()
            .filter(|finding| finding.verdict == ReviewVerdictKind::Confirmed)
            .collect()
    }

    pub(crate) fn has_confirmed_findings(&self) -> bool {
        self.findings
            .iter()
            .any(|finding| finding.verdict == ReviewVerdictKind::Confirmed)
    }

    pub(crate) fn gaps_summary(&self) -> String {
        self.confirmed_findings()
            .into_iter()
            .map(|finding| {
                let line = finding
                    .candidate
                    .line
                    .map(|line| format!(":{line}"))
                    .unwrap_or_default();
                format!(
                    "- review {}{} — {} Evidence: {}",
                    finding.candidate.file, line, finding.candidate.summary, finding.evidence
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Model/subagent boundary for the review engine.
#[async_trait::async_trait]
pub(crate) trait ReviewAgent: Send + Sync {
    async fn find_candidates(
        &self,
        angle: ReviewAngle,
        prompt: String,
    ) -> Result<Vec<CandidateDraft>>;
    async fn verify_candidate(
        &self,
        candidate: &ReviewCandidate,
        prompt: String,
    ) -> Result<ReviewVerdict>;
    async fn sweep_candidates(&self, prompt: String) -> Result<Vec<CandidateDraft>>;
}

/// Independent verifier response before it is joined to its candidate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ReviewVerdict {
    pub verdict: ReviewVerdictKind,
    pub evidence: String,
    #[serde(default)]
    pub citations: Vec<ReviewCitation>,
}

/// Inputs for one staged review run over an already-captured scope.
pub(crate) struct ReviewPipelineInput<'a> {
    pub objective: &'a str,
    pub attempt: u32,
    pub effort: ReviewEffort,
    pub scope: ReviewScope,
}

/// Run the Anthropic-inspired staged review pipeline over a captured scope.
pub(crate) async fn run_review_pipeline(
    agent: &dyn ReviewAgent,
    input: ReviewPipelineInput<'_>,
) -> Result<ReviewReport> {
    if input.scope.changed_files.is_empty() {
        let mut skipped = ReviewReport::skipped("no_code_changes");
        skipped.stats.enabled = true;
        return Ok(skipped);
    }

    let mut per_angle: BTreeMap<ReviewAngle, Vec<CandidateDraft>> = BTreeMap::new();
    for &angle in ReviewAngle::default_angles() {
        let prompt = render_finder_prompt(angle, input.objective, &input.scope);
        let drafts = agent.find_candidates(angle, prompt).await?;
        per_angle.insert(angle, drafts);
    }

    let mut raw_count = 0usize;
    let mut deduped = Vec::new();
    let mut seen = HashSet::new();
    for (angle, drafts) in &per_angle {
        for draft in drafts {
            raw_count += 1;
            if !draft_is_valid(draft) {
                continue;
            }
            let key = dedup_key(draft);
            if !seen.insert(key) {
                continue;
            }
            let id = format!("R{}-{}", input.attempt, deduped.len() + 1);
            deduped.push(ReviewCandidate {
                id,
                angle: *angle,
                file: draft.file.trim().to_owned(),
                line: draft.line,
                kind: draft.kind,
                summary: draft.summary.trim().to_owned(),
                failure_scenario: draft.failure_scenario.trim().to_owned(),
            });
        }
    }

    let mut sweep_candidate_count = 0usize;
    if input.effort.run_sweep() {
        let prompt = render_sweep_prompt(input.objective, &input.scope, &deduped);
        let sweep = agent.sweep_candidates(prompt).await?;
        for draft in sweep {
            sweep_candidate_count += 1;
            if !draft_is_valid(&draft) {
                continue;
            }
            let key = dedup_key(&draft);
            if !seen.insert(key) {
                continue;
            }
            let id = format!("R{}-{}", input.attempt, deduped.len() + 1);
            deduped.push(ReviewCandidate {
                id,
                angle: ReviewAngle::GapSweep,
                file: draft.file.trim().to_owned(),
                line: draft.line,
                kind: draft.kind,
                summary: draft.summary.trim().to_owned(),
                failure_scenario: draft.failure_scenario.trim().to_owned(),
            });
        }
    }

    let mut findings = Vec::new();
    let mut refuted = Vec::new();
    for candidate in &deduped {
        let prompt = render_verifier_prompt(input.objective, &input.scope, candidate);
        let verdict = agent.verify_candidate(candidate, prompt).await?;
        let verified = normalize_verified_candidate(candidate, &input.scope, verdict);
        match verified.verdict {
            ReviewVerdictKind::Confirmed | ReviewVerdictKind::Plausible => findings.push(verified),
            ReviewVerdictKind::Refuted => refuted.push(verified),
        }
    }

    findings.sort_by(|a, b| review_rank(a).cmp(&review_rank(b)));
    let stats = ReviewStats {
        enabled: true,
        skipped_reason: None,
        changed_file_count: input.scope.changed_files.len(),
        finder_count: ReviewAngle::default_angles().len(),
        candidate_count: raw_count,
        deduped_candidate_count: deduped.len(),
        verified_count: findings.len() + refuted.len(),
        confirmed_count: findings
            .iter()
            .filter(|f| f.verdict == ReviewVerdictKind::Confirmed)
            .count(),
        plausible_count: findings
            .iter()
            .filter(|f| f.verdict == ReviewVerdictKind::Plausible)
            .count(),
        refuted_count: refuted.len(),
        sweep_candidate_count,
    };
    let evidence_md =
        render_evidence_md(input.objective, &input.scope, &findings, &refuted, &stats);
    Ok(ReviewReport {
        findings,
        refuted,
        stats,
        evidence_md,
    })
}

fn draft_is_valid(draft: &CandidateDraft) -> bool {
    !draft.file.trim().is_empty()
        && !draft.summary.trim().is_empty()
        && !draft.failure_scenario.trim().is_empty()
}

fn normalize_verified_candidate(
    candidate: &ReviewCandidate,
    scope: &ReviewScope,
    verdict: ReviewVerdict,
) -> VerifiedReviewFinding {
    let evidence = verdict.evidence.trim().to_owned();
    let valid_citations = valid_citations_for_candidate(candidate, scope, &verdict.citations);
    if !valid_citations.is_empty() || verdict.verdict == ReviewVerdictKind::Plausible {
        return VerifiedReviewFinding {
            candidate: candidate.clone(),
            verdict: verdict.verdict,
            evidence,
            citations: valid_citations,
        };
    }

    // A CONFIRMED/REFUTED verdict without a valid structured citation is not
    // evidence. Excluding or blocking on that candidate would make free-form
    // prose authoritative again. Keep it alive as PLAUSIBLE with a concrete
    // candidate-location anchor for follow-up.
    let original = if evidence.is_empty() {
        "<empty>".to_owned()
    } else {
        evidence
    };
    let location = candidate
        .line
        .map(|line| format!("{}:{line}", candidate.file))
        .unwrap_or_else(|| candidate.file.clone());
    VerifiedReviewFinding {
        candidate: candidate.clone(),
        verdict: ReviewVerdictKind::Plausible,
        evidence: format!(
            "{location} — verifier returned {} without a valid structured citation; original evidence: {original}",
            verdict_str(verdict.verdict)
        ),
        citations: Vec::new(),
    }
}

fn valid_citations_for_candidate(
    candidate: &ReviewCandidate,
    scope: &ReviewScope,
    citations: &[ReviewCitation],
) -> Vec<ReviewCitation> {
    citations
        .iter()
        .filter(|citation| citation_is_valid_for_candidate(candidate, scope, citation))
        .cloned()
        .collect()
}

fn citation_is_valid_for_candidate(
    candidate: &ReviewCandidate,
    scope: &ReviewScope,
    citation: &ReviewCitation,
) -> bool {
    let file = citation.file.trim();
    if file.is_empty() || citation.line == 0 || citation.quote.trim().is_empty() {
        return false;
    }
    if file.contains("..") || file.contains('\0') {
        return false;
    }
    file == candidate.file
        || scope.changed_files.iter().any(|changed| changed == file)
        || scope.prior_findings.iter().any(|prior| prior.file == file)
}

fn dedup_key(draft: &CandidateDraft) -> String {
    let line_bucket = draft.line.map(|line| (line / 5) * 5).unwrap_or(0);
    format!(
        "{}:{}:{}",
        draft.file.trim().to_ascii_lowercase(),
        line_bucket,
        draft
            .summary
            .trim()
            .to_ascii_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || c.is_ascii_whitespace())
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(80)
            .collect::<String>()
    )
}

fn review_rank(finding: &VerifiedReviewFinding) -> (u8, u8, String) {
    let kind = match finding.candidate.kind {
        ReviewFindingKind::Security => 0,
        ReviewFindingKind::Correctness => 1,
        ReviewFindingKind::Cleanup => 2,
    };
    let verdict = match finding.verdict {
        ReviewVerdictKind::Confirmed => 0,
        ReviewVerdictKind::Plausible => 1,
        ReviewVerdictKind::Refuted => 2,
    };
    (kind, verdict, finding.candidate.file.clone())
}

fn render_scope_block(scope: &ReviewScope) -> String {
    let files = if scope.changed_files.is_empty() {
        "(none)".to_owned()
    } else {
        scope
            .changed_files
            .iter()
            .map(|file| format!("- {file}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let prior = if scope.prior_findings.is_empty() {
        "(none)".to_owned()
    } else {
        scope
            .prior_findings
            .iter()
            .map(|finding| {
                let line = finding
                    .line
                    .map(|line| format!(":{line}"))
                    .unwrap_or_default();
                format!(
                    "- {}{} [{}] {} — {}",
                    finding.file,
                    line,
                    verdict_str(finding.verdict),
                    finding.summary,
                    finding.evidence
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let diff_ref = scope
        .diff_path
        .as_deref()
        .map(|path| format!("Diff file: {path}"))
        .unwrap_or_else(|| "Diff file: (inline)".to_owned());
    format!(
        "## Review scope\n{diff_ref}\nChanged files:\n{files}\n\n## Prior unresolved review findings\n{prior}\n"
    )
}

fn render_finder_prompt(angle: ReviewAngle, objective: &str, scope: &ReviewScope) -> String {
    format!(
        "## Code-review finder — {}\n\nObjective:\n{}\n\n{}\n\nAssigned angle:\n{}\n\nSurface candidates with file, optional line, kind, summary, and concrete failure_scenario. Pass every candidate with a nameable failure scenario through; an independent verifier judges it next. This is general correctness review, not the high-precision security-review path: do not pad with speculative security hardening or low-confidence vulnerability theory. Return JSON: {{\"candidates\":[...]}}.",
        angle.label(),
        objective,
        render_scope_block(scope),
        angle.prompt_text(),
    )
}

fn render_sweep_prompt(objective: &str, scope: &ReviewScope, known: &[ReviewCandidate]) -> String {
    let known = if known.is_empty() {
        "(none)".to_owned()
    } else {
        known
            .iter()
            .map(|c| format!("- {} {} — {}", c.id, c.file, c.summary))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "## Code-review sweep — gaps only\n\nObjective:\n{}\n\n{}\n\nAlready-found candidates (do NOT repeat):\n{}\n\n{}\n\nReturn JSON: {{\"candidates\":[...]}}.",
        objective,
        render_scope_block(scope),
        known,
        ReviewAngle::GapSweep.prompt_text(),
    )
}

fn render_verifier_prompt(
    objective: &str,
    scope: &ReviewScope,
    candidate: &ReviewCandidate,
) -> String {
    let line = candidate
        .line
        .map(|line| format!(":{line}"))
        .unwrap_or_default();
    format!(
        "## Code-review verifier\n\nObjective:\n{}\n\n{}\n\nCandidate:\nFile: {}{}\nAngle: {}\nSummary: {}\nFailure scenario: {}\n\nReturn exactly one verdict as JSON: {{\"verdict\":\"CONFIRMED|PLAUSIBLE|REFUTED\",\"evidence\":\"short prose for display\",\"citations\":[{{\"file\":\"path/to/file\",\"line\":123,\"quote\":\"exact quoted line or snippet\"}}]}}. CONFIRMED and REFUTED require at least one valid citation for the candidate or changed file; otherwise the harness will keep the candidate as PLAUSIBLE.\n\nVerdict ladder:\n- CONFIRMED: can name the input/state that triggers wrong behavior; cite and quote the line.\n- PLAUSIBLE: mechanism is real, trigger uncertain; state what would confirm it.\n- REFUTED: factually wrong, provably impossible, or guarded elsewhere; cite and quote the line that proves it.\nDo not refute merely because runtime state is uncommon when the state is realistic.",
        objective,
        render_scope_block(scope),
        candidate.file,
        line,
        candidate.angle.label(),
        candidate.summary,
        candidate.failure_scenario,
    )
}

fn render_evidence_md(
    objective: &str,
    scope: &ReviewScope,
    findings: &[VerifiedReviewFinding],
    refuted: &[VerifiedReviewFinding],
    stats: &ReviewStats,
) -> String {
    let mut out = String::new();
    out.push_str("# Automatic goal review\n\n");
    out.push_str("## Objective\n\n");
    out.push_str(objective);
    out.push_str("\n\n## Scope\n\n");
    out.push_str(&render_scope_block(scope));
    out.push_str("\n## Stats\n\n");
    out.push_str(&format!(
        "- finders: {}\n- candidates: {}\n- deduped: {}\n- verified: {}\n- confirmed: {}\n- plausible: {}\n- refuted: {}\n- sweep candidates: {}\n\n",
        stats.finder_count,
        stats.candidate_count,
        stats.deduped_candidate_count,
        stats.verified_count,
        stats.confirmed_count,
        stats.plausible_count,
        stats.refuted_count,
        stats.sweep_candidate_count,
    ));
    out.push_str("## Surviving findings\n\n");
    if findings.is_empty() {
        out.push_str("(none)\n\n");
    } else {
        for finding in findings {
            let line = finding
                .candidate
                .line
                .map(|line| format!(":{line}"))
                .unwrap_or_default();
            out.push_str(&format!(
                "- **{}** `{}`{} — {}\n  - scenario: {}\n  - evidence: {}\n",
                verdict_str(finding.verdict),
                finding.candidate.file,
                line,
                finding.candidate.summary,
                finding.candidate.failure_scenario,
                finding.evidence
            ));
            append_citations_md(&mut out, &finding.citations);
        }
        out.push('\n');
    }
    out.push_str("## Refuted candidates\n\n");
    if refuted.is_empty() {
        out.push_str("(none)\n");
    } else {
        for finding in refuted {
            out.push_str(&format!(
                "- `{}` — {}\n  - evidence: {}\n",
                finding.candidate.file, finding.candidate.summary, finding.evidence
            ));
            append_citations_md(&mut out, &finding.citations);
        }
    }
    out
}

fn append_citations_md(out: &mut String, citations: &[ReviewCitation]) {
    if citations.is_empty() {
        return;
    }
    out.push_str("  - citations:\n");
    for citation in citations {
        out.push_str(&format!(
            "    - `{}:{}` — {}\n",
            citation.file, citation.line, citation.quote
        ));
    }
}

fn verdict_str(verdict: ReviewVerdictKind) -> &'static str {
    match verdict {
        ReviewVerdictKind::Confirmed => "CONFIRMED",
        ReviewVerdictKind::Plausible => "PLAUSIBLE",
        ReviewVerdictKind::Refuted => "REFUTED",
    }
}

/// Persist machine-readable and human-readable review artifacts.
pub(crate) async fn persist_review_report(
    review_dir: &Path,
    report: &ReviewReport,
) -> Result<ReviewArtifactPaths> {
    tokio::fs::create_dir_all(review_dir)
        .await
        .with_context(|| format!("creating review dir {}", review_dir.display()))?;
    let findings_path = review_dir.join(FINDINGS_FILE);
    let refuted_path = review_dir.join(REFUTED_FILE);
    let stats_path = review_dir.join(STATS_FILE);
    let evidence_path = review_dir.join(EVIDENCE_FILE);

    let persisted_findings = report
        .findings
        .iter()
        .map(PersistedReviewFinding::from)
        .collect::<Vec<_>>();
    let persisted_refuted = report
        .refuted
        .iter()
        .map(PersistedReviewFinding::from)
        .collect::<Vec<_>>();

    tokio::fs::write(
        &findings_path,
        serde_json::to_vec_pretty(&persisted_findings)?,
    )
    .await
    .with_context(|| format!("writing {}", findings_path.display()))?;
    tokio::fs::write(
        &refuted_path,
        serde_json::to_vec_pretty(&persisted_refuted)?,
    )
    .await
    .with_context(|| format!("writing {}", refuted_path.display()))?;
    tokio::fs::write(&stats_path, serde_json::to_vec_pretty(&report.stats)?)
        .await
        .with_context(|| format!("writing {}", stats_path.display()))?;
    tokio::fs::write(&evidence_path, &report.evidence_md)
        .await
        .with_context(|| format!("writing {}", evidence_path.display()))?;

    Ok(ReviewArtifactPaths {
        findings: findings_path,
        refuted: refuted_path,
        stats: stats_path,
        evidence: evidence_path,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReviewArtifactPaths {
    pub findings: PathBuf,
    pub refuted: PathBuf,
    pub stats: PathBuf,
    pub evidence: PathBuf,
}

/// Summary converted into the existing goal-verifier NotAchieved path when
/// confirmed review findings survive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReviewCompletionBlock {
    pub details_path: String,
    pub gaps_summary: String,
    pub pause_summary: String,
    pub gap_fingerprint: String,
}

pub(crate) fn completion_block_for_report(
    report: &ReviewReport,
    paths: &ReviewArtifactPaths,
) -> Option<ReviewCompletionBlock> {
    if !report.has_confirmed_findings() {
        return None;
    }
    let gaps_summary = report.gaps_summary();
    let pause_summary = format!(
        "Automatic review found {} confirmed finding(s). Resolve them before marking the goal complete. See {}.",
        report.stats.confirmed_count,
        paths.evidence.display()
    );
    let gap_fingerprint = review_gap_fingerprint(report);
    Some(ReviewCompletionBlock {
        details_path: paths.evidence.to_string_lossy().into_owned(),
        gaps_summary,
        pause_summary,
        gap_fingerprint,
    })
}

fn review_gap_fingerprint(report: &ReviewReport) -> String {
    let mut parts = report
        .confirmed_findings()
        .into_iter()
        .map(|finding| {
            format!(
                "{}:{}:{}",
                finding.candidate.file,
                finding.candidate.line.unwrap_or_default(),
                finding.candidate.summary.to_ascii_lowercase()
            )
        })
        .collect::<Vec<_>>();
    parts.sort();
    parts.join("|")
}

/// Load prior non-refuted findings from the previous review, if any.
pub(crate) async fn load_prior_findings(review_dir: &Path) -> Vec<PersistedReviewFinding> {
    let path = review_dir.join(FINDINGS_FILE);
    let Ok(body) = tokio::fs::read_to_string(path).await else {
        return Vec::new();
    };
    serde_json::from_str::<Vec<PersistedReviewFinding>>(&body).unwrap_or_default()
}

/// Trigger wrapper: capture the real goal diff, run the pipeline when enabled,
/// persist artifacts, and return the report plus artifact paths.
pub(crate) async fn run_post_change_review(
    agent: &dyn ReviewAgent,
    enabled: bool,
    objective: &str,
    attempt: u32,
    effort: ReviewEffort,
    baseline_commit: Option<&str>,
    workspace_root: &Path,
    goal_created_at: i64,
    review_dir: &Path,
) -> Result<(ReviewReport, ReviewArtifactPaths)> {
    if !enabled {
        let report = ReviewReport::skipped("disabled");
        let paths = persist_review_report(review_dir, &report).await?;
        return Ok((report, paths));
    }

    let captured = match crate::session::goal_classifier::evidence::capture_changes_diff(
        baseline_commit,
        workspace_root,
        goal_created_at,
    )
    .await
    {
        Ok(captured) => captured,
        Err(ChangesCaptureError::WalkdirEmpty) => {
            let mut report = ReviewReport::skipped("no_code_changes");
            report.stats.enabled = true;
            let paths = persist_review_report(review_dir, &report).await?;
            return Ok((report, paths));
        }
        Err(err) => return Err(anyhow::anyhow!("review changes capture failed: {err}")),
    };

    if captured.changed_files.is_empty() {
        let mut report = ReviewReport::skipped("no_code_changes");
        report.stats.enabled = true;
        let paths = persist_review_report(review_dir, &report).await?;
        return Ok((report, paths));
    }

    let diff_path = review_dir.join("changes.patch");
    tokio::fs::create_dir_all(review_dir).await?;
    tokio::fs::write(&diff_path, &captured.diff).await?;
    let prior = load_prior_findings(review_dir).await;
    let scope = ReviewScope::from_captured(
        captured,
        Some(diff_path.to_string_lossy().into_owned()),
        prior,
    );
    let report = run_review_pipeline(
        agent,
        ReviewPipelineInput {
            objective,
            attempt,
            effort,
            scope,
        },
    )
    .await?;
    let paths = persist_review_report(review_dir, &report).await?;
    Ok((report, paths))
}

/// Minimal production ReviewAgent backed by the existing subagent coordinator.
pub(crate) struct SubagentReviewAgent {
    pub event_tx: tokio::sync::mpsc::UnboundedSender<
        xai_grok_tools::implementations::grok_build::task::types::SubagentEvent,
    >,
    pub parent_session_id: String,
    pub parent_prompt_id: Option<String>,
    pub cwd: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CandidateResponse {
    #[serde(default)]
    candidates: Vec<CandidateDraft>,
}

#[async_trait::async_trait]
impl ReviewAgent for SubagentReviewAgent {
    async fn find_candidates(
        &self,
        angle: ReviewAngle,
        prompt: String,
    ) -> Result<Vec<CandidateDraft>> {
        let response: CandidateResponse = self
            .run_json(&format!("review-find:{}", angle.label()), prompt)
            .await?;
        Ok(response.candidates)
    }

    async fn verify_candidate(
        &self,
        candidate: &ReviewCandidate,
        prompt: String,
    ) -> Result<ReviewVerdict> {
        self.run_json(&format!("review-verify:{}", candidate.id), prompt)
            .await
    }

    async fn sweep_candidates(&self, prompt: String) -> Result<Vec<CandidateDraft>> {
        let response: CandidateResponse = self.run_json("review-sweep", prompt).await?;
        Ok(response.candidates)
    }
}

impl SubagentReviewAgent {
    async fn run_json<T: for<'de> Deserialize<'de>>(
        &self,
        label: &str,
        prompt: String,
    ) -> Result<T> {
        use xai_grok_tools::implementations::grok_build::task::spawn::SubagentSpawnParams;
        use xai_grok_tools::implementations::grok_build::task::types::SubagentEvent;
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        let request = SubagentSpawnParams {
            id: None,
            prompt,
            description: label.to_owned(),
            subagent_type: "general-purpose".to_owned(),
            parent_session_id: self.parent_session_id.clone(),
            parent_prompt_id: self.parent_prompt_id.clone(),
            cwd: self.cwd.clone(),
            ..Default::default()
        }
        .into_request_with_result_tx(result_tx);
        self.event_tx
            .send(SubagentEvent::Spawn(Box::new(request)))
            .map_err(|_| anyhow::anyhow!("subagent coordinator channel closed"))?;
        let result = result_rx.await.context("subagent result channel dropped")?;
        if !result.success {
            return Err(anyhow::anyhow!(
                "review subagent failed: {}",
                result.error.unwrap_or_else(|| "unknown error".to_owned())
            ));
        }
        parse_json_response(result.output.as_ref())
    }
}

fn parse_json_response<T: for<'de> Deserialize<'de>>(text: &str) -> Result<T> {
    match serde_json::from_str(text.trim()) {
        Ok(value) => Ok(value),
        Err(first_err) => {
            let trimmed = text.trim();
            let start = trimmed
                .find('{')
                .context("review subagent output contained no JSON object")?;
            let end = trimmed
                .rfind('}')
                .context("review subagent output contained no complete JSON object")?;
            serde_json::from_str(&trimmed[start..=end]).with_context(|| {
                format!("failed to parse review subagent JSON; first error: {first_err}")
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct MockReviewAgent {
        finder_calls: Mutex<Vec<ReviewAngle>>,
        verify_calls: Mutex<Vec<String>>,
        prompts: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl ReviewAgent for MockReviewAgent {
        async fn find_candidates(
            &self,
            angle: ReviewAngle,
            prompt: String,
        ) -> Result<Vec<CandidateDraft>> {
            self.finder_calls.lock().unwrap().push(angle);
            self.prompts.lock().unwrap().push(prompt);
            let out = match angle {
                ReviewAngle::DiffLineScan => vec![CandidateDraft {
                    file: "src/lib.rs".into(),
                    line: Some(10),
                    kind: ReviewFindingKind::Correctness,
                    summary: "missing error propagation".into(),
                    failure_scenario: "I/O failure is swallowed and success is reported".into(),
                }],
                ReviewAngle::RemovedBehavior => vec![CandidateDraft {
                    file: "src/lib.rs".into(),
                    line: Some(12),
                    kind: ReviewFindingKind::Correctness,
                    summary: "removed bounds guard".into(),
                    failure_scenario: "empty input reaches unchecked indexing".into(),
                }],
                ReviewAngle::CrossFileTrace => vec![CandidateDraft {
                    file: "src/lib.rs".into(),
                    line: Some(20),
                    kind: ReviewFindingKind::Correctness,
                    summary: "uncited refutation must survive".into(),
                    failure_scenario: "a verifier without citations tries to drop this".into(),
                }],
                ReviewAngle::LanguagePitfalls => vec![CandidateDraft {
                    file: "src/lib.rs".into(),
                    line: Some(11),
                    kind: ReviewFindingKind::Correctness,
                    summary: "missing error propagation".into(),
                    failure_scenario: "duplicate of first candidate".into(),
                }],
                _ => Vec::new(),
            };
            Ok(out)
        }

        async fn verify_candidate(
            &self,
            candidate: &ReviewCandidate,
            prompt: String,
        ) -> Result<ReviewVerdict> {
            self.verify_calls.lock().unwrap().push(candidate.id.clone());
            self.prompts.lock().unwrap().push(prompt);
            let verdict = if candidate.summary.contains("bounds") {
                ReviewVerdictKind::Refuted
            } else if candidate.summary.contains("uncited") {
                ReviewVerdictKind::Refuted
            } else if candidate.summary.contains("duplicated") {
                ReviewVerdictKind::Plausible
            } else {
                ReviewVerdictKind::Confirmed
            };
            let cited_line = candidate.line.unwrap_or(1);
            let citations = if candidate.summary.contains("uncited") {
                Vec::new()
            } else {
                vec![ReviewCitation {
                    file: candidate.file.clone(),
                    line: cited_line,
                    quote: candidate.summary.clone(),
                }]
            };
            Ok(ReviewVerdict {
                verdict,
                evidence: format!(
                    "{}:{} {} evidence",
                    candidate.file, cited_line, candidate.id
                ),
                citations,
            })
        }

        async fn sweep_candidates(&self, prompt: String) -> Result<Vec<CandidateDraft>> {
            self.prompts.lock().unwrap().push(prompt);
            Ok(vec![CandidateDraft {
                file: "src/extra.rs".into(),
                line: Some(44),
                kind: ReviewFindingKind::Cleanup,
                summary: "duplicated helper".into(),
                failure_scenario: "future fixes must update two branches".into(),
            }])
        }
    }

    fn init_git_repo() -> tempfile::TempDir {
        let tmp = tempfile::TempDir::new().unwrap();
        run_git(tmp.path(), &["init"]);
        run_git(tmp.path(), &["config", "user.email", "test@example.com"]);
        run_git(tmp.path(), &["config", "user.name", "Grok Test"]);
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(
            tmp.path().join("src/lib.rs"),
            "pub fn answer() -> i32 { 1 }\n",
        )
        .unwrap();
        run_git(tmp.path(), &["add", "."]);
        run_git(tmp.path(), &["commit", "-m", "initial"]);
        tmp
    }

    fn run_git(cwd: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn scope_with_prior() -> ReviewScope {
        ReviewScope {
            changed_files: vec!["src/lib.rs".into()],
            diff: "diff --git a/src/lib.rs b/src/lib.rs".into(),
            diff_path: Some("/tmp/review.patch".into()),
            prior_findings: vec![PersistedReviewFinding {
                id: "old-1".into(),
                file: "src/old.rs".into(),
                line: Some(9),
                summary: "old confirmed finding".into(),
                verdict: ReviewVerdictKind::Confirmed,
                evidence: "old evidence".into(),
                citations: vec![ReviewCitation {
                    file: "src/old.rs".into(),
                    line: 9,
                    quote: "old confirmed finding".into(),
                }],
            }],
        }
    }

    #[test]
    fn citation_validation_is_structural_and_scope_bound() {
        let candidate = ReviewCandidate {
            id: "R1-1".into(),
            angle: ReviewAngle::DiffLineScan,
            file: "src/lib.rs".into(),
            line: Some(9),
            kind: ReviewFindingKind::Correctness,
            summary: "possible bug".into(),
            failure_scenario: "bad input fails".into(),
        };
        let scope = scope_with_prior();
        let cases = [
            (
                ReviewCitation {
                    file: "src/lib.rs".into(),
                    line: 9,
                    quote: "possible bug".into(),
                },
                true,
                "candidate file with line and quote",
            ),
            (
                ReviewCitation {
                    file: "src/old.rs".into(),
                    line: 9,
                    quote: "old finding".into(),
                },
                true,
                "prior finding file is a valid re-review anchor",
            ),
            (
                ReviewCitation {
                    file: "src/lib.rs".into(),
                    line: 0,
                    quote: "possible bug".into(),
                },
                false,
                "line zero is invalid",
            ),
            (
                ReviewCitation {
                    file: "src/lib.rs".into(),
                    line: 9,
                    quote: "   ".into(),
                },
                false,
                "quote is required",
            ),
            (
                ReviewCitation {
                    file: "error.status".into(),
                    line: 404,
                    quote: "handled elsewhere".into(),
                },
                false,
                "dotted status-like keys are not in scope",
            ),
        ];

        for (citation, expected, label) in cases {
            assert_eq!(
                citation_is_valid_for_candidate(&candidate, &scope, &citation),
                expected,
                "{label}"
            );
        }
    }

    #[test]
    fn uncited_refuted_verdict_stays_alive_as_plausible() {
        let candidate = ReviewCandidate {
            id: "R1-1".into(),
            angle: ReviewAngle::DiffLineScan,
            file: "src/lib.rs".into(),
            line: Some(9),
            kind: ReviewFindingKind::Correctness,
            summary: "possible bug".into(),
            failure_scenario: "bad input fails".into(),
        };

        let verified = normalize_verified_candidate(
            &candidate,
            &scope_with_prior(),
            ReviewVerdict {
                verdict: ReviewVerdictKind::Refuted,
                evidence: String::new(),
                citations: Vec::new(),
            },
        );

        assert_eq!(verified.verdict, ReviewVerdictKind::Plausible);
        assert!(verified.evidence.contains("src/lib.rs:9"));
        assert!(
            verified
                .evidence
                .contains("REFUTED without a valid structured citation")
        );
    }

    #[test]
    fn status_code_colon_digit_is_not_line_citation() {
        let candidate = ReviewCandidate {
            id: "R1-1".into(),
            angle: ReviewAngle::DiffLineScan,
            file: "src/lib.rs".into(),
            line: Some(9),
            kind: ReviewFindingKind::Correctness,
            summary: "possible bug".into(),
            failure_scenario: "bad input fails".into(),
        };

        let verified = normalize_verified_candidate(
            &candidate,
            &scope_with_prior(),
            ReviewVerdict {
                verdict: ReviewVerdictKind::Refuted,
                evidence: "status:404 is handled elsewhere".into(),
                citations: Vec::new(),
            },
        );

        assert_eq!(verified.verdict, ReviewVerdictKind::Plausible);
        assert!(verified.evidence.contains("src/lib.rs:9"));
        assert!(
            verified
                .evidence
                .contains("status:404 is handled elsewhere")
        );
    }

    #[test]
    fn line_cited_refuted_verdict_remains_refuted() {
        let candidate = ReviewCandidate {
            id: "R1-1".into(),
            angle: ReviewAngle::DiffLineScan,
            file: "src/lib.rs".into(),
            line: Some(9),
            kind: ReviewFindingKind::Correctness,
            summary: "possible bug".into(),
            failure_scenario: "bad input fails".into(),
        };

        let verified = normalize_verified_candidate(
            &candidate,
            &scope_with_prior(),
            ReviewVerdict {
                verdict: ReviewVerdictKind::Refuted,
                evidence: "src/lib.rs:9 proves the guard exists".into(),
                citations: vec![ReviewCitation {
                    file: "src/lib.rs".into(),
                    line: 9,
                    quote: "guard exists".into(),
                }],
            },
        );

        assert_eq!(verified.verdict, ReviewVerdictKind::Refuted);
        assert_eq!(verified.evidence, "src/lib.rs:9 proves the guard exists");
        assert_eq!(verified.citations.len(), 1);
    }

    #[tokio::test]
    async fn review_pipeline_runs_angles_dedups_verifies_sweeps_and_excludes_refuted() {
        let agent = Arc::new(MockReviewAgent::default());
        let report = run_review_pipeline(
            agent.as_ref(),
            ReviewPipelineInput {
                objective: "review goal",
                attempt: 1,
                effort: ReviewEffort::Max,
                scope: scope_with_prior(),
            },
        )
        .await
        .unwrap();

        let finder_calls = agent.finder_calls.lock().unwrap().clone();
        assert!(finder_calls.contains(&ReviewAngle::DiffLineScan));
        assert!(finder_calls.contains(&ReviewAngle::RemovedBehavior));
        assert!(finder_calls.len() >= 2);
        assert_eq!(report.stats.candidate_count, 4);
        assert_eq!(
            report.stats.deduped_candidate_count, 4,
            "three finder candidates plus one sweep candidate survive dedup"
        );
        assert_eq!(report.stats.verified_count, 4);
        assert_eq!(report.stats.confirmed_count, 1);
        assert_eq!(
            report.stats.plausible_count, 2,
            "sweep cleanup and uncited refutation both survive as plausible"
        );
        assert_eq!(report.stats.refuted_count, 1);
        assert!(
            report
                .findings
                .iter()
                .all(|f| f.verdict != ReviewVerdictKind::Refuted)
        );
        assert!(
            report
                .refuted
                .iter()
                .any(|f| f.candidate.summary.contains("bounds"))
        );
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.candidate.summary.contains("uncited refutation")
                    && f.verdict == ReviewVerdictKind::Plausible),
            "uncited REFUTED verifier output must not exclude the candidate"
        );
        assert!(report.evidence_md.contains("old confirmed finding"));
    }

    #[tokio::test]
    async fn post_change_review_captures_real_diff_and_persists_artifacts_when_enabled() {
        let repo = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(repo.path().join("src")).unwrap();
        std::fs::write(
            repo.path().join("src/lib.rs"),
            "pub fn answer() -> i32 { 2 }\n",
        )
        .unwrap();
        let review_dir = repo.path().join(".session/goal/review");
        let agent = MockReviewAgent::default();

        let (report, paths) = run_post_change_review(
            &agent,
            true,
            "review changed code",
            1,
            ReviewEffort::High,
            None,
            repo.path(),
            chrono::Utc::now().timestamp() - 1,
            &review_dir,
        )
        .await
        .unwrap();

        assert!(report.stats.enabled);
        assert_eq!(report.stats.changed_file_count, 1);
        assert!(report.evidence_md.contains("src/lib.rs"));
        assert!(paths.findings.is_file());
        assert!(paths.refuted.is_file());
        assert!(paths.stats.is_file());
        assert!(paths.evidence.is_file());
        assert!(review_dir.join("changes.patch").is_file());
        assert!(
            !agent.finder_calls.lock().unwrap().is_empty(),
            "enabled post-change review should invoke finder angles"
        );
    }

    #[tokio::test]
    async fn post_change_review_skips_when_disabled_without_invoking_finders() {
        let repo = init_git_repo();
        let baseline = run_git(repo.path(), &["rev-parse", "HEAD"]);
        std::fs::write(
            repo.path().join("src/lib.rs"),
            "pub fn answer() -> i32 { 3 }\n",
        )
        .unwrap();
        let review_dir = repo.path().join(".session/goal/review");
        let agent = MockReviewAgent::default();

        let (report, paths) = run_post_change_review(
            &agent,
            false,
            "review changed code",
            1,
            ReviewEffort::High,
            Some(&baseline),
            repo.path(),
            0,
            &review_dir,
        )
        .await
        .unwrap();

        assert_eq!(report.stats.skipped_reason.as_deref(), Some("disabled"));
        assert!(agent.finder_calls.lock().unwrap().is_empty());
        let stats: ReviewStats =
            serde_json::from_str(&tokio::fs::read_to_string(paths.stats).await.unwrap()).unwrap();
        assert_eq!(stats.skipped_reason.as_deref(), Some("disabled"));
    }

    #[tokio::test]
    async fn post_change_review_skips_when_real_diff_has_no_code_changes() {
        let repo = init_git_repo();
        let baseline = run_git(repo.path(), &["rev-parse", "HEAD"]);
        let review_dir = repo.path().join(".session/goal/review");
        let agent = MockReviewAgent::default();

        let (report, _paths) = run_post_change_review(
            &agent,
            true,
            "review changed code",
            1,
            ReviewEffort::High,
            Some(&baseline),
            repo.path(),
            0,
            &review_dir,
        )
        .await
        .unwrap();

        assert_eq!(
            report.stats.skipped_reason.as_deref(),
            Some("no_code_changes")
        );
        assert!(agent.finder_calls.lock().unwrap().is_empty());
    }

    #[test]
    fn confirmed_review_findings_build_completion_block() {
        let report = ReviewReport {
            findings: vec![VerifiedReviewFinding {
                candidate: ReviewCandidate {
                    id: "R1-1".into(),
                    angle: ReviewAngle::DiffLineScan,
                    file: "src/lib.rs".into(),
                    line: Some(7),
                    kind: ReviewFindingKind::Correctness,
                    summary: "confirmed bug".into(),
                    failure_scenario: "input x fails".into(),
                },
                verdict: ReviewVerdictKind::Confirmed,
                evidence: "src/lib.rs:7".into(),
                citations: vec![ReviewCitation {
                    file: "src/lib.rs".into(),
                    line: 7,
                    quote: "confirmed bug".into(),
                }],
            }],
            refuted: Vec::new(),
            stats: ReviewStats {
                enabled: true,
                confirmed_count: 1,
                ..Default::default()
            },
            evidence_md: String::new(),
        };
        let paths = ReviewArtifactPaths {
            findings: PathBuf::from("/session/goal/review/findings.json"),
            refuted: PathBuf::from("/session/goal/review/refuted.json"),
            stats: PathBuf::from("/session/goal/review/stats.json"),
            evidence: PathBuf::from("/session/goal/review/evidence.md"),
        };

        let block = completion_block_for_report(&report, &paths).unwrap();

        assert!(block.details_path.ends_with("review/evidence.md"));
        assert!(block.gaps_summary.contains("confirmed bug"));
        assert!(
            block
                .pause_summary
                .contains("Automatic review found 1 confirmed")
        );
        assert!(!block.gap_fingerprint.is_empty());
    }

    #[test]
    fn plausible_only_review_findings_do_not_block_completion() {
        let report = ReviewReport {
            findings: vec![VerifiedReviewFinding {
                candidate: ReviewCandidate {
                    id: "R1-1".into(),
                    angle: ReviewAngle::CleanupAltitude,
                    file: "src/lib.rs".into(),
                    line: None,
                    kind: ReviewFindingKind::Cleanup,
                    summary: "plausible cleanup".into(),
                    failure_scenario: "maintainer must touch duplicate code".into(),
                },
                verdict: ReviewVerdictKind::Plausible,
                evidence: "could be simplified".into(),
                citations: Vec::new(),
            }],
            refuted: Vec::new(),
            stats: ReviewStats {
                enabled: true,
                plausible_count: 1,
                ..Default::default()
            },
            evidence_md: String::new(),
        };
        let paths = ReviewArtifactPaths {
            findings: PathBuf::from("findings.json"),
            refuted: PathBuf::from("refuted.json"),
            stats: PathBuf::from("stats.json"),
            evidence: PathBuf::from("evidence.md"),
        };

        assert!(completion_block_for_report(&report, &paths).is_none());
    }

    #[tokio::test]
    async fn review_artifacts_persist_and_reload_prior_findings() {
        let tmp = tempfile::TempDir::new().unwrap();
        let report = ReviewReport {
            findings: vec![VerifiedReviewFinding {
                candidate: ReviewCandidate {
                    id: "R1-1".into(),
                    angle: ReviewAngle::DiffLineScan,
                    file: "src/lib.rs".into(),
                    line: Some(7),
                    kind: ReviewFindingKind::Correctness,
                    summary: "confirmed bug".into(),
                    failure_scenario: "input x fails".into(),
                },
                verdict: ReviewVerdictKind::Confirmed,
                evidence: "src/lib.rs:7".into(),
                citations: vec![ReviewCitation {
                    file: "src/lib.rs".into(),
                    line: 7,
                    quote: "confirmed bug".into(),
                }],
            }],
            refuted: vec![VerifiedReviewFinding {
                candidate: ReviewCandidate {
                    id: "R1-2".into(),
                    angle: ReviewAngle::RemovedBehavior,
                    file: "src/lib.rs".into(),
                    line: Some(8),
                    kind: ReviewFindingKind::Correctness,
                    summary: "refuted bug".into(),
                    failure_scenario: "not actually reachable".into(),
                },
                verdict: ReviewVerdictKind::Refuted,
                evidence: "guarded elsewhere".into(),
                citations: vec![ReviewCitation {
                    file: "src/lib.rs".into(),
                    line: 8,
                    quote: "guarded elsewhere".into(),
                }],
            }],
            stats: ReviewStats {
                enabled: true,
                changed_file_count: 1,
                finder_count: 2,
                candidate_count: 2,
                deduped_candidate_count: 2,
                verified_count: 2,
                confirmed_count: 1,
                plausible_count: 0,
                refuted_count: 1,
                sweep_candidate_count: 0,
                skipped_reason: None,
            },
            evidence_md: "# Automatic goal review\n".into(),
        };

        let paths = persist_review_report(tmp.path(), &report).await.unwrap();

        assert!(paths.findings.is_file());
        assert!(paths.refuted.is_file());
        assert!(paths.stats.is_file());
        assert!(paths.evidence.is_file());
        let prior = load_prior_findings(tmp.path()).await;
        assert_eq!(prior.len(), 1);
        assert_eq!(prior[0].summary, "confirmed bug");
        let stats: ReviewStats =
            serde_json::from_str(&tokio::fs::read_to_string(paths.stats).await.unwrap()).unwrap();
        assert_eq!(stats.confirmed_count, 1);
    }
}
