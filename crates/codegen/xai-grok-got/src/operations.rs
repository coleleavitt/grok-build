//! The thought transformations. Everything the engine can do to a graph.

use crate::thought::{Thought, ThoughtState};

/// Score a batch of states without consulting the model.
pub type ScoringFn = Box<dyn Fn(&[ThoughtState]) -> Vec<f64> + Send + Sync>;
/// Decide whether a state is well-formed without consulting the model.
pub type ValidateFn = Box<dyn Fn(&ThoughtState) -> bool + Send + Sync>;
/// Decide whether a state actually solves the problem (benchmarking only).
pub type GroundTruthFn = Box<dyn Fn(&ThoughtState) -> bool + Send + Sync>;
/// Pick a subset of thoughts to route down one branch.
pub type SelectorFn = Box<dyn Fn(&[Thought]) -> Vec<Thought> + Send + Sync>;

/// The transformations of §3.2, plus the bookkeeping operations the reference
/// implementation adds to make them composable.
///
/// The three the paper calls out as the graph-enabled core are [`Self::Generate`]
/// (branch), [`Self::Aggregate`] (fan-in — the one CoT and ToT cannot express),
/// and [`Self::Improve`] (the self-loop). The rest exist so a decomposition can
/// prune between them.
pub enum OpKind {
    /// Branch one or more new thoughts off each input.
    ///
    /// Two independent fan-out knobs, because they cost differently and fail
    /// differently. `branches_prompt` asks ONE reply to carry k thoughts (cheap,
    /// shares context, but the model must partition correctly in a single pass).
    /// `branches_response` samples the SAME prompt k times (k independent
    /// context-isolated attempts, k times the cost). The paper's sorting
    /// decomposition uses the first to split the array and the second to take k
    /// independent shots at sorting each chunk.
    Generate {
        branches_prompt: u32,
        branches_response: u32,
    },
    /// Merge every input thought into one, the transformation that makes this a
    /// graph rather than a tree.
    ///
    /// `num_responses` takes several independent shots at the merge; the paper
    /// uses 10 for sorting and keeps the best, because merging is where its
    /// pipeline most often loses elements.
    Aggregate { num_responses: u32 },
    /// Attach a score to each input thought.
    ///
    /// With `scorer` set, scoring is a local pure function (the paper's sorting
    /// and set-intersection cases) — no tokens, no variance. Without it the
    /// model judges, which is what the document-merging case has to do and why
    /// that case has the softest evaluation in the paper.
    ///
    /// `combined` scores all thoughts in one call instead of one call each:
    /// cheaper, and necessary when scores are relative to each other, but it
    /// makes every score depend on the batch.
    Score {
        num_samples: u32,
        combined: bool,
        scorer: Option<ScoringFn>,
    },
    /// Validate each thought and, while it fails, ask the model to fix it.
    ///
    /// Bounded by `num_tries` because refinement is not monotone — the paper's
    /// own appendix (Table 9) shows improve steps returning answers with more
    /// errors than they started with. The loop keeps the last attempt whether
    /// or not it ended valid; pair it with [`Self::KeepValid`] to drop failures.
    ValidateAndImprove {
        num_samples: u32,
        improve: bool,
        num_tries: u32,
        validator: Option<ValidateFn>,
    },
    /// Refine each thought in place — the self-loop `(v, v)` of §3.2.
    Improve,
    /// Refine, but never return something worse than what you were given.
    ///
    /// [`Self::Improve`] and [`Self::ValidateAndImprove`] both keep the LAST
    /// attempt, which is only sound if refinement improves monotonically. It
    /// does not: the GoT paper's own Table 9 shows improve steps returning
    /// answers with 6, 8, and 10 errors after starting from fewer. Keeping the
    /// last attempt means keeping those.
    ///
    /// This scores every attempt including the original and returns the best,
    /// so `score(output) >= score(input)` by construction — the operation can
    /// stall but can never regress.
    ///
    /// # Termination
    ///
    /// `num_tries` is what bounds the loop, and it is doing the real work. It
    /// is tempting to argue that freezing accepted parts gives termination for
    /// free — the accepted set only grows, so the loop must finish. That
    /// argument is wrong twice over: a round that fixes nothing leaves the set
    /// unchanged and makes no progress, and a rewrite can introduce new
    /// material, so the total is not a fixed bound to converge against.
    /// Monotonicity buys non-regression, not termination. The budget buys
    /// termination.
    Repair {
        num_tries: u32,
        /// Required: without a score there is no "worse", and the operation
        /// degenerates into [`Self::Improve`].
        scorer: ScoringFn,
        higher_is_better: bool,
        /// Stop early once a thought is good enough, so a run that succeeds on
        /// the first try costs one score instead of `num_tries` round-trips.
        accept_at: Option<f64>,
    },
    /// Keep the `n` best-scoring inputs.
    ///
    /// `higher_is_better: false` for error-scope metrics, where zero is perfect.
    KeepBestN { n: usize, higher_is_better: bool },
    /// Drop thoughts that were validated and failed; keep the never-validated.
    KeepValid,
    /// Mark which thoughts solve the problem. Benchmarking instrumentation —
    /// it needs the answer, so it cannot run in production.
    GroundTruth { evaluator: GroundTruthFn },
    /// Route a subset of thoughts onward, so branches can diverge.
    Selector { selector: SelectorFn },
}

