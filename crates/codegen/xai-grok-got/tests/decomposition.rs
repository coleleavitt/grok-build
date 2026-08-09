//! End-to-end runs of the paper's decompositions against a scripted model.
//!
//! These assert the ENGINE, not the model: a scripted model makes every reply
//! deterministic, so what is under test is whether the graph routes thoughts
//! the way §5 says it does.

use serde_json::json;
use xai_grok_got::{
    Controller, GraphOfOperations, OpKind, Parser, Prompter, RepairContext, ScriptedModel,
    ThoughtState, Usage, positive_score, sorting_error_scope,
};

/// Reads/writes a list of numbers under the `"list"` key.
struct ListPrompter;

impl Prompter for ListPrompter {
    fn generate_prompt(&self, branches: u32, state: &ThoughtState) -> String {
        format!("split into {branches}: {:?}", state.get("list"))
    }
    fn aggregation_prompt(&self, states: &[ThoughtState]) -> String {
        format!("merge {} sorted lists", states.len())
    }
    fn improve_prompt(&self, state: &ThoughtState, repair: &RepairContext<'_>) -> String {
        format!(
            "fix (attempt {}, {}): {:?}",
            repair.attempt,
            repair.failure.unwrap_or("unprompted"),
            state.get("list"),
        )
    }
    fn validation_prompt(&self, state: &ThoughtState) -> String {
        format!("valid? {:?}", state.get("list"))
    }
    fn score_prompt(&self, states: &[ThoughtState]) -> String {
        format!("score {} lists", states.len())
    }
}

/// Parses `"1,2,3"` into a list, and `"1,2|3,4"` into two.
struct ListParser;

fn parse_list(text: &str) -> Vec<u32> {
    text.split(',')
        .filter(|piece| !piece.trim().is_empty())
        .filter_map(|piece| piece.trim().parse().ok())
        .collect()
}

fn list_state(values: Vec<u32>) -> ThoughtState {
    let mut state = ThoughtState::new();
    state.insert("list".into(), json!(values));
    state
}

fn state_list(state: &ThoughtState) -> Vec<u32> {
    state
        .get("list")
        .and_then(|value| value.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|v| v.as_u64().map(|n| n as u32))
                .collect()
        })
        .unwrap_or_default()
}

impl Parser for ListParser {
    fn parse_generate(&self, _base: &ThoughtState, texts: &[String]) -> Vec<ThoughtState> {
        texts
            .iter()
            .flat_map(|text| text.split('|').map(|chunk| list_state(parse_list(chunk))))
            .collect()
    }
    fn parse_aggregation(&self, _states: &[ThoughtState], texts: &[String]) -> Vec<ThoughtState> {
        texts
            .iter()
            .map(|text| list_state(parse_list(text)))
            .collect()
    }
    fn parse_improve(&self, _state: &ThoughtState, texts: &[String]) -> ThoughtState {
        list_state(texts.first().map(|t| parse_list(t)).unwrap_or_default())
    }
    fn parse_validation(&self, _state: &ThoughtState, texts: &[String]) -> bool {
        texts.first().is_some_and(|text| text.trim() == "valid")
    }
    fn parse_score(&self, states: &[ThoughtState], texts: &[String]) -> Vec<f64> {
        texts
            .iter()
            .filter_map(|text| text.trim().parse().ok())
            .take(states.len())
            .collect()
    }
}

fn generate(prompt: u32, response: u32) -> OpKind {
    OpKind::Generate {
        branches_prompt: prompt,
        branches_response: response,
    }
}

