//! A deterministic pre-check that runs before the skeptic panel.
//!
//! The panel is N language models reading a diff and voting on whether a goal
//! was achieved. For anything the session can look up, that is strictly worse
//! than the lookup: it costs N spawns, it is non-deterministic, and it can be
//! argued out of a correct verdict.
//!
//! This gate handles exactly one claim, because it is the only one currently
//! adjudicable without ambiguity: **a code-change goal whose workspace is
//! byte-identical to where it started cannot have been achieved.** No amount of
//! reasoning makes an empty diff into a code change.
//!
//! # Fail-open by construction
//!
//! A false `Refuted` is far worse than a wasted panel: it blocks a goal that
//! genuinely completed, and the user has no way to argue with a hardcoded
//! check. So every uncertainty resolves to [`Inconclusive`] and defers to the
//! panel — a missing baseline, an unreadable plan, a git invocation that fails
//! or times out, a goal whose kind is not `code-change`. The gate only speaks
//! when it is certain, and its certainty comes from an absence, not a judgement.
//!
//! [`Inconclusive`]: DeterministicVerdict::Inconclusive
//!
//! # Untracked files count as changes
//!
//! `git diff` does not see a file that was never added, so a goal whose entire
//! output is new files would look unchanged to a diff-only probe. That is the
//! obvious way this gate would false-reject, so the probe checks
//! `status --porcelain`, which reports untracked entries too.

use std::path::Path;
use std::time::Duration;

use super::goal_classifier::GoalKind;

/// The probe gets a short budget. It runs on the hot path before verification,
/// and a slow git is a reason to defer to the panel, not to stall the goal.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// What the deterministic check concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeterministicVerdict {
    /// Nothing can be concluded without judgement. Run the panel.
    Inconclusive,
    /// The goal provably was not achieved. Skip the panel.
    Refuted {
        /// Stable machine reason, also used as the rejection headline.
        reason: &'static str,
    },
}

/// The reason string for the only refutation this gate issues.
pub(crate) const REASON_NO_WORKSPACE_CHANGE: &str = "code_change_goal_with_no_workspace_change";

/// Decide from already-gathered facts.
///
/// Split from the git probe so the decision is unit-testable without a
/// repository, and so every fail-open branch is visible in one place rather
/// than scattered through I/O error handling.
///
/// `changed` is `None` when the probe could not determine the answer — which
/// must defer, not refute.
pub(crate) fn decide(kind: Option<GoalKind>, changed: Option<bool>) -> DeterministicVerdict {
    match (kind, changed) {
        // The only refutable case: a goal that is definitionally about changing
        // code, whose workspace provably did not change.
        (Some(GoalKind::CodeChange), Some(false)) => DeterministicVerdict::Refuted {
            reason: REASON_NO_WORKSPACE_CHANGE,
        },
        // An analysis or research goal can be achieved with no diff at all —
        // the deliverable is a conclusion, not an edit.
        _ => DeterministicVerdict::Inconclusive,
    }
}

/// Whether the workspace differs from `baseline_commit`.
///
/// `None` on any uncertainty: no baseline recorded, git missing, non-zero exit,
/// or the probe exceeding its budget. Every one of those defers to the panel.
pub(crate) async fn workspace_changed(
    workspace_root: &Path,
    baseline_commit: Option<&str>,
) -> Option<bool> {
    let baseline = baseline_commit?.trim();
    if baseline.is_empty() {
        return None;
    }

    // `status --porcelain` rather than `diff`: a goal whose whole output is new
    // files has an empty diff but a non-empty status, and refuting that would
    // be the gate's most likely false positive.
    if untracked_or_modified(workspace_root).await? {
        return Some(true);
    }
    // Nothing uncommitted; the work may still have been committed since the
    // baseline was taken.
    committed_since(workspace_root, baseline).await
}

async fn untracked_or_modified(workspace_root: &Path) -> Option<bool> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("status")
        .arg("--porcelain")
        .current_dir(workspace_root);
    run_probe(cmd).await.map(|out| !out.trim().is_empty())
}

async fn committed_since(workspace_root: &Path, baseline: &str) -> Option<bool> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("diff")
        .arg("--name-only")
        .arg(baseline)
        .arg("HEAD")
        .current_dir(workspace_root);
    run_probe(cmd).await.map(|out| !out.trim().is_empty())
}

