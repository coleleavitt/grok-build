//! The LLM boundary, plus the Prompter/Parser pair that surrounds it.

use crate::thought::ThoughtState;
use async_trait::async_trait;
use std::sync::atomic::{AtomicU64, Ordering};

/// What one LLM round-trip cost, so a run can be priced the way the paper
/// prices its plots (quality against dollars, not against call count).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cost: f64,
}

impl std::ops::AddAssign for Usage {
    fn add_assign(&mut self, rhs: Self) {
        self.prompt_tokens += rhs.prompt_tokens;
        self.completion_tokens += rhs.completion_tokens;
        self.cost += rhs.cost;
    }
}

/// A completed query: the sampled texts plus what they cost.
#[derive(Clone, Debug, Default)]
pub struct Completion {
    pub texts: Vec<String>,
    pub usage: Usage,
}

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("language model query failed: {0}")]
    Query(String),
}

/// The single seam between the engine and an actual model.
///
/// `num_responses` is sampled server-side where possible; the engine treats
/// the returned texts as independent samples of the same prompt, which is what
/// makes `Generate` with `branches_response > 1` a fan-out rather than a loop.
#[async_trait]
pub trait LanguageModel: Send + Sync {
    async fn query(&self, prompt: &str, num_responses: u32) -> Result<Completion, ModelError>;
}

/// Builds the prompt for each operation that talks to the model.
///
/// Kept synchronous and separate from [`Parser`] so a use case can be unit
/// tested end to end without a model: the paper's whole method is "the graph
/// decomposition is the contribution", and that decomposition is testable
/// exactly when prompt construction is a pure function.
pub trait Prompter: Send + Sync {
    /// `branches` is the fan-out the prompt itself should request (the paper's
    /// `Generate(t, k)` where one reply carries k thoughts), as distinct from
    /// sampling the prompt k times.
    fn generate_prompt(&self, branches: u32, state: &ThoughtState) -> String;
    fn aggregation_prompt(&self, states: &[ThoughtState]) -> String;
    fn improve_prompt(&self, state: &ThoughtState) -> String;
    fn validation_prompt(&self, state: &ThoughtState) -> String;
    fn score_prompt(&self, states: &[ThoughtState]) -> String;
}

/// Turns model text back into thought state.
///
/// Every method takes the originating state(s) because parsing is usually
/// relative: a sort reply is only interpretable against the list that was sent,
/// and a score reply against the thought it judges.
pub trait Parser: Send + Sync {
    /// Each returned state is layered over the base state by the caller, so a
    /// parser may return only the keys it changed.
    fn parse_generate(&self, base: &ThoughtState, texts: &[String]) -> Vec<ThoughtState>;
    fn parse_aggregation(&self, states: &[ThoughtState], texts: &[String]) -> Vec<ThoughtState>;
    fn parse_improve(&self, state: &ThoughtState, texts: &[String]) -> ThoughtState;
    fn parse_validation(&self, state: &ThoughtState, texts: &[String]) -> bool;
    /// One score per input state, in the same order.
    fn parse_score(&self, states: &[ThoughtState], texts: &[String]) -> Vec<f64>;
}

/// A [`LanguageModel`] that replays canned responses in order.
///
/// Exposed (not `#[cfg(test)]`) so downstream crates can test their own
/// Prompter/Parser and graph decomposition without a network or an API key —
/// the decomposition is the part worth testing, and it is deterministic.
pub struct ScriptedModel {
    responses: Vec<Vec<String>>,
    cursor: AtomicU64,
    per_call_usage: Usage,
}

impl ScriptedModel {
    /// Each element is the full set of texts one `query` returns.
    pub fn new(responses: Vec<Vec<String>>) -> Self {
        Self {
            responses,
            cursor: AtomicU64::new(0),
            per_call_usage: Usage::default(),
        }
    }

    /// Charge every call the same usage, so cost accounting can be asserted.
    pub fn with_usage(mut self, usage: Usage) -> Self {
        self.per_call_usage = usage;
        self
    }

    /// How many queries have been served.
    pub fn calls(&self) -> u64 {
        self.cursor.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl LanguageModel for ScriptedModel {
    async fn query(&self, _prompt: &str, num_responses: u32) -> Result<Completion, ModelError> {
        let index = self.cursor.fetch_add(1, Ordering::SeqCst) as usize;
        let texts = self
            .responses
            .get(index)
            .ok_or_else(|| {
                ModelError::Query(format!(
                    "scripted model exhausted: call {index} has no response (scripted {})",
                    self.responses.len()
                ))
            })?
            .clone();
        // Honor the requested sample count so a decomposition that asks for k
        // samples cannot silently pass on a script that supplies fewer.
        let texts = texts
            .into_iter()
            .take(num_responses.max(1) as usize)
            .collect();
        Ok(Completion {
            texts,
            usage: self.per_call_usage,
        })
    }
}