/// The §5.1 shape: split into chunks, sort each independently, merge, keep the
/// best. Scoring is the paper's local error-scope function, so no tokens are
/// spent judging and the scores are exact.
#[tokio::test]
async fn sorting_decomposition_splits_solves_and_merges() {
    let input: Vec<u32> = vec![3, 1, 2, 9, 8, 7];
    let problem = list_state(input.clone());

    let mut graph = GraphOfOperations::new();
    // Split into 2 chunks (one reply carrying both).
    let split = graph.append(generate(2, 1));
    // Two independent shots at sorting each chunk, then keep the better one.
    let sorted = graph.add(generate(1, 2), &[split]);
    let scored = graph.add(local_scorer(input.clone()), &[sorted]);
    let best = graph.add(
        OpKind::KeepBestN {
            n: 2,
            higher_is_better: false,
        },
        &[scored],
    );
    // Fan-in: the transformation a chain or tree cannot express.
    let merged = graph.add(OpKind::Aggregate { num_responses: 1 }, &[best]);
    let merged_scored = graph.add(local_scorer(input.clone()), &[merged]);
    graph.add(
        OpKind::KeepBestN {
            n: 1,
            higher_is_better: false,
        },
        &[merged_scored],
    );

    let model = ScriptedModel::new(vec![
        vec!["3,1,2|9,8,7".into()],           // split
        vec!["1,2,3".into(), "1,3".into()],   // chunk A: a good and a lossy attempt
        vec!["7,8,9".into(), "7,8,9".into()], // chunk B
        vec!["1,2,3,7,8,9".into()],           // merge
    ]);

    let mut controller = Controller::new(graph, &model, &ListPrompter, &ListParser, problem);
    controller.run().await.expect("the decomposition runs");

    let finals = controller.final_thoughts();
    assert_eq!(finals.len(), 1, "KeepBestN(1) leaves exactly one answer");
    assert_eq!(state_list(&finals[0].state), vec![1, 2, 3, 7, 8, 9]);
    assert_eq!(
        finals[0].score(),
        0.0,
        "the merged list is a perfect sort of the input",
    );
}

/// Score against the ORIGINAL input, which is what makes a dropped element
/// detectable at a chunk boundary.
fn local_scorer(input: Vec<u32>) -> OpKind {
    OpKind::Score {
        num_samples: 1,
        combined: false,
        scorer: Some(Box::new(move |states| {
            states
                .iter()
                .map(|state| {
                    let output = state_list(state);
                    // A chunk is scored against the slice of the input it could
                    // contain; using the whole input would penalize every chunk
                    // for the elements it was never given. Restricting to the
                    // values present keeps the metric meaningful per chunk.
                    let relevant: Vec<u32> = input
                        .iter()
                        .copied()
                        .filter(|value| output.contains(value))
                        .collect();
                    sorting_error_scope(&relevant, &output) as f64
                })
                .collect()
        })),
    }
}

/// The lossy sort attempt must lose to the faithful one. This is the pruning
/// step that makes independent resampling worth paying for.
#[tokio::test]
async fn keep_best_n_prunes_the_lossy_attempt() {
    let input: Vec<u32> = vec![3, 1, 2];
    let mut graph = GraphOfOperations::new();
    let generated = graph.append(generate(1, 2));
    let scored = graph.add(local_scorer(input.clone()), &[generated]);
    graph.add(
        OpKind::KeepBestN {
            n: 1,
            higher_is_better: false,
        },
        &[scored],
    );

    let model = ScriptedModel::new(vec![vec!["1,3,2".into(), "1,2,3".into()]]);
    let mut controller =
        Controller::new(graph, &model, &ListPrompter, &ListParser, list_state(input));
    controller.run().await.unwrap();

    let finals = controller.final_thoughts();
    assert_eq!(finals.len(), 1);
    assert_eq!(
        state_list(&finals[0].state),
        vec![1, 2, 3],
        "the out-of-order attempt is pruned",
    );
}

