//! Promote the plan that achieved a goal into the Brain's procedural memory.
//!
//! The Brain has held a `memory_procedure` table and a recall API for a while,
//! and nothing ever wrote to it — `procedures: 0` in `/brain status` for every
//! real user. This is the writer.
//!
//! # Why here rather than in `GoalTracker`
//!
//! `GoalTracker` is a pure state machine over an orchestration record. Giving
//! it a `BrainStore` would make every tracker transition a database write and
//! couple a lifecycle type to storage. The session layer already reaches the
//! Brain (`acp_session_impl::brain`), so promotion lives here and the tracker
//! stays what it is.
//!
//! # Best-effort by construction
//!
//! Every failure path returns quietly. A goal that genuinely succeeded must not
//! be reported as failed because a memory write did not land — the Brain is an
//! optimisation over re-planning, never a correctness dependency. Failures are
//! traced, not surfaced.
//!
//! # Idempotent, which is what makes the call sites safe
//!
//! `record_procedure` is an upsert against a unique identity index, so calling
//! this more than once for one goal bumps `uses` rather than duplicating. That
//! matters because `tracker.complete()` has four call sites with different
//! surrounding semantics and no chokepoint; a helper that had to be called
//! exactly once would be a latent bug waiting for a fifth branch.

use std::path::Path;

/// Largest plan we will store. A plan is a procedure, not an archive; past a
/// point it is a document that happens to have been written during a goal, and
/// storing it wastes recall budget on something no one will reuse verbatim.
const MAX_PROMOTED_PLAN_BYTES: usize = 32 * 1024;

/// Record `objective -> plan` as a procedure that worked.
///
/// No-ops when there is no plan file, when the plan is empty or oversized, or
/// when the Brain cannot be opened. Returns whether a procedure was recorded,
/// which exists for tests — no caller branches on it.
pub(crate) fn promote_goal_procedure(
    objective: &str,
    plan_file: Option<&Path>,
    workspace: Option<&str>,
) -> bool {
    promote_into(
        &xai_grok_brain::default_store_path(),
        objective,
        plan_file,
        workspace,
    )
}

/// Promote from a tracker that is about to complete.
///
/// Called at every `tracker.complete()` site. Reads the snapshot BEFORE the
/// transition, because `complete()` removes the goal's scratch root and resets
/// derived state; taking the objective and plan path first keeps the read
/// unambiguous regardless of what the transition clears next.
pub(crate) fn promote_from_tracker(tracker: &crate::session::goal_tracker::GoalTracker) {
    promote_from_tracker_into(&xai_grok_brain::default_store_path(), tracker);
}

/// [`promote_from_tracker`] against an explicit store.
///
/// Split out so the tracker-reading half is testable without touching the
/// user's real Brain. Without this the wiring was untestable, and gutting it
/// left the whole suite green — the same computed-and-dropped shape a review
/// lane already caught once in this change set.
pub(crate) fn promote_from_tracker_into(
    store_path: &Path,
    tracker: &crate::session::goal_tracker::GoalTracker,
) -> bool {
    let Some(orchestration) = tracker.snapshot() else {
        return false;
    };
    promote_into(
        store_path,
        &orchestration.objective,
        orchestration.plan_file.as_deref(),
        None,
    )
}