/// Run one git probe, returning stdout only on a clean exit inside the budget.
async fn run_probe(mut cmd: tokio::process::Command) -> Option<String> {
    cmd.stdin(std::process::Stdio::null());
    match tokio::time::timeout(PROBE_TIMEOUT, cmd.output()).await {
        Ok(Ok(output)) if output.status.success() => {
            Some(String::from_utf8_lossy(&output.stdout).into_owned())
        }
        Ok(Ok(output)) => {
            tracing::debug!(status = ?output.status, "deterministic gate: git probe failed");
            None
        }
        Ok(Err(err)) => {
            tracing::debug!(error = %err, "deterministic gate: could not spawn git");
            None
        }
        Err(_) => {
            tracing::debug!("deterministic gate: git probe exceeded its budget");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one case the gate exists for.
    #[test]
    fn a_code_change_goal_with_no_change_is_refuted() {
        assert_eq!(
            decide(Some(GoalKind::CodeChange), Some(false)),
            DeterministicVerdict::Refuted {
                reason: REASON_NO_WORKSPACE_CHANGE,
            },
        );
    }

    /// A code-change goal that changed something needs judgement about WHETHER
    /// the change achieves the goal — which is exactly what the panel is for.
    #[test]
    fn a_code_change_goal_with_changes_defers_to_the_panel() {
        assert_eq!(
            decide(Some(GoalKind::CodeChange), Some(true)),
            DeterministicVerdict::Inconclusive,
        );
    }

    /// Analysis and research goals deliver a conclusion, not an edit. Refuting
    /// them on an empty diff would reject correct work.
    #[test]
    fn non_code_change_goals_are_never_refuted_on_an_empty_diff() {
        for kind in [GoalKind::Analysis, GoalKind::Research] {
            assert_eq!(
                decide(Some(kind), Some(false)),
                DeterministicVerdict::Inconclusive,
                "{kind:?} can be achieved with no diff",
            );
        }
    }

    /// Every uncertainty defers. A false refutation blocks a completed goal and
    /// the user cannot argue with a hardcoded check, so the gate must only
    /// speak when it is certain.
    #[test]
    fn every_uncertain_input_fails_open() {
        // Unknown goal kind (unreadable or unparseable plan).
        assert_eq!(
            decide(None, Some(false)),
            DeterministicVerdict::Inconclusive,
        );
        // Probe could not determine the answer.
        assert_eq!(
            decide(Some(GoalKind::CodeChange), None),
            DeterministicVerdict::Inconclusive,
        );
        // Both unknown.
        assert_eq!(decide(None, None), DeterministicVerdict::Inconclusive);
    }

    /// The probe defers rather than guessing when there is no baseline to
    /// compare against.
    #[tokio::test]
    async fn a_missing_or_blank_baseline_yields_no_answer() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(workspace_changed(dir.path(), None).await, None);
        assert_eq!(workspace_changed(dir.path(), Some("   ")).await, None);
    }

    /// A directory that is not a repository makes git exit non-zero, which must
    /// read as "cannot tell", not as "nothing changed".
    #[tokio::test]
    async fn a_non_repository_yields_no_answer_rather_than_false() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            workspace_changed(dir.path(), Some("deadbeef")).await,
            None,
            "a failed probe must not be read as an empty diff",
        );
    }

    /// End to end against a real repository: an untouched tree reports no
    /// change, and an UNTRACKED file counts as a change. The untracked case is
    /// the gate's most likely false positive, since `git diff` cannot see it.
    #[tokio::test]
    async fn an_untracked_file_counts_as_a_workspace_change() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(root)
                .output()
                .expect("git")
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(root.join("seed.txt"), "seed").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "seed"]);
        let head = String::from_utf8_lossy(&git(&["rev-parse", "HEAD"]).stdout)
            .trim()
            .to_owned();

        assert_eq!(
            workspace_changed(root, Some(&head)).await,
            Some(false),
            "an untouched tree must report no change",
        );
        assert_eq!(
            decide(Some(GoalKind::CodeChange), Some(false)),
            DeterministicVerdict::Refuted {
                reason: REASON_NO_WORKSPACE_CHANGE,
            },
        );

        // A brand-new file git has never seen: invisible to `diff`, visible to
        // `status --porcelain`.
        std::fs::write(root.join("brand_new.rs").as_path(), "fn main() {}").unwrap();
        assert_eq!(
            workspace_changed(root, Some(&head)).await,
            Some(true),
            "an untracked file is a workspace change; missing it would reject \
             a goal whose whole output is new files",
        );
    }

    /// Work that was COMMITTED since the baseline leaves a clean status but is
    /// still a change. Checking only `status` would refute a goal that
    /// committed its work.
    #[tokio::test]
    async fn work_committed_since_the_baseline_counts_as_a_change() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(root)
                .output()
                .expect("git")
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(root.join("seed.txt"), "seed").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "seed"]);
        let baseline = String::from_utf8_lossy(&git(&["rev-parse", "HEAD"]).stdout)
            .trim()
            .to_owned();

        std::fs::write(root.join("added.rs"), "fn added() {}").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "work"]);

        assert!(
            String::from_utf8_lossy(&git(&["status", "--porcelain"]).stdout)
                .trim()
                .is_empty(),
            "precondition: committing leaves a clean status",
        );
        assert_eq!(
            workspace_changed(root, Some(&baseline)).await,
            Some(true),
            "committed work is still a change against the baseline",
        );
    }
}
