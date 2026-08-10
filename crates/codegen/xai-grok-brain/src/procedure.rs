//! Procedural memory: plans that worked, recalled for goals that resemble the
//! one they solved.
//!
//! The Brain already holds *semantic* memory (pages: facts about the project)
//! and the session log holds *episodic* memory (what happened). Neither stores
//! **how a task was accomplished**, so a recurring goal is re-planned from
//! scratch every time, at full cost, with no guarantee the second plan is as
//! good as the first.
//!
//! # Why a separate table rather than a fifth category
//!
//! [`MemoryCategory`](crate::MemoryCategory) is an Onyx-compatible wire enum
//! (`notes` / `concepts` / `entities` / `workstreams`). Adding a `procedures`
//! variant would emit a category value Onyx does not know, so a page written
//! here could be coerced on round-trip. A procedure is also shaped differently
//! from a page — it is keyed by the goal it solves and carries an outcome — so
//! overloading `memory` would mean nullable columns that only ever apply to one
//! kind of row. `memory_procedure` keeps the wire contract untouched.
//!
//! # Recall is lexical on purpose
//!
//! Similarity is Jaccard overlap of normalized signature tokens: no embedding
//! provider, no network, no API key, and identical results on every machine.
//! The Brain's *page* search already has an embedding path with a lexical
//! fallback; procedures deliberately start at the fallback tier, because a
//! wrong procedure is worse than no procedure — it sends the agent down a plan
//! built for a different problem. A semantic tier can be layered on later
//! behind the same API once there is evidence lexical recall is the limit.

use crate::Result;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use std::collections::HashSet;

/// How a recorded plan ended.
///
/// Only [`Self::Succeeded`] is recallable. A failed plan is still recorded so
/// the same dead end can be recognised rather than silently retried, but it is
/// never offered as something to reuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcedureOutcome {
    Succeeded,
    Failed,
}

impl ProcedureOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }

    /// Parse a stored value. Anything that is not byte-exactly what
    /// [`Self::as_str`] writes reads as [`Self::Failed`] — the conservative
    /// direction, since an unreadable outcome must not make a plan look
    /// reusable.
    ///
    /// Deliberately NOT case-tolerant. Recall filters in SQL with
    /// `outcome = 'succeeded'`, which is byte-exact and cannot be made
    /// case-insensitive without also widening what counts as reusable. A
    /// lenient parser next to a strict filter means the two readers of this
    /// column disagree: `get_procedure` would report a row stored as
    /// `"Succeeded"` as a success while `recall_procedures` silently excluded
    /// it. `as_str` is the only writer, so any other spelling is foreign to
    /// this store and both readers now agree to distrust it.
    pub fn parse(value: &str) -> Self {
        match value {
            "succeeded" => Self::Succeeded,
            _ => Self::Failed,
        }
    }
}

/// A plan that was run, with the goal it was run for and how it ended.
#[derive(Debug, Clone)]
pub struct Procedure {
    pub id: i64,
    /// The goal text this plan addressed, as given.
    pub signature: String,
    /// The plan itself, in whatever form the caller stores (steps, JSON, prose).
    pub plan: String,
    pub outcome: ProcedureOutcome,
    /// Optional workspace scope, so one machine's procedures do not leak across
    /// unrelated checkouts.
    pub scope_id: Option<String>,
    /// How many times this signature+plan pair has been recorded **as a
    /// success**. Repeated success is the only durable evidence a plan
    /// generalises rather than having worked once by luck.
    ///
    /// Failed re-records deliberately do NOT increment it. Recall ranks on this
    /// field, so counting every record would let a plan that failed five times
    /// and succeeded once outrank a plan that has only ever succeeded — the
    /// exact inversion of what the field is read to mean.
    pub uses: i64,
    /// How many times this pair has been recorded as a FAILURE.
    ///
    /// Ranked on ahead of recency: two plans with one success each are not
    /// equally trustworthy when one of them also failed five times, and
    /// without this the more recently touched row wins on a coin flip.
    pub failures: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Procedure {
    /// Laplace-smoothed success rate in `(0, 1)`.
    ///
    /// Smoothed so a single lucky success does not score a perfect 1.0 and
    /// outrank a plan with a long, nearly-clean record: one success scores
    /// 0.75 while nine successes and one failure score 0.86. Unsmoothed rates
    /// would make the sparsest evidence look strongest, which is the opposite
    /// of what the ranking is for.
    pub fn reliability(&self) -> f64 {
        // Clamped because the columns carry no CHECK constraint: a corrupted
        // negative pair sums to -1 and divides by zero, and `+inf` sorts ahead
        // of every honest record under `total_cmp`.
        let uses = self.uses.max(0) as f64;
        let failures = self.failures.max(0) as f64;
        (uses + 0.5) / (uses + failures + 1.0)
    }
}

/// A recalled procedure and how well its signature matched the query.
#[derive(Debug, Clone)]
pub struct RecalledProcedure {
    pub procedure: Procedure,
    /// Jaccard overlap in `[0, 1]`; 1.0 is an exact token-set match.
    pub similarity: f64,
}

/// Default for [`ProcedureRecallOptions::max_candidates`]. Large enough that a
/// realistic store is never truncated, small enough to bound one recall's
/// memory regardless of how big the table grows.
const DEFAULT_MAX_RECALL_CANDIDATES: usize = 512;

/// Options for [`crate::BrainStore::recall_procedures`].
#[derive(Debug, Clone)]
pub struct ProcedureRecallOptions {
    pub goal: String,
    pub scope_id: Option<String>,
    pub limit: usize,
    /// Minimum similarity to return at all.
    ///
    /// Non-zero by default and deliberately so: recall that returns the
    /// best-of-a-bad-set hands the agent a plan for a different problem, which
    /// is worse than returning nothing and letting it plan fresh.
    pub min_similarity: f64,
    /// Hard cap on how many rows are pulled out of SQLite before ranking.
    ///
    /// Similarity is Jaccard over token sets, which SQLite cannot express, so
    /// ranking happens in Rust and every candidate must be materialised first —
    /// signature and plan strings included. Scope narrows VISIBILITY, not
    /// cardinality, so without this a store with many successful procedures in
    /// one workspace loads all of them on every recall.
    ///
    /// The cap takes the most recently updated rows, so the cut is deterministic
    /// rather than arbitrary. The cost is real and worth stating: a very old
    /// procedure can fall outside the window even if it would have scored
    /// highest. Raise it when recall quality matters more than the load.
    pub max_candidates: usize,
}

impl ProcedureRecallOptions {
    pub fn new(goal: impl Into<String>) -> Self {
        Self {
            goal: goal.into(),
            scope_id: None,
            limit: 3,
            min_similarity: 0.2,
            max_candidates: DEFAULT_MAX_RECALL_CANDIDATES,
        }
    }