/// [`promote_goal_procedure`] against an explicit store, so tests never touch
/// the user's real Brain.
pub(crate) fn promote_into(
    store_path: &Path,
    objective: &str,
    plan_file: Option<&Path>,
    workspace: Option<&str>,
) -> bool {
    let objective = objective.trim();
    if objective.is_empty() {
        return false;
    }
    let Some(plan_file) = plan_file else {
        // A goal completed without a planner run has no procedure to teach.
        return false;
    };
    let Ok(plan) = std::fs::read_to_string(plan_file) else {
        return false;
    };
    let plan = plan.trim();
    if plan.is_empty() || plan.len() > MAX_PROMOTED_PLAN_BYTES {
        return false;
    }

    let service = match xai_grok_brain::BrainService::open(store_path) {
        Ok(service) => service,
        Err(err) => {
            tracing::debug!(error = %err, "brain unavailable; skipping procedure promotion");
            return false;
        }
    };
    match service.store().record_procedure(
        objective,
        plan,
        xai_grok_brain::ProcedureOutcome::Succeeded,
        workspace,
    ) {
        Ok(_) => true,
        Err(err) => {
            tracing::debug!(error = %err, "failed to promote goal procedure");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(dir: &Path, body: &str) -> std::path::PathBuf {
        let path = dir.join("plan.md");
        std::fs::write(&path, body).unwrap();
        path
    }

    /// The WIRING, not just the helper. `promote_from_tracker` is what the
    /// four `tracker.complete()` sites call; without a test on it, gutting the
    /// body left every other test green.
    #[test]
    fn promoting_from_a_tracker_reads_the_objective_and_plan_file() {
        use crate::session::goal_tracker::GoalTracker;

        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("brain.sqlite3");
        let plan_file = plan(dir.path(), "1. reproduce 2. fix 3. regression test");

        let mut tracker = GoalTracker::new(dir.path().to_path_buf());
        tracker.create_goal(
            "goal-1".into(),
            "fix the flaky reconnect test".into(),
            None,
            0,
            "2026-01-01T00:00:00Z".into(),
            None,
        );
        tracker.snapshot_mut().unwrap().plan_file = Some(plan_file.clone());

        assert!(promote_from_tracker_into(&db, &tracker));

        let service = xai_grok_brain::BrainService::open(&db).unwrap();
        let recalled = service
            .store()
            .recall_procedures(&xai_grok_brain::ProcedureRecallOptions::new(
                "fix the flaky reconnect test",
            ))
            .unwrap();
        assert_eq!(
            recalled.len(),
            1,
            "the tracker's objective keys the procedure"
        );
        assert!(
            recalled[0].procedure.plan.contains("regression test"),
            "and the tracker's plan_file supplies the plan",
        );
    }

    /// A tracker with no active goal has nothing to promote.
    #[test]
    fn promoting_from_an_inactive_tracker_is_a_no_op() {
        use crate::session::goal_tracker::GoalTracker;

        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("brain.sqlite3");
        let tracker = GoalTracker::new(dir.path().to_path_buf());
        assert!(!promote_from_tracker_into(&db, &tracker));
        assert!(!db.exists());
    }

    #[test]
    fn a_completed_goal_promotes_its_plan_and_becomes_recallable() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("brain.sqlite3");
        let plan_file = plan(dir.path(), "1. read the parser\n2. add the case\n3. test");

        assert!(promote_into(
            &db,
            "add a new token kind to the parser",
            Some(&plan_file),
            None,
        ));

        let service = xai_grok_brain::BrainService::open(&db).unwrap();
        let recalled = service
            .store()
            .recall_procedures(&xai_grok_brain::ProcedureRecallOptions::new(
                "add a new token kind to the parser",
            ))
            .unwrap();
        assert_eq!(recalled.len(), 1, "the plan that worked must be recallable");
        assert!(recalled[0].procedure.plan.contains("add the case"));
        assert_eq!(recalled[0].procedure.uses, 1);
    }

    /// The four `tracker.complete()` sites have no chokepoint, so promotion
    /// must be safe to call more than once for one goal.
    #[test]
    fn promoting_the_same_goal_twice_accrues_evidence_rather_than_duplicating() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("brain.sqlite3");
        let plan_file = plan(dir.path(), "the plan");

        assert!(promote_into(&db, "recurring goal", Some(&plan_file), None));
        assert!(promote_into(&db, "recurring goal", Some(&plan_file), None));

        let service = xai_grok_brain::BrainService::open(&db).unwrap();
        assert_eq!(service.store().procedure_count().unwrap(), 1);
        let recalled = service
            .store()
            .recall_procedures(&xai_grok_brain::ProcedureRecallOptions::new(
                "recurring goal",
            ))
            .unwrap();
        assert_eq!(
            recalled[0].procedure.uses, 2,
            "a plan that keeps working accrues evidence",
        );
    }

    #[test]
    fn a_goal_without_a_plan_file_promotes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("brain.sqlite3");
        assert!(!promote_into(&db, "no planner ran", None, None));
        // The store is not even created, so nothing was written.
        assert!(!db.exists());
    }

    #[test]
    fn an_empty_or_missing_plan_promotes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("brain.sqlite3");
        let empty = plan(dir.path(), "   \n  ");
        assert!(!promote_into(&db, "goal", Some(&empty), None));
        assert!(!promote_into(
            &db,
            "goal",
            Some(&dir.path().join("absent.md")),
            None,
        ));
    }

    #[test]
    fn an_empty_objective_promotes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("brain.sqlite3");
        let plan_file = plan(dir.path(), "the plan");
        assert!(!promote_into(&db, "   ", Some(&plan_file), None));
    }

    /// A plan past the cap is a document, not a procedure. Storing it would
    /// spend recall budget on something nobody reuses verbatim.
    #[test]
    fn an_oversized_plan_is_not_promoted() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("brain.sqlite3");
        let huge = plan(dir.path(), &"x".repeat(MAX_PROMOTED_PLAN_BYTES + 1));
        assert!(!promote_into(&db, "goal", Some(&huge), None));

        let just_under = plan(dir.path(), &"y".repeat(MAX_PROMOTED_PLAN_BYTES));
        assert!(
            promote_into(&db, "goal", Some(&just_under), None),
            "the cap is a ceiling, not an off switch",
        );
    }

    /// Promotion is best-effort: a broken store must never make a goal that
    /// genuinely succeeded look like it failed.
    #[test]
    fn an_unusable_store_fails_quietly_rather_than_propagating() {
        let dir = tempfile::tempdir().unwrap();
        // A directory where the database file should be: open must fail.
        let db = dir.path().join("not-a-db");
        std::fs::create_dir(&db).unwrap();
        let plan_file = plan(dir.path(), "the plan");
        assert!(
            !promote_into(&db, "goal", Some(&plan_file), None),
            "a store failure reports false and does not panic or propagate",
        );
    }

    /// Workspace scoping carries through, so one checkout's procedures do not
    /// surface in another's recall.
    #[test]
    fn the_workspace_scope_is_carried_through() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("brain.sqlite3");
        let plan_file = plan(dir.path(), "scoped plan");
        assert!(promote_into(
            &db,
            "scoped goal",
            Some(&plan_file),
            Some("/repo/a"),
        ));

        let service = xai_grok_brain::BrainService::open(&db).unwrap();
        let elsewhere = service
            .store()
            .recall_procedures(
                &xai_grok_brain::ProcedureRecallOptions::new("scoped goal")
                    .scope_id(Some("/repo/b".to_owned())),
            )
            .unwrap();
        assert!(
            elsewhere.is_empty(),
            "another checkout must not see this workspace's procedure",
        );
        let here = service
            .store()
            .recall_procedures(
                &xai_grok_brain::ProcedureRecallOptions::new("scoped goal")
                    .scope_id(Some("/repo/a".to_owned())),
            )
            .unwrap();
        assert_eq!(here.len(), 1);
    }
}