/// Ranking unscored thoughts would silently favour them: the default score is
/// 0.0, which under an error-scope metric is the BEST possible value.
#[tokio::test]
async fn keep_best_n_refuses_unscored_thoughts() {
    let mut graph = GraphOfOperations::new();
    let generated = graph.append(generate(1, 1));
    graph.add(
        OpKind::KeepBestN {
            n: 1,
            higher_is_better: false,
        },
        &[generated],
    );

    let model = ScriptedModel::new(vec![vec!["1,2,3".into()]]);
    let mut controller = Controller::new(
        graph,
        &model,
        &ListPrompter,
        &ListParser,
        list_state(vec![3, 2, 1]),
    );
    let error = controller.run().await.expect_err("must refuse to rank");
    assert!(
        matches!(error, xai_grok_got::GotError::UnscoredThoughts { .. }),
        "got {error:?}",
    );
}

/// A decomposition that cannot run is rejected before a single paid call.
#[tokio::test]
async fn a_graph_rooted_at_a_consuming_operation_is_rejected_before_any_query() {
    let mut graph = GraphOfOperations::new();
    graph.append(OpKind::Aggregate { num_responses: 1 });

    let model = ScriptedModel::new(vec![]);
    let mut controller = Controller::new(
        graph,
        &model,
        &ListPrompter,
        &ListParser,
        ThoughtState::new(),
    );
    let error = controller.run().await.expect_err("must reject");
    assert!(
        matches!(error, xai_grok_got::GotError::RootConsumesThoughts { .. }),
        "got {error:?}",
    );
    assert_eq!(model.calls(), 0, "validation must precede spending");
}

/// The refine self-loop, bounded. The paper's own appendix shows improvement
/// steps making answers worse, so the bound is load-bearing, not defensive.
#[tokio::test]
async fn validate_and_improve_stops_at_the_try_limit() {
    let mut graph = GraphOfOperations::new();
    let generated = graph.append(generate(1, 1));
    graph.add(
        OpKind::ValidateAndImprove {
            num_samples: 1,
            improve: true,
            num_tries: 2,
            validator: None,
        },
        &[generated],
    );

    // Never returns "valid": validate, improve, validate, improve, validate.
    let model = ScriptedModel::new(vec![
        vec!["1,2,3".into()],
        vec!["invalid".into()],
        vec!["1,2".into()],
        vec!["invalid".into()],
        vec!["1".into()],
        vec!["invalid".into()],
    ]);
    let mut controller = Controller::new(
        graph,
        &model,
        &ListPrompter,
        &ListParser,
        list_state(vec![3, 2, 1]),
    );
    controller.run().await.unwrap();

    assert_eq!(
        model.calls(),
        6,
        "1 generate + 3 validations + 2 improvements, then the limit stops it",
    );
    let finals = controller.final_thoughts();
    assert_eq!(finals.len(), 1, "only the last attempt of the chain leaves");
    assert!(
        finals[0].is_validated() && !finals[0].is_valid(),
        "a thought that never validated is reported as failed, not unassessed",
    );
}

/// `KeepValid` keeps the never-validated and drops the validated-and-failed.
#[tokio::test]
async fn keep_valid_distinguishes_unassessed_from_failed() {
    let mut graph = GraphOfOperations::new();
    let generated = graph.append(generate(2, 1));
    let checked = graph.add(
        OpKind::ValidateAndImprove {
            num_samples: 1,
            improve: false,
            num_tries: 0,
            // Only lists of length >= 2 are acceptable.
            validator: Some(Box::new(|state| state_list(state).len() >= 2)),
        },
        &[generated],
    );
    graph.add(OpKind::KeepValid, &[checked]);

    let model = ScriptedModel::new(vec![vec!["1,2|9".into()]]);
    let mut controller = Controller::new(
        graph,
        &model,
        &ListPrompter,
        &ListParser,
        list_state(vec![1, 2, 9]),
    );
    controller.run().await.unwrap();

    let finals = controller.final_thoughts();
    assert_eq!(finals.len(), 1, "the single-element thought is dropped");
    assert_eq!(state_list(&finals[0].state), vec![1, 2]);
}

