//! Graph of Thoughts — an engine for LLM reasoning shaped as an arbitrary graph.
//!
//! Implements Besta et al., *Graph of Thoughts: Solving Elaborate Problems with
//! Large Language Models* (arXiv:2308.09687). A thought is a vertex, a
//! dependency is an edge, and the reasoning process is a graph you author up
//! front and then execute.
//!
//! # What this buys over a chain or a tree
//!
//! Chain-of-Thought is one path; Tree of Thoughts branches and backtracks along
//! that path. Neither can express **fan-in**: taking several independently
//! developed thoughts and merging them into one. That single missing edge is
//! the paper's contribution, and it is what makes divide-and-conquer
//! expressible — split a problem the model cannot do in one pass into chunks it
//! can, solve them separately, merge the results.
//!
//! # Honest framing
//!
//! The graph here is an execution plan you write, not a structure the model
//! discovers. The paper's own §7.3 states the operative insight plainly:
//! *"combining or concatenating subresults is usually an easier task than
//! solving large task instances from scratch."* Decompose until each leaf is
//! something the model gets right most of the time, then merge. Treat
//! [`GraphOfOperations::thought_volume`] as a description of the topology you
//! authored rather than as evidence about answer quality — the paper never
//! connects the two empirically.
//!
//! # Architecture
//!
//! The §4 module split, which is the part of the paper that aged best:
//!
//! - [`Prompter`] builds prompts; [`Parser`] turns replies back into state.
//!   Both are pure and synchronous, so a decomposition is testable without a
//!   model.
//! - [`LanguageModel`] is the only async seam. [`ScriptedModel`] replays canned
//!   replies for deterministic tests.
//! - [`GraphOfOperations`] is the static plan (GoO). The thoughts that
//!   accumulate on its nodes during a run are the Graph Reasoning State (GRS).
//! - [`Controller`] executes the plan, tracking [`Usage`] so a run can be priced.
//!
//! # Example
//!
//! Split, solve the halves, merge — the shape of the paper's sorting case.
//!
//! ```
//! use xai_grok_got::{Controller, GraphOfOperations, OpKind, ScriptedModel, ThoughtState};
//! # use xai_grok_got::{Parser, Prompter};
//! # use serde_json::json;
//! # struct P;
//! # impl Prompter for P {
//! #     fn generate_prompt(&self, _: u32, _: &ThoughtState) -> String { "split".into() }
//! #     fn aggregation_prompt(&self, _: &[ThoughtState]) -> String { "merge".into() }
//! #     fn improve_prompt(&self, _: &ThoughtState) -> String { String::new() }
//! #     fn validation_prompt(&self, _: &ThoughtState) -> String { String::new() }
//! #     fn score_prompt(&self, _: &[ThoughtState]) -> String { String::new() }
//! # }
//! # struct Q;
//! # impl Parser for Q {
//! #     fn parse_generate(&self, _: &ThoughtState, texts: &[String]) -> Vec<ThoughtState> {
//! #         texts.iter().map(|t| {
//! #             let mut s = ThoughtState::new();
//! #             s.insert("part".into(), json!(t));
//! #             s
//! #         }).collect()
//! #     }
//! #     fn parse_aggregation(&self, _: &[ThoughtState], texts: &[String]) -> Vec<ThoughtState> {
//! #         texts.iter().map(|t| {
//! #             let mut s = ThoughtState::new();
//! #             s.insert("merged".into(), json!(t));
//! #             s
//! #         }).collect()
//! #     }
//! #     fn parse_improve(&self, _: &ThoughtState, _: &[String]) -> ThoughtState { ThoughtState::new() }
//! #     fn parse_validation(&self, _: &ThoughtState, _: &[String]) -> bool { true }
//! #     fn parse_score(&self, states: &[ThoughtState], _: &[String]) -> Vec<f64> { vec![0.0; states.len()] }
//! # }
//! # tokio_test_stub(async {
//! let mut graph = GraphOfOperations::new();
//! let split = graph.append(OpKind::Generate { branches_prompt: 2, branches_response: 1 });
//! graph.add(OpKind::Aggregate { num_responses: 1 }, &[split]);
//!
//! let model = ScriptedModel::new(vec![
//!     vec!["left|right".into()],   // the split
//!     vec!["left+right".into()],   // the merge
//! ]);
//! let mut controller = Controller::new(graph, &model, &P, &Q, ThoughtState::new());
//! controller.run().await.unwrap();
//!
//! assert_eq!(controller.final_thoughts().len(), 1);
//! # });
//! # fn tokio_test_stub<F: std::future::Future>(f: F) { futures_lite_block_on(f); }
//! # fn futures_lite_block_on<F: std::future::Future>(mut f: F) {
//! #     use std::task::{Context, Poll, Waker};
//! #     let mut f = std::pin::pin!(f);
//! #     let mut cx = Context::from_waker(Waker::noop());
//! #     loop { if let Poll::Ready(_) = f.as_mut().poll(&mut cx) { return } }
//! # }
//! ```

mod controller;
mod graph;
mod model;
mod operations;
mod scoring;
mod thought;

pub use controller::{Controller, GotError};
pub use graph::{GraphOfOperations, OpId, OperationNode};
pub use model::{Completion, LanguageModel, ModelError, Parser, Prompter, ScriptedModel, Usage};
pub use operations::{GroundTruthFn, OpKind, ScoringFn, SelectorFn, ValidateFn};
pub use scoring::{positive_score, set_intersection_error_scope, sorting_error_scope};
pub use thought::{Thought, ThoughtState, merge_states};