impl OpKind {
    /// Stable name for logs and graph dumps.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Generate { .. } => "Generate",
            Self::Aggregate { .. } => "Aggregate",
            Self::Score { .. } => "Score",
            Self::ValidateAndImprove { .. } => "ValidateAndImprove",
            Self::Improve => "Improve",
            Self::Repair { .. } => "Repair",
            Self::KeepBestN { .. } => "KeepBestN",
            Self::KeepValid => "KeepValid",
            Self::GroundTruth { .. } => "GroundTruth",
            Self::Selector { .. } => "Selector",
        }
    }

    /// Whether this operation can start a graph.
    ///
    /// Only [`Self::Generate`] and [`Self::Selector`] can: every other operation
    /// transforms thoughts it must already have, so a graph rooted at one has
    /// nothing to work on. Catching that at build time turns a silent empty run
    /// into an error naming the operation.
    pub fn can_be_root(&self) -> bool {
        matches!(self, Self::Generate { .. } | Self::Selector { .. })
    }
}

impl std::fmt::Debug for OpKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The callback variants hold closures, so derive is unavailable and the
        // useful content is the name plus the numeric knobs.
        match self {
            Self::Generate {
                branches_prompt,
                branches_response,
            } => write!(f, "Generate({branches_prompt}x{branches_response})"),
            Self::Aggregate { num_responses } => write!(f, "Aggregate({num_responses})"),
            Self::Score {
                num_samples,
                combined,
                scorer,
            } => write!(
                f,
                "Score(samples={num_samples}, combined={combined}, local={})",
                scorer.is_some()
            ),
            Self::ValidateAndImprove {
                num_tries, improve, ..
            } => write!(
                f,
                "ValidateAndImprove(tries={num_tries}, improve={improve})"
            ),
            Self::KeepBestN {
                n,
                higher_is_better,
            } => {
                write!(f, "KeepBestN({n}, higher_is_better={higher_is_better})")
            }
            other => write!(f, "{}", other.name()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_thought_producing_operations_can_root_a_graph() {
        assert!(
            OpKind::Generate {
                branches_prompt: 1,
                branches_response: 1
            }
            .can_be_root()
        );
        assert!(
            OpKind::Selector {
                selector: Box::new(|thoughts| thoughts.to_vec())
            }
            .can_be_root()
        );
        for kind in [
            OpKind::Aggregate { num_responses: 1 },
            OpKind::Improve,
            OpKind::KeepValid,
            OpKind::KeepBestN {
                n: 1,
                higher_is_better: true,
            },
        ] {
            assert!(
                !kind.can_be_root(),
                "{} consumes thoughts, so it cannot be a root",
                kind.name(),
            );
        }
    }

    #[test]
    fn debug_surfaces_the_knobs_that_drive_cost() {
        let generate = OpKind::Generate {
            branches_prompt: 4,
            branches_response: 3,
        };
        assert_eq!(format!("{generate:?}"), "Generate(4x3)");
        let scored = OpKind::Score {
            num_samples: 2,
            combined: true,
            scorer: None,
        };
        assert_eq!(
            format!("{scored:?}"),
            "Score(samples=2, combined=true, local=false)",
        );
    }
}
