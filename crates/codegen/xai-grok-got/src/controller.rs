//! The Controller: drives a [`GraphOfOperations`] to completion against a model.

use crate::graph::{GraphOfOperations, OpId};
use crate::model::{LanguageModel, ModelError, Parser, Prompter, Usage};
use crate::operations::OpKind;
use crate::thought::{Thought, ThoughtState, merge_states};
use std::collections::VecDeque;

#[derive(Debug, thiserror::Error)]
pub enum GotError {
    #[error("the graph has no operations")]
    EmptyGraph,
    #[error("operation {id} ({kind}) consumes thoughts but has no predecessors")]
    RootConsumesThoughts { id: OpId, kind: &'static str },
    #[error("operation {id} ({kind}) requires at least one predecessor")]
    MissingPredecessor { id: OpId, kind: &'static str },
    #[error("KeepBestN at operation {id} received {unscored} unscored thought(s); score first")]
    UnscoredThoughts { id: OpId, unscored: usize },
    #[error(transparent)]
    Model(#[from] ModelError),
}

/// Executes a graph decomposition, accumulating the Graph Reasoning State into
/// the graph itself.
///
/// Owns the thought-id counter rather than the graph so that producing thoughts
/// (which needs the counter) does not require a mutable borrow of the graph
/// while the operation's own definition is still borrowed.
pub struct Controller<'a> {
    graph: GraphOfOperations,
    model: &'a dyn LanguageModel,
    prompter: &'a dyn Prompter,
    parser: &'a dyn Parser,
    /// Seed state handed to root operations, which have no predecessor to
    /// inherit from — the problem statement itself.
    problem: ThoughtState,
    next_thought_id: u64,
    usage: Usage,
}

impl<'a> Controller<'a> {
    pub fn new(
        graph: GraphOfOperations,
        model: &'a dyn LanguageModel,
        prompter: &'a dyn Prompter,
        parser: &'a dyn Parser,
        problem: ThoughtState,
    ) -> Self {
        Self {
            graph,
            model,
            prompter,
            parser,
            problem,
            next_thought_id: 0,
            usage: Usage::default(),
        }
    }

    pub fn graph(&self) -> &GraphOfOperations {
        &self.graph
    }
    /// What the run cost so far, accumulated across every model round-trip.
    pub fn usage(&self) -> Usage {
        self.usage
    }

    /// Thoughts left at the graph's leaves — the run's answer.
    pub fn final_thoughts(&self) -> Vec<&Thought> {
        self.graph
            .leaves()
            .iter()
            .flat_map(|&leaf| self.graph.node(leaf).thoughts.iter())
            .collect()
    }

    /// Reject a decomposition that cannot run, before spending anything.
    ///
    /// The reference implementation asserts these mid-execution, which surfaces
    /// as a crash after an arbitrary number of paid calls. Checking up front is
    /// the whole benefit of the GoO being static.
    fn validate(&self) -> Result<(), GotError> {
        if self.graph.is_empty() {
            return Err(GotError::EmptyGraph);
        }
        for id in self.graph.ids() {
            let node = self.graph.node(id);
            let kind = node.kind.name();
            if node.predecessors.is_empty() {
                if !node.kind.can_be_root() {
                    return Err(GotError::RootConsumesThoughts { id, kind });
                }
            } else if matches!(node.kind, OpKind::Generate { .. } | OpKind::Selector { .. }) {
                // These tolerate having predecessors; nothing to check.
            }
            if node.predecessors.is_empty() && !node.kind.can_be_root() {
                return Err(GotError::MissingPredecessor { id, kind });
            }
        }
        Ok(())
    }

    /// Run every operation whose predecessors have completed, until none remain.
    ///
    /// Ready-queue order rather than a topological sort, matching the reference:
    /// an operation is enqueued the moment its last predecessor finishes.
    pub async fn run(&mut self) -> Result<(), GotError> {
        self.validate()?;

        let mut queue: VecDeque<OpId> = self
            .graph
            .ids()
            .filter(|&id| self.graph.can_execute(id))
            .collect();
        let mut enqueued: Vec<bool> = vec![false; self.graph.len()];
        for &id in &queue {
            enqueued[id] = true;
        }

        while let Some(id) = queue.pop_front() {
            let previous = self.graph.previous_thoughts(id);
            let produced = self.execute(id, previous).await?;

            let node = self.graph.node_mut(id);
            node.thoughts = produced;
            node.executed = true;

            let successors = self.graph.node(id).successors.clone();
            for successor in successors {
                if !enqueued[successor] && self.graph.can_execute(successor) {
                    enqueued[successor] = true;
                    queue.push_back(successor);
                }
            }
        }
        Ok(())
    }

    fn mint(&mut self) -> u64 {
        let id = self.next_thought_id;
        self.next_thought_id += 1;
        id
    }

    async fn ask(&mut self, prompt: &str, samples: u32) -> Result<Vec<String>, GotError> {
        let completion = self.model.query(prompt, samples).await?;
        self.usage += completion.usage;
        Ok(completion.texts)
    }

    async fn execute(
        &mut self,
        id: OpId,
        previous: Vec<Thought>,
    ) -> Result<Vec<Thought>, GotError> {
        // Clone the operation's knobs so the graph borrow ends here; the
        // closure-bearing variants are read through `self.graph` inside each
        // arm, which is fine because those arms never mutate the graph.
        let kind_name = self.graph.node(id).kind.name();
        match &self.graph.node(id).kind {
            OpKind::Generate {
                branches_prompt,
                branches_response,
            } => {
                let (branches_prompt, branches_response) = (*branches_prompt, *branches_response);
                self.run_generate(id, previous, branches_prompt, branches_response)
                    .await
            }
            OpKind::Aggregate { num_responses } => {
                let num_responses = *num_responses;
                self.run_aggregate(previous, num_responses).await
            }
            OpKind::Score { .. } => self.run_score(id, previous).await,
            OpKind::ValidateAndImprove { .. } => self.run_validate_and_improve(id, previous).await,
            OpKind::Improve => self.run_improve(previous).await,
            OpKind::KeepBestN {
                n,
                higher_is_better,
            } => {
                let (n, higher_is_better) = (*n, *higher_is_better);
                self.run_keep_best_n(id, previous, n, higher_is_better)
            }
            OpKind::KeepValid => Ok(self.keep(previous.into_iter().filter(|thought| {
                // Never-validated thoughts pass: absence of a verdict is not a
                // failing verdict.
                !thought.is_validated() || thought.is_valid()
            }))),
            OpKind::GroundTruth { .. } => {
                let mut out = Vec::with_capacity(previous.len());
                for thought in previous {
                    let solved = match &self.graph.node(id).kind {
                        OpKind::GroundTruth { evaluator } => evaluator(&thought.state),
                        _ => unreachable!("matched GroundTruth"),
                    };
                    let mut derived = thought.derive(self.mint());
                    derived.set_solved(solved);
                    out.push(derived);
                }
                Ok(out)
            }
            OpKind::Selector { .. } => {
                let seed;
                let input = if previous.is_empty() {
                    seed = vec![Thought::new(self.mint(), self.problem.clone())];
                    &seed
                } else {
                    &previous
                };
                let selected = match &self.graph.node(id).kind {
                    OpKind::Selector { selector } => selector(input),
                    _ => unreachable!("matched Selector"),
                };
                Ok(self.keep(selected.into_iter()))
            }
        }
        .map_err(|error| {
            tracing::debug!(operation = id, kind = kind_name, ?error, "operation failed");
            error
        })
    }

    /// Re-mint a pass-through set so the graph records the hop.
    fn keep(&mut self, thoughts: impl Iterator<Item = Thought>) -> Vec<Thought> {
        thoughts
            .collect::<Vec<_>>()
            .into_iter()
            .map(|thought| {
                let id = self.mint();
                thought.derive(id)
            })
            .collect()
    }

    async fn run_generate(
        &mut self,
        id: OpId,
        previous: Vec<Thought>,
        branches_prompt: u32,
        branches_response: u32,
    ) -> Result<Vec<Thought>, GotError> {
        // A root Generate has no predecessor, so the problem statement is its
        // base state. A non-root whose predecessors produced nothing yields
        // nothing — it must NOT fall back to the problem statement, or a pruned
        // branch would silently restart from scratch.
        let inputs = if previous.is_empty() {
            if !self.graph.node(id).predecessors.is_empty() {
                return Ok(Vec::new());
            }
            vec![Thought::new(self.mint(), self.problem.clone())]
        } else {
            previous
        };

        let mut out = Vec::new();
        for thought in inputs {
            let prompt = self
                .prompter
                .generate_prompt(branches_prompt, &thought.state);
            let texts = self.ask(&prompt, branches_response).await?;
            for update in self.parser.parse_generate(&thought.state, &texts) {
                let state = merge_states(&thought.state, &update);
                let id = self.mint();
                out.push(Thought::new(id, state));
            }
        }
        Ok(out)
    }

    async fn run_aggregate(
        &mut self,
        previous: Vec<Thought>,
        num_responses: u32,
    ) -> Result<Vec<Thought>, GotError> {
        if previous.is_empty() {
            return Ok(Vec::new());
        }
        // Layer predecessor states in ascending score order so the best-scoring
        // thought's keys win any collision the parser does not overwrite.
        let mut ordered = previous.clone();
        ordered.sort_by(|a, b| a.score().total_cmp(&b.score()));
        let mut base = ThoughtState::new();
        for thought in &ordered {
            base = merge_states(&base, &thought.state);
        }

        let states: Vec<ThoughtState> = previous.iter().map(|t| t.state.clone()).collect();
        let prompt = self.prompter.aggregation_prompt(&states);
        let texts = self.ask(&prompt, num_responses).await?;

        let mut out = Vec::new();
        for update in self.parser.parse_aggregation(&states, &texts) {
            let state = merge_states(&base, &update);
            let id = self.mint();
            out.push(Thought::new(id, state));
        }
        Ok(out)
    }

    async fn run_score(
        &mut self,
        id: OpId,
        previous: Vec<Thought>,
    ) -> Result<Vec<Thought>, GotError> {
        if previous.is_empty() {
            return Err(GotError::MissingPredecessor { id, kind: "Score" });
        }
        let (num_samples, combined, has_scorer) = match &self.graph.node(id).kind {
            OpKind::Score {
                num_samples,
                combined,
                scorer,
            } => (*num_samples, *combined, scorer.is_some()),
            _ => unreachable!("matched Score"),
        };
        let states: Vec<ThoughtState> = previous.iter().map(|t| t.state.clone()).collect();

        let scores: Vec<f64> = if has_scorer {
            match &self.graph.node(id).kind {
                OpKind::Score {
                    scorer: Some(scorer),
                    ..
                } => scorer(&states),
                _ => unreachable!("has_scorer"),
            }
        } else if combined {
            let prompt = self.prompter.score_prompt(&states);
            let texts = self.ask(&prompt, num_samples).await?;
            self.parser.parse_score(&states, &texts)
        } else {
            let mut scores = Vec::with_capacity(states.len());
            for state in &states {
                let one = std::slice::from_ref(state);
                let prompt = self.prompter.score_prompt(one);
                let texts = self.ask(&prompt, num_samples).await?;
                scores.extend(self.parser.parse_score(one, &texts).into_iter().take(1));
            }
            scores
        };

        Ok(previous
            .into_iter()
            .zip(scores)
            .map(|(thought, score)| {
                let id = self.mint();
                let mut derived = thought.derive(id);
                derived.set_score(score);
                derived
            })
            .collect())
    }

    async fn run_validate_and_improve(
        &mut self,
        id: OpId,
        previous: Vec<Thought>,
    ) -> Result<Vec<Thought>, GotError> {
        if previous.is_empty() {
            return Err(GotError::MissingPredecessor {
                id,
                kind: "ValidateAndImprove",
            });
        }
        let (num_samples, improve, num_tries, has_validator) = match &self.graph.node(id).kind {
            OpKind::ValidateAndImprove {
                num_samples,
                improve,
                num_tries,
                validator,
            } => (*num_samples, *improve, *num_tries, validator.is_some()),
            _ => unreachable!("matched ValidateAndImprove"),
        };

        let mut out = Vec::new();
        for thought in previous {
            let mut current = thought;
            let mut tries = 0;
            loop {
                let valid = if has_validator {
                    match &self.graph.node(id).kind {
                        OpKind::ValidateAndImprove {
                            validator: Some(validator),
                            ..
                        } => validator(&current.state),
                        _ => unreachable!("has_validator"),
                    }
                } else {
                    let prompt = self.prompter.validation_prompt(&current.state);
                    let texts = self.ask(&prompt, num_samples).await?;
                    self.parser.parse_validation(&current.state, &texts)
                };

                let minted = self.mint();
                let mut settled = current.derive(minted);
                settled.set_valid(valid);
                current = settled;

                if !improve || valid || tries >= num_tries {
                    break;
                }
                let prompt = self.prompter.improve_prompt(&current.state);
                let texts = self.ask(&prompt, 1).await?;
                let update = self.parser.parse_improve(&current.state, &texts);
                let state = merge_states(&current.state, &update);
                let minted = self.mint();
                current = Thought::new(minted, state);
                tries += 1;
            }
            // Only the last attempt of each chain leaves the operation; the
            // intermediates are the loop, not the result.
            out.push(current);
        }
        Ok(out)
    }

    async fn run_improve(&mut self, previous: Vec<Thought>) -> Result<Vec<Thought>, GotError> {
        let mut out = Vec::new();
        for thought in previous {
            let prompt = self.prompter.improve_prompt(&thought.state);
            let texts = self.ask(&prompt, 1).await?;
            let update = self.parser.parse_improve(&thought.state, &texts);
            let state = merge_states(&thought.state, &update);
            let id = self.mint();
            out.push(Thought::new(id, state));
        }
        Ok(out)
    }

    fn run_keep_best_n(
        &mut self,
        id: OpId,
        previous: Vec<Thought>,
        n: usize,
        higher_is_better: bool,
    ) -> Result<Vec<Thought>, GotError> {
        let unscored = previous.iter().filter(|t| !t.is_scored()).count();
        if unscored > 0 {
            // Ranking unscored thoughts silently ranks them all equal at the
            // default, which for an error-scope metric is the BEST score — the
            // pruning step would then keep exactly the thoughts nobody judged.
            return Err(GotError::UnscoredThoughts { id, unscored });
        }
        let mut ranked = previous;
        ranked.sort_by(|a, b| {
            if higher_is_better {
                b.score().total_cmp(&a.score())
            } else {
                a.score().total_cmp(&b.score())
            }
        });
        ranked.truncate(n);
        Ok(self.keep(ranked.into_iter()))
    }
}