/// `GroundTruth` is benchmarking instrumentation: it marks solved thoughts
/// without changing them.
#[tokio::test]
async fn ground_truth_marks_solutions() {
    let mut graph = GraphOfOperations::new();
    let generated = graph.append(generate(2, 1));
    graph.add(
        OpKind::GroundTruth {
            evaluator: Box::new(|state| state_list(state) == vec![1, 2, 3]),
        },
        &[generated],
    );

    let model = ScriptedModel::new(vec![vec!["1,2,3|3,2,1".into()]]);
    let mut controller = Controller::new(
        graph,
        &model,
        &ListPrompter,
        &ListParser,
        list_state(vec![3, 2, 1]),
    );
    controller.run().await.unwrap();

    let finals = controller.final_thoughts();
    assert_eq!(finals.len(), 2);
    assert!(finals.iter().all(|t| t.is_compared_to_ground_truth()));
    assert_eq!(finals.iter().filter(|t| t.is_solved()).count(), 1);
}

/// Table 2's actual claim: at equal latency, a fan-in reaches back into every
/// branch while a tree path reaches back only along itself.
#[tokio::test]
async fn aggregation_buys_volume_a_tree_cannot_reach_at_the_same_latency() {
    let mut got = GraphOfOperations::new();
    let split = got.append(generate(2, 1));
    // `Selector` routes one chunk down each branch. Without it a branch
    // operation fires once per input thought, so both branches would
    // redundantly reprocess both chunks.
    let left = got.add(
        OpKind::Selector {
            selector: Box::new(|thoughts| thoughts.iter().take(1).cloned().collect()),
        },
        &[split],
    );
    let right = got.add(
        OpKind::Selector {
            selector: Box::new(|thoughts| thoughts.iter().skip(1).take(1).cloned().collect()),
        },
        &[split],
    );
    let merge = got.add(OpKind::Aggregate { num_responses: 1 }, &[left, right]);

    // Selectors cost nothing; only the split and the merge query the model.
    let model = ScriptedModel::new(vec![vec!["1,2|3,4".into()], vec!["1,2,3,4".into()]]);
    let mut controller = Controller::new(
        got,
        &model,
        &ListPrompter,
        &ListParser,
        list_state(vec![4, 3, 2, 1]),
    );
    controller.run().await.unwrap();
    let graph = controller.graph();

    assert_eq!(graph.latency(), 2, "split -> branch -> merge");
    let merge_volume = graph.thought_volume(merge);
    let branch_volume = graph.thought_volume(left);
    assert!(
        merge_volume > branch_volume,
        "the merge sees both branches ({merge_volume}) where a branch sees one ({branch_volume})",
    );
    assert_eq!(
        graph.operation_volume(merge),
        3,
        "split plus both branches are all reachable from the merge",
    );
}

/// Cost is accumulated across every round-trip, so a decomposition can be
/// priced the way the paper prices its plots.
#[tokio::test]
async fn usage_accumulates_across_the_run() {
    let per_call = Usage {
        prompt_tokens: 100,
        completion_tokens: 20,
        cost: 0.5,
    };
    let mut graph = GraphOfOperations::new();
    let split = graph.append(generate(2, 1));
    graph.add(OpKind::Aggregate { num_responses: 1 }, &[split]);

    let model = ScriptedModel::new(vec![vec!["1,2|3,4".into()], vec!["1,2,3,4".into()]])
        .with_usage(per_call);
    let mut controller = Controller::new(
        graph,
        &model,
        &ListPrompter,
        &ListParser,
        list_state(vec![4, 3, 2, 1]),
    );
    controller.run().await.unwrap();

    let usage = controller.usage();
    assert_eq!(usage.prompt_tokens, 200, "two calls were made");
    assert_eq!(usage.completion_tokens, 40);
    assert_eq!(usage.cost, 1.0);
}

/// Positive-score reporting from Appendix A, over a real error scope.
#[test]
fn positive_score_reports_the_papers_higher_is_better_view() {
    let input = [3, 1, 2, 1];
    let error = sorting_error_scope(&input, &[1, 2, 3]);
    assert_eq!(error, 1);
    assert_eq!(positive_score(input.len(), error), 3);
}

