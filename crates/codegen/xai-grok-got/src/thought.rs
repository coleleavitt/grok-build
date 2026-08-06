//! The unit of information a Graph of Thoughts reasons over.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A thought's payload.
///
/// A JSON object rather than a typed struct because the paper's contract is
/// that a thought's shape is use-case specific (a sorted sublist, a keyword
/// tally, a merged document) and that operations MERGE states key-by-key —
/// `Generate` layers a parsed update over the base state, `Aggregate` layers
/// every predecessor's state in score order. A typed struct would have to be
/// re-declared per use case and could not express that merge.
pub type ThoughtState = Map<String, Value>;

/// Layer `update` over `base`, with `update` winning on collision.
///
/// The Python reference spells this `{**base, **update}` at four call sites;
/// naming it once keeps the precedence from drifting between operations.
pub fn merge_states(base: &ThoughtState, update: &ThoughtState) -> ThoughtState {
    let mut merged = base.clone();
    for (key, value) in update {
        merged.insert(key.clone(), value.clone());
    }
    merged
}

/// One vertex of the reasoning graph: a solution (initial, intermediate, or
/// final) plus the verdicts operations have reached about it.
///
/// The three verdict fields are paired with a "has this been assessed yet"
/// flag, and the setters raise the flag. That distinction is load-bearing:
/// `KeepBestN` refuses to rank thoughts that were never scored, and
/// `KeepValid` keeps a thought that was never validated while dropping one
/// that was validated and failed. A bare `score: 0.0` cannot express
/// "unscored" — `0.0` is a legitimate score, and for an error-scope metric it
/// is the BEST possible one.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Thought {
    /// Unique within one [`crate::GraphOfOperations`] run; assigned by the arena.
    pub id: u64,
    pub state: ThoughtState,
    score: f64,
    valid: bool,
    solved: bool,
    scored: bool,
    validated: bool,
    compared_to_ground_truth: bool,
}

impl Thought {
    pub fn new(id: u64, state: ThoughtState) -> Self {
        Self {
            id,
            state,
            score: 0.0,
            valid: false,
            solved: false,
            scored: false,
            validated: false,
            compared_to_ground_truth: false,
        }
    }

    /// Clone `self` under a fresh `id`.
    ///
    /// Operations that pass a thought through (`Score`, `KeepBestN`,
    /// `KeepValid`, `GroundTruth`, `Selector`) emit a NEW vertex rather than
    /// mutating the predecessor's, so the graph records that the thought
    /// flowed through the operation. Without it the reasoning graph would lose
    /// every non-generating step and `volume` would under-count.
    pub fn derive(&self, id: u64) -> Self {
        let mut derived = self.clone();
        derived.id = id;
        derived
    }

    pub fn score(&self) -> f64 {
        self.score
    }
    pub fn is_valid(&self) -> bool {
        self.valid
    }
    pub fn is_solved(&self) -> bool {
        self.solved
    }
    pub fn is_scored(&self) -> bool {
        self.scored
    }
    pub fn is_validated(&self) -> bool {
        self.validated
    }
    pub fn is_compared_to_ground_truth(&self) -> bool {
        self.compared_to_ground_truth
    }

    /// Record a score, marking the thought scored.
    pub fn set_score(&mut self, score: f64) {
        self.scored = true;
        self.score = score;
    }

    /// Record a validity verdict, marking the thought validated.
    pub fn set_valid(&mut self, valid: bool) {
        self.validated = true;
        self.valid = valid;
    }

    /// Record whether the thought solves the problem, marking it compared.
    pub fn set_solved(&mut self, solved: bool) {
        self.compared_to_ground_truth = true;
        self.solved = solved;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn state(pairs: &[(&str, Value)]) -> ThoughtState {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect()
    }

    #[test]
    fn merge_states_lets_the_update_win() {
        let base = state(&[("a", json!(1)), ("b", json!(2))]);
        let update = state(&[("b", json!(20)), ("c", json!(3))]);
        let merged = merge_states(&base, &update);
        assert_eq!(merged["a"], json!(1), "untouched keys survive");
        assert_eq!(merged["b"], json!(20), "the update wins on collision");
        assert_eq!(merged["c"], json!(3), "new keys are added");
    }

    /// The scored/validated flags exist so that "not assessed" is
    /// distinguishable from an assessment that happens to equal the default.
    /// Zero is the BEST score under an error-scope metric, so conflating the
    /// two would let `KeepBestN` rank an unscored thought first.
    #[test]
    fn assessment_flags_separate_unset_from_a_default_valued_verdict() {
        let mut thought = Thought::new(0, ThoughtState::new());
        assert!(!thought.is_scored() && thought.score() == 0.0);
        assert!(!thought.is_validated() && !thought.is_valid());
        assert!(!thought.is_compared_to_ground_truth() && !thought.is_solved());

        thought.set_score(0.0);
        assert!(
            thought.is_scored(),
            "an explicit 0.0 still counts as scored"
        );
        thought.set_valid(false);
        assert!(
            thought.is_validated() && !thought.is_valid(),
            "validated-and-failed is not the same as never validated",
        );
        thought.set_solved(false);
        assert!(thought.is_compared_to_ground_truth());
    }

    #[test]
    fn derive_carries_verdicts_under_a_fresh_id() {
        let mut original = Thought::new(7, state(&[("k", json!("v"))]));
        original.set_score(4.5);
        original.set_valid(true);

        let derived = original.derive(8);
        assert_eq!(derived.id, 8);
        assert_eq!(derived.state, original.state);
        assert_eq!(derived.score(), 4.5);
        assert!(derived.is_valid() && derived.is_scored() && derived.is_validated());
    }
}