    #[must_use]
    pub fn scope_id(mut self, scope_id: Option<String>) -> Self {
        self.scope_id = scope_id;
        self
    }

    #[must_use]
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    #[must_use]
    pub fn min_similarity(mut self, min_similarity: f64) -> Self {
        self.min_similarity = min_similarity;
        self
    }

    #[must_use]
    pub fn max_candidates(mut self, max_candidates: usize) -> Self {
        self.max_candidates = max_candidates;
        self
    }
}

/// Lowercased, punctuation-stripped token set used for signature comparison.
///
/// A set rather than a bag: repeating a word in a goal statement says nothing
/// about which stored plan fits it.
pub(crate) fn signature_tokens(value: &str) -> HashSet<String> {
    value
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

/// Jaccard overlap of two token sets. Two empty signatures score 0, not 1 —
/// an empty goal matches nothing rather than matching everything.
pub(crate) fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    // Both sets are non-empty after the guard above, so the union is at least 1
    // and no zero-division branch is reachable.
    let intersection = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    intersection / union
}

pub(crate) fn row_to_procedure(row: &rusqlite::Row<'_>) -> rusqlite::Result<Procedure> {
    let created: String = row.get(5)?;
    let updated: String = row.get(6)?;
    Ok(Procedure {
        id: row.get(0)?,
        signature: row.get(1)?,
        plan: row.get(2)?,
        outcome: ProcedureOutcome::parse(&row.get::<_, String>(3)?),
        scope_id: row.get(4)?,
        uses: row.get(7)?,
        failures: row.get(8)?,
        created_at: parse_procedure_ts(&created),
        updated_at: parse_procedure_ts(&updated),
    })
}

/// Parse a stored procedure timestamp, falling back to the minimum
/// representable instant.
///
/// Deliberately NOT `Utc::now()`: `updated_at` is the recency tiebreak, so
/// substituting the current time would rank a row with a corrupt timestamp as
/// the freshest procedure in the store and let it win every tie. Sorting last
/// is the direction that fails safe.
///
/// Named distinctly from `store::parse_ts`, which falls back to the Unix epoch
/// (`DateTime::default()`) instead. Both fail safe for recency ordering, but two
/// module-private helpers sharing one name while substituting different
/// constants is a trap for the next reader, so the difference is in the name
/// rather than only in the body.
fn parse_procedure_ts(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .map(|ts| ts.with_timezone(&Utc))
        .unwrap_or(DateTime::<Utc>::MIN_UTC)
}

/// Trim a scope, strip trailing slashes, and drop empties, so `Some("")`
/// cannot create a workspace scope that matches no checkout and `/repo/a/` is
/// the same scope as `/repo/a`.
///
/// Deliberately NOT identical to the page-side `normalize_scope_id`: that one
/// checks emptiness before stripping slashes, so a root workspace `"/"` becomes
/// `Some("")` — a scope no checkout can match. Here `"/"` becomes `None`
/// (global), which is the safer of the two readings.
pub(crate) fn normalize_procedure_scope(scope_id: Option<&str>) -> Option<String> {
    let trimmed = scope_id?.trim().trim_end_matches('/');
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

impl crate::BrainStore {
    /// Record that `plan` was run for `signature` and ended in `outcome`.
    ///
    /// Re-recording the same `(signature, plan, scope)` refreshes `updated_at`
    /// instead of inserting a duplicate, so a plan that keeps working accrues
    /// evidence rather than filling the table with copies. `uses` counts
    /// successes only. The outcome is overwritten by the newest record: a plan
    /// that has started failing must stop being recallable.
    ///
    /// The upsert is a single `ON CONFLICT` statement against a unique index
    /// rather than a read-then-write, because two processes sharing the default
    /// store would otherwise both miss the row and both insert, splitting the
    /// success evidence the ranking depends on.
    pub fn record_procedure(
        &self,
        signature: &str,
        plan: &str,
        outcome: ProcedureOutcome,
        scope_id: Option<&str>,
    ) -> Result<i64> {
        let signature = signature.trim();
        let plan = plan.trim();
        if signature.is_empty() || plan.is_empty() {
            return Err(crate::BrainError::Validation(
                "procedure requires a non-empty signature and plan".to_owned(),
            ));
        }
        let now = Utc::now().to_rfc3339();
        let key = signature.to_ascii_lowercase();
        // Normalized so a trailing slash is the same workspace, matching how
        // pages scope themselves; an unnormalized scope would be invisible to
        // every scoped query and visible to none.
        let scope = normalize_procedure_scope(scope_id);
        let credit = i64::from(outcome == ProcedureOutcome::Succeeded);

        // Single statement against the unique index. `COALESCE(scope_id, '')`
        // in the index is required because SQLite treats NULLs as distinct, so
        // a plain unique index would not collapse global rows.
        self.conn().execute(
            "INSERT INTO memory_procedure
             (signature, signature_key, plan, outcome, scope_id, uses, failures,
              created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
             ON CONFLICT (signature_key, plan, COALESCE(scope_id, '')) DO UPDATE SET
                 uses = uses + ?6,
                 failures = failures + ?7,
                 outcome = excluded.outcome,
                 signature = excluded.signature,
                 updated_at = excluded.updated_at",
            params![
                signature,
                key,
                plan,
                outcome.as_str(),
                scope,
                credit,
                1 - credit,
                now
            ],
        )?;

        self.conn()
            .query_row(
                "SELECT id FROM memory_procedure
                 WHERE signature_key = ?1 AND plan = ?2 AND COALESCE(scope_id, '') = ?3",
                params![key, plan, scope.as_deref().unwrap_or("")],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    /// Fetch one procedure by id.
    pub fn get_procedure(&self, id: i64) -> Result<Option<Procedure>> {
        self.conn()
            .query_row(
                "SELECT id, signature, plan, outcome, scope_id, created_at, updated_at, uses,
                        failures
                 FROM memory_procedure WHERE id = ?1",
                params![id],
                row_to_procedure,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Recall successful plans whose signature resembles `options.goal`.
    ///
    /// Ranked by similarity, then [`Procedure::reliability`], then raw `uses`,
    /// then recency. Failed procedures are never returned at all.
    ///
    /// Reliability rather than raw success volume: ordering on `uses` with
    /// `failures` as a mere tiebreak means one extra success outweighs ANY
    /// number of failures, so a plan that succeeded twice and failed fifty
    /// times outranks one that has only ever succeeded. Volume still breaks
    /// ties between equally reliable plans, so evidence quantity is not
    /// discarded — it is just no longer allowed to override quality.
    pub fn recall_procedures(
        &self,
        options: &ProcedureRecallOptions,
    ) -> Result<Vec<RecalledProcedure>> {
        let query_tokens = signature_tokens(&options.goal);
        if query_tokens.is_empty() || options.limit == 0 {
            return Ok(Vec::new());
        }

        // Scope filter in SQL; similarity in Rust, because Jaccard over token
        // sets is not expressible in stock SQLite. Scope bounds VISIBILITY, not
        // cardinality, so the row count is bounded explicitly by
        // `max_candidates` — see its docs for what that trades away.
        //
        // Globals are always visible; a workspace row only on exact match. The
        // previous `?1 IS NULL OR ...` form made an UNSCOPED query match every
        // row, leaking one checkout's procedures into another's recall.
        let mut stmt = self.conn().prepare(
            "SELECT id, signature, plan, outcome, scope_id, created_at, updated_at, uses,
                    failures
             FROM memory_procedure
             WHERE outcome = ?1 AND (scope_id IS NULL OR scope_id IS ?2)
             ORDER BY updated_at DESC, id DESC
             LIMIT ?3",
        )?;
        let scope = normalize_procedure_scope(options.scope_id.as_deref());
        let candidates = stmt
            .query_map(
                // Bound from the enum rather than inlined, so the filter cannot
                // drift from what `as_str` writes.
                params![
                    ProcedureOutcome::Succeeded.as_str(),
                    scope,
                    options.max_candidates as i64
                ],
                row_to_procedure,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let mut scored: Vec<RecalledProcedure> = candidates
            .into_iter()
            .filter_map(|procedure| {
                let similarity = jaccard(&query_tokens, &signature_tokens(&procedure.signature));
                (similarity >= options.min_similarity).then_some(RecalledProcedure {
                    procedure,
                    similarity,
                })
            })
            .collect();

        scored.sort_by(|a, b| {
            b.similarity
                .total_cmp(&a.similarity)
                .then(
                    b.procedure
                        .reliability()
                        .total_cmp(&a.procedure.reliability()),
                )
                .then(b.procedure.uses.cmp(&a.procedure.uses))
                .then(b.procedure.updated_at.cmp(&a.procedure.updated_at))
        });
        scored.truncate(options.limit);
        Ok(scored)
    }

    /// Delete one procedure. Returns whether a row was removed.
    pub fn forget_procedure(&self, id: i64) -> Result<bool> {
        let removed = self
            .conn()
            .execute("DELETE FROM memory_procedure WHERE id = ?1", params![id])?;
        Ok(removed > 0)
    }

    /// Total number of stored procedures, for status reporting.
    pub fn procedure_count(&self) -> Result<i64> {
        self.conn()
            .query_row("SELECT COUNT(*) FROM memory_procedure", [], |row| {
                row.get(0)
            })
            .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BrainStore;

    fn store() -> BrainStore {
        BrainStore::open_in_memory().expect("open in-memory brain")
    }

    /// Create a `memory_procedure` table in the shape a previous build left
    /// behind, so migration tests exercise the real upgrade path.
    ///
    /// `with_failures` selects between the two shipped shapes: the original had
    /// no `failures` column, and defaults drifted from 1 to 0 when `uses`
    /// started counting successes only. Pasting this DDL per test let those two
    /// facts diverge silently between copies.
    fn forge_legacy_table(path: &std::path::Path, with_failures: bool) -> rusqlite::Connection {
        let conn = rusqlite::Connection::open(path).unwrap();
        let failures = if with_failures {
            "failures INTEGER NOT NULL DEFAULT 0,"
        } else {
            ""
        };
        let uses_default = if with_failures { 0 } else { 1 };
        conn.execute_batch(&format!(
            "CREATE TABLE memory_procedure (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 signature TEXT NOT NULL, signature_key TEXT NOT NULL,
                 plan TEXT NOT NULL, outcome TEXT NOT NULL, scope_id TEXT,
                 uses INTEGER NOT NULL DEFAULT {uses_default},
                 {failures}
                 created_at TEXT NOT NULL, updated_at TEXT NOT NULL);"
        ))
        .unwrap();
        conn
    }

    /// Insert a legacy row through the forged table. `failures` is ignored when
    /// the forged shape predates that column.
    fn forge_row(
        conn: &rusqlite::Connection,
        signature: &str,
        scope: Option<&str>,
        uses: i64,
        failures: Option<i64>,
    ) {
        const TS: &str = "2026-01-01T00:00:00+00:00";
        match failures {
            Some(failures) => conn.execute(
                "INSERT INTO memory_procedure
                 (signature, signature_key, plan, outcome, scope_id, uses, failures,
                  created_at, updated_at)
                 VALUES (?1, ?1, 'p', 'succeeded', ?2, ?3, ?4, ?5, ?5)",
                rusqlite::params![signature, scope, uses, failures, TS],
            ),
            None => conn.execute(
                "INSERT INTO memory_procedure
                 (signature, signature_key, plan, outcome, scope_id, uses,
                  created_at, updated_at)
                 VALUES (?1, ?1, 'p', 'succeeded', ?2, ?3, ?4, ?4)",
                rusqlite::params![signature, scope, uses, TS],
            ),
        }
        .unwrap();
    }

    #[test]
    fn record_and_recall_round_trip() {
        let store = store();
        let id = store
            .record_procedure(
                "add a new CLI subcommand",
                "1. define args 2. wire dispatch 3. test",
                ProcedureOutcome::Succeeded,
                None,
            )
            .unwrap();

        let recalled = store
            .recall_procedures(&ProcedureRecallOptions::new("add a new CLI subcommand"))
            .unwrap();
        assert_eq!(recalled.len(), 1);
        assert_eq!(recalled[0].procedure.id, id);
        assert_eq!(
            recalled[0].similarity, 1.0,
            "identical signature is an exact match"
        );
        assert!(recalled[0].procedure.plan.contains("wire dispatch"));
    }

    #[test]
    fn recall_ranks_the_closer_signature_higher() {
        let store = store();
        store
            .record_procedure(
                "add a new CLI subcommand with tests",
                "close plan",
                ProcedureOutcome::Succeeded,
                None,
            )
            .unwrap();
        store
            .record_procedure(
                "migrate the database schema",
                "far plan",
                ProcedureOutcome::Succeeded,
                None,
            )
            .unwrap();

        let recalled = store
            .recall_procedures(
                &ProcedureRecallOptions::new("add a new CLI subcommand").min_similarity(0.0),
            )
            .unwrap();
        assert_eq!(recalled[0].procedure.plan, "close plan");
        assert!(
            recalled[0].similarity > recalled.get(1).map_or(0.0, |r| r.similarity),
            "the closer signature must rank first",
        );
    }

    /// Returning the best of a bad set hands the agent a plan for a different
    /// problem, which is worse than planning fresh.
    #[test]
    fn an_unrelated_goal_recalls_nothing() {
        let store = store();
        store
            .record_procedure(
                "add a new CLI subcommand",
                "plan",
                ProcedureOutcome::Succeeded,
                None,
            )
            .unwrap();

        let recalled = store
            .recall_procedures(&ProcedureRecallOptions::new(
                "renew the TLS certificate chain",
            ))
            .unwrap();
        assert!(recalled.is_empty(), "got {recalled:?}");
    }

    #[test]
    fn an_empty_store_recalls_nothing() {
        let store = store();
        assert!(
            store
                .recall_procedures(&ProcedureRecallOptions::new("anything at all"))
                .unwrap()
                .is_empty()
        );
        assert_eq!(store.procedure_count().unwrap(), 0);
    }

    /// A plan that stopped working must stop being offered.
    #[test]
    fn failed_procedures_are_recorded_but_never_recalled() {
        let store = store();
        store
            .record_procedure("flaky task", "bad plan", ProcedureOutcome::Failed, None)
            .unwrap();

        assert_eq!(store.procedure_count().unwrap(), 1, "still recorded");
        assert!(
            store
                .recall_procedures(&ProcedureRecallOptions::new("flaky task"))
                .unwrap()
                .is_empty(),
            "but never offered for reuse",
        );
    }

    /// Re-recording the same pair accrues evidence instead of duplicating, and
    /// a later failure demotes a previously-successful plan.
    #[test]
    fn re_recording_accrues_uses_and_the_latest_outcome_wins() {
        let store = store();
        let first = store
            .record_procedure("recurring task", "plan", ProcedureOutcome::Succeeded, None)
            .unwrap();
        let second = store
            .record_procedure("recurring task", "plan", ProcedureOutcome::Succeeded, None)
            .unwrap();
        assert_eq!(first, second, "same pair updates in place");
        assert_eq!(store.procedure_count().unwrap(), 1);
        assert_eq!(store.get_procedure(first).unwrap().unwrap().uses, 2);

        store
            .record_procedure("recurring task", "plan", ProcedureOutcome::Failed, None)
            .unwrap();
        assert!(
            store
                .recall_procedures(&ProcedureRecallOptions::new("recurring task"))
                .unwrap()
                .is_empty(),
            "a plan that started failing must stop being recallable",
        );
    }

    #[test]
    fn workspace_scope_is_honored_and_global_stays_visible() {
        let store = store();
        store
            .record_procedure(
                "scoped task",
                "here",
                ProcedureOutcome::Succeeded,
                Some("/repo/a"),
            )
            .unwrap();
        store
            .record_procedure(
                "scoped task",
                "elsewhere",
                ProcedureOutcome::Succeeded,
                Some("/repo/b"),
            )
            .unwrap();
        store
            .record_procedure(
                "scoped task",
                "everywhere",
                ProcedureOutcome::Succeeded,
                None,
            )
            .unwrap();

        let plans: Vec<String> = store
            .recall_procedures(
                &ProcedureRecallOptions::new("scoped task")
                    .scope_id(Some("/repo/a".to_owned()))
                    .limit(10),
            )
            .unwrap()
            .into_iter()
            .map(|r| r.procedure.plan)
            .collect();
        assert!(plans.contains(&"here".to_owned()));
        assert!(
            plans.contains(&"everywhere".to_owned()),
            "global stays visible"
        );
        assert!(
            !plans.contains(&"elsewhere".to_owned()),
            "other workspace is hidden"
        );
    }

    #[test]
    fn empty_signature_or_plan_is_rejected() {
        let store = store();
        assert!(
            store
                .record_procedure("   ", "plan", ProcedureOutcome::Succeeded, None)
                .is_err()
        );
        assert!(
            store
                .record_procedure("goal", "  ", ProcedureOutcome::Succeeded, None)
                .is_err()
        );
        assert_eq!(store.procedure_count().unwrap(), 0);
    }

    #[test]
    fn forget_procedure_removes_it() {
        let store = store();
        let id = store
            .record_procedure("task", "plan", ProcedureOutcome::Succeeded, None)
            .unwrap();
        assert!(store.forget_procedure(id).unwrap());
        assert!(
            !store.forget_procedure(id).unwrap(),
            "second delete is a no-op"
        );
        assert_eq!(store.procedure_count().unwrap(), 0);
    }

    /// The point of procedural memory is surviving the session that learned
    /// it. Reopening an existing database must find the procedures and must
    /// not fail re-running the schema over already-created tables.
    #[test]
    fn procedures_survive_reopening_an_existing_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("brain.sqlite3");

        let id = {
            let store = BrainStore::open(&path).unwrap();
            store
                .record_procedure(
                    "durable task",
                    "the plan that worked",
                    ProcedureOutcome::Succeeded,
                    None,
                )
                .unwrap()
        };

        // Reopening re-runs the full schema batch over a populated database.
        let reopened = BrainStore::open(&path).unwrap();
        assert_eq!(reopened.procedure_count().unwrap(), 1);
        let recalled = reopened
            .recall_procedures(&ProcedureRecallOptions::new("durable task"))
            .unwrap();
        assert_eq!(recalled.len(), 1);
        assert_eq!(recalled[0].procedure.id, id);
        assert_eq!(recalled[0].procedure.plan, "the plan that worked");
    }

    /// Generation-1 BLOCKER, red-team minimal repro verbatim: `uses` counted
    /// every record including failures, then recall ranked on it, so a plan
    /// that failed five times outranked a plan that had only ever succeeded.
    #[test]
    fn a_repeatedly_failed_plan_does_not_outrank_a_clean_success() {
        let store = store();
        store
            .record_procedure(
                "shared goal text",
                "clean",
                ProcedureOutcome::Succeeded,
                None,
            )
            .unwrap();
        for _ in 0..5 {
            store
                .record_procedure("shared goal text", "flaky", ProcedureOutcome::Failed, None)
                .unwrap();
        }
        store
            .record_procedure(
                "shared goal text",
                "flaky",
                ProcedureOutcome::Succeeded,
                None,
            )
            .unwrap();

        let recalled = store
            .recall_procedures(&ProcedureRecallOptions::new("shared goal text").limit(10))
            .unwrap();
        assert_eq!(
            recalled[0].procedure.plan, "clean",
            "the only-ever-successful plan must rank first",
        );
        assert_eq!(recalled[0].procedure.uses, 1, "one success is one use",);
        let flaky = recalled
            .iter()
            .find(|r| r.procedure.plan == "flaky")
            .expect("still recallable, it did eventually succeed");
        assert_eq!(
            flaky.procedure.uses, 1,
            "five failures credited nothing; uses counts successes, not records",
        );
        assert_eq!(
            flaky.procedure.failures, 5,
            "but the failures are remembered and rank it below a clean success",
        );
    }

    /// Generation-2 BLOCKER (red-team minimal repro): `uses` was the primary
    /// ordinal with `failures` only breaking ties, so one extra raw success
    /// outweighed any number of failures — a 3.8% success rate outranked a
    /// 100% one.
    #[test]
    fn a_worse_success_rate_does_not_outrank_a_perfect_one() {
        let store = store();
        for _ in 0..50 {
            store
                .record_procedure("shared goal text", "risky", ProcedureOutcome::Failed, None)
                .unwrap();
        }
        for _ in 0..2 {
            store
                .record_procedure(
                    "shared goal text",
                    "risky",
                    ProcedureOutcome::Succeeded,
                    None,
                )
                .unwrap();
        }
        store
            .record_procedure(
                "shared goal text",
                "safe",
                ProcedureOutcome::Succeeded,
                None,
            )
            .unwrap();

        let recalled = store
            .recall_procedures(&ProcedureRecallOptions::new("shared goal text").limit(10))
            .unwrap();
        assert_eq!(
            recalled[0].procedure.plan, "safe",
            "1 success / 0 failures must beat 2 successes / 50 failures",
        );
        assert_eq!(recalled[1].procedure.plan, "risky");
    }

    /// Volume decides between plans whose reliability genuinely ties.
    ///
    /// The earlier version of this test used one clean success against four,
    /// which are NOT equally reliable (0.75 vs 0.90) — the reliability
    /// comparator decided it and the `uses` tiebreak was never reached, so
    /// deleting that comparator left the suite green. `(1,1)` and `(3,3)` both
    /// smooth to exactly 0.5, so only the volume comparator can order them.
    #[test]
    fn volume_breaks_ties_between_equally_reliable_plans() {
        let store = store();
        // "thick" reaches (3, 3) FIRST, so both SQL recency and the recency
        // comparator favour "thin". Only the volume comparator can put "thick"
        // ahead. Without that ordering this test passes with the comparator
        // deleted — which is exactly how the previous version was vacuous.
        for _ in 0..3 {
            store
                .record_procedure("shared goal text", "thick", ProcedureOutcome::Failed, None)
                .unwrap();
            store
                .record_procedure(
                    "shared goal text",
                    "thick",
                    ProcedureOutcome::Succeeded,
                    None,
                )
                .unwrap();
        }
        store
            .record_procedure("shared goal text", "thin", ProcedureOutcome::Failed, None)
            .unwrap();
        store
            .record_procedure(
                "shared goal text",
                "thin",
                ProcedureOutcome::Succeeded,
                None,
            )
            .unwrap();

        let recalled = store
            .recall_procedures(&ProcedureRecallOptions::new("shared goal text").limit(10))
            .unwrap();
        let thick = &recalled[0].procedure;
        let thin = &recalled[1].procedure;
        assert_eq!(thick.plan, "thick", "more evidence at an equal rate wins");
        assert_eq!((thick.uses, thick.failures), (3, 3));
        assert_eq!((thin.uses, thin.failures), (1, 1));
        assert_eq!(
            thick.reliability(),
            thin.reliability(),
            "the tie must be exact, or the reliability comparator decides it \
             and the volume comparator is never exercised",
        );
    }

    /// Smoothing exists so the sparsest evidence does not look strongest.
    #[test]
    fn reliability_is_smoothed_so_one_lucky_success_is_not_perfect() {
        let store = store();
        let id = store
            .record_procedure("t", "p", ProcedureOutcome::Succeeded, None)
            .unwrap();
        let one = store.get_procedure(id).unwrap().unwrap();
        assert!(
            one.reliability() < 1.0,
            "a single success must not score a perfect rate, got {}",
            one.reliability(),
        );
        for _ in 0..8 {
            store
                .record_procedure("t", "p", ProcedureOutcome::Succeeded, None)
                .unwrap();
        }
        store
            .record_procedure("t", "p", ProcedureOutcome::Failed, None)
            .unwrap();
        let many = store.get_procedure(id).unwrap().unwrap();
        assert!(
            many.reliability() > one.reliability(),
            "9 successes with 1 failure ({}) must outscore a lone success ({})",
            many.reliability(),
            one.reliability(),
        );
    }

    /// Generation-2 BLOCKER: creating the unique index over a table that
    /// already contained a duplicate failed inside `migrate_schema`, and
    /// `BrainService::open` goes through `BrainStore::open` — so one duplicate
    /// procedure row made the ENTIRE store permanently unopenable.
    #[test]
    fn a_store_with_pre_existing_duplicates_still_opens_and_collapses_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("brain.sqlite3");
        {
            // The pre-index shape, with a duplicate identity.
            let conn = forge_legacy_table(&path, false);
            forge_row(&conn, "t", None, 3, None);
            forge_row(&conn, "t", None, 4, None);
        }

        let store = BrainStore::open(&path).expect("a duplicate must not brick the store");
        assert_eq!(
            store.procedure_count().unwrap(),
            1,
            "duplicates collapse to one row",
        );
        let recalled = store
            .recall_procedures(&ProcedureRecallOptions::new("t"))
            .unwrap();
        assert_eq!(
            recalled[0].procedure.uses, 7,
            "the survivor absorbs the duplicates' counters rather than losing them",
        );
        // And the store is genuinely usable afterwards, not just openable.
        store
            .record_procedure("t", "p", ProcedureOutcome::Succeeded, None)
            .unwrap();
        assert_eq!(store.procedure_count().unwrap(), 1);
    }

    /// Generation-3 BLOCKER: an earlier build wrote `Some("")` through
    /// unnormalized. Those rows match neither branch of the recall filter and
    /// no caller can bind `''` back, so migrating without repairing the scope
    /// left them permanently unrecallable — silent data loss, and strictly
    /// worse than the loud open failure the migration was written to prevent.
    #[test]
    fn a_legacy_empty_string_scope_is_repaired_rather_than_stranded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("brain.sqlite3");
        {
            let conn = forge_legacy_table(&path, true);
            forge_row(&conn, "solo", Some(""), 1, Some(0));
            forge_row(&conn, "slash", Some("/repo/a/"), 1, Some(0));
        }

        let store = BrainStore::open(&path).unwrap();
        // The empty scope becomes global and is reachable unscoped.
        let solo = store
            .recall_procedures(&ProcedureRecallOptions::new("solo"))
            .unwrap();
        assert_eq!(solo.len(), 1, "a repaired empty scope must be recallable");
        assert_eq!(solo[0].procedure.scope_id, None);

        // The trailing-slash scope becomes canonical and is reachable by the
        // unslashed spelling a caller would actually pass.
        let slash = store
            .recall_procedures(
                &ProcedureRecallOptions::new("slash").scope_id(Some("/repo/a".to_owned())),
            )
            .unwrap();
        assert_eq!(
            slash.len(),
            1,
            "a repaired trailing slash must be recallable"
        );
        assert_eq!(slash[0].procedure.scope_id.as_deref(), Some("/repo/a"));
    }

    /// Generation-3 finding: the fold summed `uses` but not `failures`, so a
    /// survivor reported a clean record for a plan that had failed repeatedly
    /// — re-creating the ranking blocker through the migration path.
    #[test]
    fn the_dedup_fold_absorbs_failures_as_well_as_successes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("brain.sqlite3");
        {
            let conn = forge_legacy_table(&path, true);
            for failures in [0, 40, 60] {
                forge_row(&conn, "t", None, 1, Some(failures));
            }
        }

        let store = BrainStore::open(&path).unwrap();
        assert_eq!(store.procedure_count().unwrap(), 1);
        let survivor = store
            .recall_procedures(&ProcedureRecallOptions::new("t"))
            .unwrap()
            .remove(0)
            .procedure;
        assert_eq!(survivor.uses, 3);
        assert_eq!(
            survivor.failures, 100,
            "100 failures must not vanish into a clean-looking record",
        );
        assert!(
            survivor.reliability() < 0.1,
            "reliability must reflect the folded failures, got {}",
            survivor.reliability(),
        );
    }

    /// A corrupt negative counter must not divide by zero and sort ahead of
    /// every honest record.
    #[test]
    fn corrupt_negative_counters_cannot_produce_an_infinite_score() {
        let store = store();
        let id = store
            .record_procedure("t", "p", ProcedureOutcome::Succeeded, None)
            .unwrap();
        store
            .conn()
            .execute(
                "UPDATE memory_procedure SET uses = 0, failures = -1 WHERE id = ?1",
                rusqlite::params![id],
            )
            .unwrap();
        let corrupt = store.get_procedure(id).unwrap().unwrap();
        assert!(
            corrupt.reliability().is_finite(),
            "got {}",
            corrupt.reliability(),
        );
        assert!(corrupt.reliability() <= 1.0);
    }

    /// The repair is gated on a schema version, so it runs once rather than
    /// dropping and rebuilding the identity index on every open — `open` backs
    /// the memory tools and session hooks, so an ungated write transaction
    /// there is a concurrency hazard, not merely wasted work.
    #[test]
    fn the_procedure_repair_runs_once_and_is_stamped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("brain.sqlite3");

        let store = BrainStore::open(&path).unwrap();
        store
            .record_procedure("t", "p", ProcedureOutcome::Succeeded, None)
            .unwrap();
        let stamped: i64 = store
            .conn()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert!(stamped >= 1, "a migrated store records its schema revision");

        // The identity index must exist after the gated repair, or the upsert
        // has no conflict target and duplicates return.
        let reopened = BrainStore::open(&path).unwrap();
        let indexed: i64 = reopened
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name = 'ux_memory_procedure_identity'
                   AND tbl_name = 'memory_procedure'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(indexed, 1, "the identity index survives a gated reopen");

        reopened
            .record_procedure("t", "p", ProcedureOutcome::Succeeded, None)
            .unwrap();
        assert_eq!(
            reopened.procedure_count().unwrap(),
            1,
            "dedup still works after the repair is skipped",
        );
    }

    /// Final-generation red-team finding: gating the repair on the schema stamp
    /// alone let a store be marked migrated while the identity index was
    /// absent — the table is created by the ungated schema batch, the index
    /// only by the gated repair — and the index was then never restored, so
    /// every `record_procedure` failed for the life of that store. The gate
    /// checks the invariant, so a lost index heals on the next open.
    #[test]
    fn a_lost_identity_index_is_restored_despite_the_migration_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("brain.sqlite3");
        {
            let store = BrainStore::open(&path).unwrap();
            store
                .record_procedure("t", "p", ProcedureOutcome::Succeeded, None)
                .unwrap();
        }
        // Drop the index behind the store's back; the stamp stays at 1.
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch("DROP INDEX ux_memory_procedure_identity")
            .unwrap();

        let reopened = BrainStore::open(&path).unwrap();
        assert!(
            reopened
                .record_procedure("t", "p", ProcedureOutcome::Succeeded, None)
                .is_ok(),
            "the upsert needs its conflict target back",
        );
        assert_eq!(
            reopened.procedure_count().unwrap(),
            1,
            "and dedup works again rather than accumulating duplicates",
        );
    }

    /// Generation-1 finding: an UNSCOPED recall matched every row, leaking one
    /// checkout's procedures into another's.
    #[test]
    fn an_unscoped_recall_does_not_leak_other_workspaces() {
        let store = store();
        store
            .record_procedure(
                "scoped task",
                "from a",
                ProcedureOutcome::Succeeded,
                Some("/repo/a"),
            )
            .unwrap();
        store
            .record_procedure("scoped task", "global", ProcedureOutcome::Succeeded, None)
            .unwrap();

        let plans: Vec<String> = store
            .recall_procedures(&ProcedureRecallOptions::new("scoped task").limit(10))
            .unwrap()
            .into_iter()
            .map(|r| r.procedure.plan)
            .collect();
        assert_eq!(
            plans,
            vec!["global".to_owned()],
            "an unscoped query sees globals only, matching the page convention",
        );
    }

    /// Generation-1 finding: dedup was a read-then-write with no unique index,
    /// so concurrent writers produced duplicate rows and split the evidence.
    #[test]
    fn concurrent_identical_records_collapse_to_one_row() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("brain.sqlite3");
        BrainStore::open(&path).unwrap();

        let accepted = std::sync::atomic::AtomicI64::new(0);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let path = path.clone();
                let accepted = &accepted;
                scope.spawn(move || {
                    let store = BrainStore::open(&path).unwrap();
                    for _ in 0..25 {
                        // Contention on a shared file is expected; a busy
                        // database is not a correctness failure, a duplicate
                        // row or a lost increment is.
                        if store
                            .record_procedure(
                                "contended",
                                "plan",
                                ProcedureOutcome::Succeeded,
                                None,
                            )
                            .is_ok()
                        {
                            accepted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        }
                    }
                });
            }
        });

        let accepted = accepted.load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            accepted > 0,
            "the probe must actually have written something"
        );
        let store = BrainStore::open(&path).unwrap();
        assert_eq!(
            store.procedure_count().unwrap(),
            1,
            "the unique index collapses every writer onto one row",
        );
        // Counting accepted writes distinguishes 200 collapsed upserts from
        // 199 rejections; without it a lost increment would pass silently.
        assert_eq!(
            store.get_procedure(1).unwrap().unwrap().uses,
            accepted,
            "every accepted write must be credited exactly once",
        );
    }

    /// Generation-1 finding: `Some("")` created a workspace scope matching no
    /// checkout, and a trailing slash made a second distinct scope.
    #[test]
    fn scope_ids_are_normalized_on_both_record_and_recall() {
        let store = store();
        store
            .record_procedure(
                "task",
                "plan",
                ProcedureOutcome::Succeeded,
                Some("/repo/a/"),
            )
            .unwrap();
        store
            .record_procedure("task", "plan", ProcedureOutcome::Succeeded, Some("/repo/a"))
            .unwrap();
        assert_eq!(
            store.procedure_count().unwrap(),
            1,
            "a trailing slash is the same workspace",
        );

        let recalled = store
            .recall_procedures(
                &ProcedureRecallOptions::new("task").scope_id(Some("/repo/a/".to_owned())),
            )
            .unwrap();
        assert_eq!(recalled.len(), 1, "and recall normalizes it too");
    }

    /// An empty scope is not a workspace at all; it must become global rather
    /// than a scope nothing can match. Split from the trailing-slash case so a
    /// failure in one cannot hide the other.
    #[test]
    fn a_blank_scope_records_as_global() {
        let store = store();
        store
            .record_procedure("t", "p", ProcedureOutcome::Succeeded, Some("   "))
            .unwrap();
        assert_eq!(
            store
                .recall_procedures(&ProcedureRecallOptions::new("t"))
                .unwrap()
                .len(),
            1,
            "an empty scope records as global, visible to an unscoped recall",
        );
    }

    /// Generation-1 finding: the previous version of this test passed with the
    /// ranking comparators deleted, because SQL recency already ordered the
    /// rows. Here recency favours the row that must LOSE, so the ranking chain
    /// — reliability first, then volume — is what decides it.
    #[test]
    fn evidence_outranks_recency() {
        let store = store();
        store
            .record_procedure(
                "shared goal text",
                "proven",
                ProcedureOutcome::Succeeded,
                None,
            )
            .unwrap();
        store
            .record_procedure(
                "shared goal text",
                "proven",
                ProcedureOutcome::Succeeded,
                None,
            )
            .unwrap();
        // Recorded LAST, so SQL recency ordering puts it first.
        store
            .record_procedure(
                "shared goal text",
                "recent",
                ProcedureOutcome::Succeeded,
                None,
            )
            .unwrap();

        let recalled = store
            .recall_procedures(&ProcedureRecallOptions::new("shared goal text").limit(10))
            .unwrap();
        assert_eq!(
            recalled[0].procedure.plan, "proven",
            "two successes beat one, even though the other row is more recent",
        );
        assert_eq!(recalled[0].procedure.uses, 2);
    }

    /// A corrupt timestamp must sort LAST, not become the freshest row and win
    /// every recency tiebreak.
    #[test]
    fn a_corrupt_timestamp_sorts_last_instead_of_freshest() {
        assert_eq!(
            super::parse_procedure_ts("not-a-timestamp"),
            DateTime::<Utc>::MIN_UTC,
        );
        assert!(super::parse_procedure_ts("not-a-timestamp") < Utc::now());
    }

    /// A3: `get_procedure` and `recall_procedures` are two readers of one
    /// column and must agree about what it says. A lenient parser beside a
    /// byte-exact SQL filter meant a row stored as "Succeeded" read as a
    /// success through one API and was silently excluded by the other.
    #[test]
    fn both_readers_agree_on_a_non_canonical_outcome() {
        let store = store();
        let id = store
            .record_procedure("tampered", "p", ProcedureOutcome::Succeeded, None)
            .unwrap();
        for spelling in [
            "Succeeded",
            "SUCCEEDED",
            " succeeded",
            "succeeded ",
            "SUCCESS",
        ] {
            store
                .conn()
                .execute(
                    "UPDATE memory_procedure SET outcome = ?1 WHERE id = ?2",
                    rusqlite::params![spelling, id],
                )
                .unwrap();

            let typed = store.get_procedure(id).unwrap().unwrap().outcome;
            let recalled = store
                .recall_procedures(&ProcedureRecallOptions::new("tampered"))
                .unwrap();
            assert_eq!(
                typed,
                ProcedureOutcome::Failed,
                "{spelling:?} is not what as_str writes, so it must not read as success",
            );
            assert!(
                recalled.is_empty(),
                "{spelling:?} must be excluded by recall too, agreeing with get_procedure",
            );
        }

        // The canonical spelling still works through both readers.
        store
            .conn()
            .execute(
                "UPDATE memory_procedure SET outcome = 'succeeded' WHERE id = ?1",
                rusqlite::params![id],
            )
            .unwrap();
        assert_eq!(
            store.get_procedure(id).unwrap().unwrap().outcome,
            ProcedureOutcome::Succeeded,
        );
        assert_eq!(
            store
                .recall_procedures(&ProcedureRecallOptions::new("tampered"))
                .unwrap()
                .len(),
            1,
        );
    }

    /// A13: scope bounds visibility, not cardinality, so the candidate set is
    /// capped explicitly. The cap takes the most recently updated rows, which
    /// makes the cut deterministic rather than arbitrary.
    #[test]
    fn the_candidate_set_is_bounded_and_takes_the_freshest_rows() {
        let store = store();
        for i in 0..10 {
            store
                .record_procedure(
                    "shared goal text",
                    &format!("plan {i}"),
                    ProcedureOutcome::Succeeded,
                    None,
                )
                .unwrap();
        }

        let capped = store
            .recall_procedures(
                &ProcedureRecallOptions::new("shared goal text")
                    .limit(10)
                    .max_candidates(3),
            )
            .unwrap();
        assert_eq!(capped.len(), 3, "the SQL cap bounds what ranking ever sees");

        let uncapped = store
            .recall_procedures(&ProcedureRecallOptions::new("shared goal text").limit(10))
            .unwrap();
        assert_eq!(
            uncapped.len(),
            10,
            "the default is not truncating a small store"
        );

        // The cap keeps the freshest, so the last-written plan survives it.
        let plans: Vec<&str> = capped.iter().map(|r| r.procedure.plan.as_str()).collect();
        assert!(
            plans.contains(&"plan 9"),
            "the cap must take the most recently updated rows, got {plans:?}",
        );
    }

    /// A2: the page-side scope normalizer used to check emptiness BEFORE
    /// stripping trailing slashes, so a root workspace "/" became `Some("")` —
    /// a workspace scope no checkout can match, invisible to scoped and
    /// unscoped queries alike. Both normalizers now agree.
    #[test]
    fn a_root_workspace_scope_is_global_for_pages_and_procedures_alike() {
        use crate::{MemoryCategory, NewPage};

        let store = store();
        for scope in ["/", "//", "  /  ", ""] {
            assert_eq!(
                normalize_procedure_scope(Some(scope)),
                None,
                "procedures: {scope:?} is not a workspace",
            );
        }

        // A page scoped to "/" must be reachable, not stranded in Some("").
        let page = store
            .create_page_scoped(
                NewPage {
                    title: Some("root scoped".to_owned()),
                    memory_text: "body".to_owned(),
                    category: MemoryCategory::Notes,
                    source: None,
                },
                Some("/"),
            )
            .unwrap();
        assert_eq!(
            page.scope_id, None,
            "a root workspace reads as global rather than an unmatchable scope",
        );
        assert!(
            store.get_page(page.id).unwrap().is_some(),
            "and the page is retrievable",
        );
    }

    /// An empty goal must match nothing rather than everything.
    #[test]
    fn an_empty_goal_matches_nothing() {
        let store = store();
        store
            .record_procedure("task", "plan", ProcedureOutcome::Succeeded, None)
            .unwrap();
        assert!(
            store
                .recall_procedures(&ProcedureRecallOptions::new("   ").min_similarity(0.0))
                .unwrap()
                .is_empty()
        );
    }
}