/// GoT's Table 9 reproduced: refinement is not monotone. Successive "improve"
/// steps return answers with MORE errors than they started with, and `Improve`
/// keeps whichever came last.
#[tokio::test]
async fn improve_can_return_a_worse_answer_than_it_was_given() {
    let input: Vec<u32> = vec![1, 2, 3];
    let mut graph = GraphOfOperations::new();
    let generated = graph.append(generate(1, 1));
    graph.add(OpKind::Improve, &[generated]);

    let model = ScriptedModel::new(vec![
        vec!["1,2,3".into()], // a correct answer
        vec!["1,3".into()],   // the "improvement" drops an element
    ]);
    let mut controller = Controller::new(
        graph,
        &model,
        &ListPrompter,
        &ListParser,
        list_state(input.clone()),
    );
    controller.run().await.unwrap();

    let finals = controller.final_thoughts();
    let result = state_list(&finals[0].state);
    assert_eq!(result, vec![1, 3], "Improve keeps the last attempt");
    assert!(
        sorting_error_scope(&input, &result) > 0,
        "which is measurably worse than what it started from",
    );
}

/// `Repair` on the identical script returns the input it was given, because it
/// scores every attempt and keeps the best. Non-regression by construction.
#[tokio::test]
async fn repair_never_returns_worse_than_its_input() {
    let input: Vec<u32> = vec![1, 2, 3];
    let mut graph = GraphOfOperations::new();
    let generated = graph.append(generate(1, 1));
    graph.add(
        OpKind::Repair {
            num_tries: 3,
            scorer: error_scope_scorer(input.clone()),
            higher_is_better: false,
            accept_at: None,
        },
        &[generated],
    );

    let model = ScriptedModel::new(vec![
        vec!["1,2,3".into()], // correct
        vec!["1,3".into()],   // worse
        vec!["3,1".into()],   // worse still
        vec!["1".into()],     // worst
    ]);
    let mut controller = Controller::new(
        graph,
        &model,
        &ListPrompter,
        &ListParser,
        list_state(input.clone()),
    );
    controller.run().await.unwrap();

    let finals = controller.final_thoughts();
    assert_eq!(
        state_list(&finals[0].state),
        vec![1, 2, 3],
        "every attempt was worse, so the original survives",
    );
    assert_eq!(finals[0].score(), 0.0, "and its score comes with it");
}

/// The upside case: a bad start genuinely repaired, and the budget respected.
#[tokio::test]
async fn repair_adopts_a_genuine_improvement_and_stops_at_the_bar() {
    let input: Vec<u32> = vec![1, 2, 3];
    let mut graph = GraphOfOperations::new();
    let generated = graph.append(generate(1, 1));
    graph.add(
        OpKind::Repair {
            num_tries: 5,
            scorer: error_scope_scorer(input.clone()),
            higher_is_better: false,
            // Stop as soon as the answer is perfect rather than burning budget.
            accept_at: Some(0.0),
        },
        &[generated],
    );

    let model = ScriptedModel::new(vec![
        vec!["3,2,1".into()], // bad start
        vec!["1,3,2".into()], // better
        vec!["1,2,3".into()], // perfect -> loop must stop here
    ]);
    let mut controller =
        Controller::new(graph, &model, &ListPrompter, &ListParser, list_state(input));
    controller.run().await.unwrap();

    assert_eq!(
        state_list(&controller.final_thoughts()[0].state),
        vec![1, 2, 3]
    );
    assert_eq!(
        model.calls(),
        3,
        "the acceptance bar stops the loop instead of spending all 5 tries",
    );
}

fn error_scope_scorer(input: Vec<u32>) -> xai_grok_got::ScoringFn {
    Box::new(move |states| {
        states
            .iter()
            .map(|state| sorting_error_scope(&input, &state_list(state)) as f64)
            .collect()
    })
}
