use crate::bounty::{BountyError, BountyManager};
use crate::charter::Charter;
use crate::collusion::CollusionDetector;
use crate::ledger::{BudgetError, TokenLedger};
use crate::reporting::{
    AgentInvoker, BountyReport, CycleOutcome, SolverPrompt, SolverReport, SwarmProvider,
    ValidationReport, ValidatorPrompt,
};
use crate::settlement::SettlementEngine;
use crate::solver::SolverPool;
use crate::trust::TrustRegistry;
use crate::types::{AgentId, MarketState, Solution, ValidationChallenge, ValidationVerdict};
use crate::validator::ValidationPool;
use std::collections::HashMap;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum OrchestratorError {
    #[error(transparent)]
    Bounty(#[from] BountyError),
    #[error(transparent)]
    Budget(#[from] BudgetError),
    #[error("charter violation: {0}")]
    CharterViolation(String),
}

pub struct MarketOrchestrator {
    ledger: TokenLedger,
    trust: TrustRegistry,
    bounties: BountyManager,
    charter: Charter,
    solvers: SolverPool,
    validators: ValidationPool,
    collusion: CollusionDetector,
    validations_by_bounty: HashMap<String, Vec<ValidationReport>>,
    settlements_by_bounty: HashMap<String, crate::types::Settlement>,
}

impl MarketOrchestrator {
    pub fn new(charter: Charter) -> Self {
        let budget = charter.max_budget_per_bounty;
        let spawn_fee = charter.spawn_fee;
        Self {
            ledger: TokenLedger::new(budget, budget, spawn_fee),
            trust: TrustRegistry::new(),
            bounties: BountyManager::new(),
            charter,
            solvers: SolverPool::new(),
            validators: ValidationPool::new(),
            collusion: CollusionDetector::new(),
            validations_by_bounty: HashMap::new(),
            settlements_by_bounty: HashMap::new(),
        }
    }

    pub fn ledger(&self) -> &TokenLedger {
        &self.ledger
    }

    pub fn trust(&self) -> &TrustRegistry {
        &self.trust
    }

    pub fn bounties(&self) -> &BountyManager {
        &self.bounties
    }

    pub fn charter(&self) -> &Charter {
        &self.charter
    }

    pub fn solvers(&self) -> &SolverPool {
        &self.solvers
    }

    pub fn validators(&self) -> &ValidationPool {
        &self.validators
    }

    pub fn collusion(&self) -> &CollusionDetector {
        &self.collusion
    }

    pub fn post_bounty(
        &mut self,
        description: String,
        reward: u64,
        criteria: String,
        max_solvers: Option<u8>,
    ) -> Result<String, OrchestratorError> {
        if reward > self.charter.max_budget_per_bounty {
            return Err(OrchestratorError::CharterViolation(format!(
                "reward {reward} exceeds max_budget_per_bounty {}",
                self.charter.max_budget_per_bounty
            )));
        }
        let id = self.bounties.post(
            description,
            reward,
            criteria,
            Duration::from_secs(300),
            max_solvers.unwrap_or(self.charter.max_solvers),
        );
        self.bounties.transition(&id, MarketState::Open)?;
        Ok(id)
    }

    fn effective_token_limit(&self, prompt_max_tokens: u64) -> u64 {
        prompt_max_tokens.min(self.charter.max_token_spend_per_agent)
    }

    fn check_actual_tokens(
        &self,
        agent_id: &AgentId,
        role: &str,
        tokens_consumed: u64,
        max_tokens: u64,
    ) -> Result<(), OrchestratorError> {
        if tokens_consumed > max_tokens {
            return Err(OrchestratorError::CharterViolation(format!(
                "{role} {agent_id} consumed {tokens_consumed} tokens, exceeding limit {max_tokens}"
            )));
        }
        Ok(())
    }

    fn validate_solution_identity(
        &self,
        solution: &Solution,
        agent_id: &AgentId,
        bounty_id: &str,
    ) -> Result<(), OrchestratorError> {
        if &solution.agent_id != agent_id || solution.bounty_id != bounty_id {
            return Err(OrchestratorError::CharterViolation(format!(
                "solver {agent_id} returned mismatched solution identity agent={} bounty={}",
                solution.agent_id, solution.bounty_id
            )));
        }
        Ok(())
    }

    fn mark_failed(&mut self, bounty_id: &str) {
        let _ = self.bounties.transition(bounty_id, MarketState::Failed);
    }

    pub async fn run_bounty_cycle(
        &mut self,
        bounty_id: &str,
        invoker: &dyn AgentInvoker,
        swarm: &dyn SwarmProvider,
        n_solvers: u8,
        n_validators_per_solution: u8,
    ) -> Result<CycleOutcome, OrchestratorError> {
        let bounty = self
            .bounties
            .get(bounty_id)
            .ok_or_else(|| BountyError::NotFound(bounty_id.to_owned()))?
            .clone();
        let actual_solvers = (n_solvers as usize)
            .min(self.charter.max_solvers as usize)
            .min(bounty.max_solvers as usize)
            .max(1);
        let actual_validators = (n_validators_per_solution as usize)
            .min(self.charter.max_validators as usize)
            .max(1);

        // Fail before any solver insertion, spawn fee debit, or worktree creation.
        self.bounties
            .validate_transition(bounty_id, MarketState::Bidding)?;

        let estimated_token_cost = self.ledger.gate_check(
            "grok-economy-estimate",
            self.charter.max_token_spend_per_agent * actual_solvers as u64,
            0,
        )?;
        let max_spawned_agents =
            actual_solvers.saturating_add(actual_solvers.saturating_mul(actual_validators));
        let estimated_spawn_fees = self
            .charter
            .spawn_fee
            .saturating_mul(max_spawned_agents as u64);
        self.ledger
            .gate_amount(estimated_token_cost.saturating_add(estimated_spawn_fees))?;

        let mut prompts = Vec::new();
        for i in 0..actual_solvers {
            let agent_id = AgentId::market_stable("solver", i);
            self.trust.register(agent_id.clone());
            self.solvers.spawn_with_id(agent_id.clone(), bounty_id);
            self.ledger.debit_spawn(&agent_id)?;
            let worktree = swarm.create_worktree(bounty_id, &agent_id).await;
            if let Some(solver) = self.solvers.get_mut_for_bounty(&agent_id, bounty_id) {
                solver.start(worktree.clone());
            }
            prompts.push(SolverPrompt {
                bounty_id: bounty_id.to_owned(),
                bounty_description: bounty.description.clone(),
                acceptance_criteria: bounty.acceptance_criteria.clone(),
                agent_id,
                worktree,
                max_tokens: (bounty.reward / (actual_solvers as u64 + 1)).max(1),
            });
        }

        self.bounties.transition(bounty_id, MarketState::Bidding)?;
        self.bounties
            .transition(bounty_id, MarketState::Executing)?;
        for prompt in prompts {
            let agent_id = prompt.agent_id.clone();
            let worktree = prompt.worktree.clone();
            let max_tokens = self.effective_token_limit(prompt.max_tokens);
            match invoker.invoke_solver(prompt).await {
                Ok(solution) => {
                    let tokens = solution.tokens_consumed;
                    let validation = self
                        .validate_solution_identity(&solution, &agent_id, bounty_id)
                        .and_then(|()| {
                            self.check_actual_tokens(&agent_id, "solver", tokens, max_tokens)
                        });
                    if let Err(err) = validation {
                        if let Some(solver) = self.solvers.get_mut_for_bounty(&agent_id, bounty_id)
                        {
                            solver.abandon();
                        }
                        if let Some(path) = worktree {
                            swarm.remove_worktree(&path).await;
                        }
                        self.mark_failed(bounty_id);
                        return Err(err);
                    }
                    if let Err(err) = self.ledger.record_usage(&agent_id, "solver", tokens, 0) {
                        if let Some(solver) = self.solvers.get_mut_for_bounty(&agent_id, bounty_id)
                        {
                            solver.abandon();
                        }
                        if let Some(path) = worktree {
                            swarm.remove_worktree(&path).await;
                        }
                        self.mark_failed(bounty_id);
                        return Err(err.into());
                    }
                    if let Some(solver) = self.solvers.get_mut_for_bounty(&agent_id, bounty_id) {
                        solver.submit(solution);
                    }
                }
                Err(_) => {
                    if let Some(solver) = self.solvers.get_mut_for_bounty(&agent_id, bounty_id) {
                        solver.abandon();
                    }
                    if let Some(path) = worktree {
                        swarm.remove_worktree(&path).await;
                    }
                }
            }
        }

        self.bounties
            .transition(bounty_id, MarketState::Validating)?;
        self.validations_by_bounty.remove(bounty_id);
        let mut validation_reports = Vec::new();
        let solutions: Vec<Solution> = self
            .solvers
            .completed_solutions_for_bounty(bounty_id)
            .into_iter()
            .cloned()
            .collect();
        for solution in &solutions {
            for idx in 0..actual_validators {
                let validator_id = AgentId::market_stable(
                    "validator",
                    validation_reports.len().saturating_add(idx),
                );
                if validator_id == solution.agent_id {
                    continue;
                }
                self.trust.register(validator_id.clone());
                if let Err(err) = self.ledger.debit_spawn(&validator_id) {
                    self.mark_failed(bounty_id);
                    return Err(err.into());
                }
                let prompt = ValidatorPrompt {
                    bounty_id: bounty_id.to_owned(),
                    bounty_description: bounty.description.clone(),
                    solution: solution.clone(),
                    validator_id: validator_id.clone(),
                    max_tokens: (bounty.reward / 10).max(1),
                };
                let max_tokens = self.effective_token_limit(prompt.max_tokens);
                let Ok(outcome) = invoker.invoke_validator(prompt).await else {
                    continue;
                };
                if let Err(err) = self.check_actual_tokens(
                    &validator_id,
                    "validator",
                    outcome.tokens_consumed,
                    max_tokens,
                ) {
                    self.mark_failed(bounty_id);
                    return Err(err);
                }
                if let Err(err) =
                    self.ledger
                        .record_usage(&validator_id, "validator", outcome.tokens_consumed, 0)
                {
                    self.mark_failed(bounty_id);
                    return Err(err.into());
                }
                let session_idx = self
                    .validators
                    .start_session(
                        validator_id.clone(),
                        solution.agent_id.clone(),
                        bounty_id.to_owned(),
                    )
                    .map_err(|e| OrchestratorError::CharterViolation(e.to_string()))?;
                let challenge = ValidationChallenge {
                    validator_id: validator_id.clone(),
                    solution_agent_id: solution.agent_id.clone(),
                    bounty_id: bounty_id.to_owned(),
                    proposed_flaw: outcome.flaw.clone().unwrap_or_default(),
                    test_code: outcome.test_code.clone(),
                    confidence: outcome.confidence,
                };
                self.validators
                    .submit_challenge(session_idx, challenge)
                    .map_err(|e| OrchestratorError::CharterViolation(e.to_string()))?;
                if !self
                    .validators
                    .session(session_idx)
                    .is_some_and(|session| session.is_complete())
                {
                    self.validators
                        .submit_defense(
                            session_idx,
                            "(solver defense omitted in deterministic Grok economy v1)".into(),
                        )
                        .ok();
                    let test_fails = match outcome.test_code.as_deref() {
                        Some(test) => {
                            invoker
                                .adjudicate_test(test, solution.worktree_path.as_deref())
                                .await
                        }
                        None => false,
                    };
                    self.validators
                        .adjudicate(session_idx, test_fails)
                        .map_err(|e| OrchestratorError::CharterViolation(e.to_string()))?;
                }
                let verdict = self
                    .validators
                    .session(session_idx)
                    .and_then(|session| session.verdict())
                    .expect("complete session has verdict");
                self.collusion.record(&validator_id, verdict);
                validation_reports.push(ValidationReport {
                    validator_id,
                    solution_agent_id: solution.agent_id.clone(),
                    verdict,
                    flaw: outcome.flaw,
                    test_code: outcome.test_code,
                    confidence: outcome.confidence,
                });
            }
        }

        self.bounties.transition(bounty_id, MarketState::Settling)?;
        let disqualified = validation_reports
            .iter()
            .filter(|validation| validation.verdict == ValidationVerdict::FlawUpheld)
            .map(|validation| validation.solution_agent_id.clone())
            .collect::<std::collections::HashSet<_>>();
        let winner = self
            .solvers
            .rank_solutions_for_bounty(bounty_id)
            .into_iter()
            .find(|solution| !disqualified.contains(&solution.agent_id))
            .map(|solution| solution.agent_id.clone());
        let winning_solution = winner.as_ref().and_then(|winner| {
            self.solvers
                .completed_solutions_for_bounty(bounty_id)
                .into_iter()
                .find(|solution| &solution.agent_id == winner)
                .cloned()
        });
        let validator_verdicts = validation_reports
            .iter()
            .map(|validation| (validation.validator_id.clone(), validation.verdict))
            .collect::<Vec<_>>();
        let settlement = SettlementEngine::settle(
            bounty_id,
            bounty.reward,
            winner.as_ref(),
            &validator_verdicts,
            &self.charter,
            &mut self.ledger,
            &mut self.trust,
        );
        self.validations_by_bounty
            .insert(bounty_id.to_owned(), validation_reports);
        self.settlements_by_bounty
            .insert(bounty_id.to_owned(), settlement.clone());
        self.bounties.transition(bounty_id, MarketState::Complete)?;
        let report = self.report(bounty_id).expect("completed bounty has report");
        Ok(CycleOutcome {
            settlement,
            winning_solution,
            report,
        })
    }

    pub fn report(&self, bounty_id: &str) -> Option<BountyReport> {
        let bounty = self.bounties.get(bounty_id)?;
        let solvers = self
            .solvers
            .all()
            .iter()
            .filter(|solver| solver.bounty_id == bounty_id)
            .map(|solver| SolverReport {
                agent_id: solver.id.clone(),
                status: format!("{:?}", solver.status),
                tokens_consumed: solver.tokens_consumed,
                compiles: solver.solution.as_ref().and_then(|s| s.compiles),
                tests_pass: solver.solution.as_ref().and_then(|s| s.tests_pass),
                suspicious: solver.solution.as_ref().is_some_and(|s| s.suspicious),
                worktree_path: solver.worktree_path.clone(),
            })
            .collect();
        let mut warnings = self
            .collusion
            .flagged_agents()
            .into_iter()
            .map(|(id, kind)| format!("{id} flagged for {kind}"))
            .collect::<Vec<_>>();
        let settlement = self.settlements_by_bounty.get(bounty_id).cloned();
        if settlement
            .as_ref()
            .and_then(|s| s.winner.as_ref())
            .is_none()
        {
            warnings.push("no winning solution survived validation".into());
        }
        Some(BountyReport {
            bounty_id: bounty.id.clone(),
            description: bounty.description.clone(),
            acceptance_criteria: bounty.acceptance_criteria.clone(),
            state: format!("{:?}", bounty.state),
            solvers,
            validations: self
                .validations_by_bounty
                .get(bounty_id)
                .cloned()
                .unwrap_or_default(),
            settlement,
            total_spent: self.ledger.total_spent(),
            remaining_budget: self.ledger.remaining(),
            warnings,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reporting::{SwarmProvider, ValidatorOutcome};
    use std::sync::Mutex;

    struct FakeSwarm;
    #[async_trait::async_trait]
    impl SwarmProvider for FakeSwarm {
        async fn create_worktree(
            &self,
            bounty_id: &str,
            agent_id: &AgentId,
        ) -> Option<std::path::PathBuf> {
            Some(std::path::PathBuf::from(format!(
                "/worktrees/{bounty_id}/{}",
                agent_id.label()
            )))
        }
        async fn remove_worktree(&self, _path: &std::path::Path) {}
    }

    #[derive(Default)]
    struct CountingSwarm {
        created: Mutex<Vec<std::path::PathBuf>>,
    }

    #[async_trait::async_trait]
    impl SwarmProvider for CountingSwarm {
        async fn create_worktree(
            &self,
            bounty_id: &str,
            agent_id: &AgentId,
        ) -> Option<std::path::PathBuf> {
            let path =
                std::path::PathBuf::from(format!("/worktrees/{bounty_id}/{}", agent_id.label()));
            self.created.lock().unwrap().push(path.clone());
            Some(path)
        }

        async fn remove_worktree(&self, _path: &std::path::Path) {}
    }

    struct FakeInvoker {
        fail_first_solver: bool,
        flaw_solver: Option<String>,
        solver_tokens: u64,
        validator_tokens: u64,
        calls: Mutex<Vec<String>>,
    }

    impl FakeInvoker {
        fn new(fail_first_solver: bool, flaw_solver: Option<String>) -> Self {
            Self {
                fail_first_solver,
                flaw_solver,
                solver_tokens: 100,
                validator_tokens: 25,
                calls: Mutex::new(Vec::new()),
            }
        }

        fn with_solver_tokens(mut self, tokens: u64) -> Self {
            self.solver_tokens = tokens;
            self
        }

        fn with_validator_tokens(mut self, tokens: u64) -> Self {
            self.validator_tokens = tokens;
            self
        }
    }

    #[async_trait::async_trait]
    impl AgentInvoker for FakeInvoker {
        async fn invoke_solver(&self, prompt: SolverPrompt) -> Result<Solution, String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("solver:{}", prompt.agent_id));
            if self.fail_first_solver && prompt.agent_id.label() == "solver-0" {
                return Err("boom".into());
            }
            Ok(Solution {
                agent_id: prompt.agent_id,
                bounty_id: prompt.bounty_id,
                patch: "diff".into(),
                explanation: "fixed".into(),
                self_assessment: 0.8,
                tokens_consumed: self.solver_tokens,
                compiles: Some(true),
                tests_pass: Some(true),
                suspicious: false,
                worktree_path: prompt.worktree,
            })
        }

        async fn invoke_validator(
            &self,
            prompt: ValidatorPrompt,
        ) -> Result<ValidatorOutcome, String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("validator:{}", prompt.validator_id));
            let flaw = self.flaw_solver.as_deref() == Some(prompt.solution.agent_id.label());
            Ok(ValidatorOutcome {
                flaw: flaw.then_some("repro failure".into()),
                test_code: flaw.then_some("fail".into()),
                confidence: if flaw { 0.8 } else { 0.99 },
                tokens_consumed: self.validator_tokens,
            })
        }

        async fn adjudicate_test(
            &self,
            test_code: &str,
            _solver_worktree: Option<&std::path::Path>,
        ) -> bool {
            test_code == "fail"
        }
    }

    #[tokio::test]
    async fn exported_cycle_tolerates_failed_solver_and_settles_winner() {
        let mut orchestrator = MarketOrchestrator::new(Charter::default());
        let bounty = orchestrator
            .post_bounty(
                "Implement feature".into(),
                1_000,
                "Tests pass".into(),
                Some(2),
            )
            .unwrap();
        let outcome = orchestrator
            .run_bounty_cycle(&bounty, &FakeInvoker::new(true, None), &FakeSwarm, 2, 1)
            .await
            .unwrap();
        assert_eq!(
            outcome.settlement.winner.as_ref().unwrap().label(),
            "solver-1"
        );
        assert!(outcome.report.to_markdown().contains("## Settlement"));
        assert_eq!(outcome.report.to_json()["state"], "Complete");
        assert!(orchestrator.ledger().total_spent() > 0);
    }

    #[tokio::test]
    async fn rerunning_completed_bounty_rejects_before_side_effects() {
        let mut orchestrator = MarketOrchestrator::new(Charter::default());
        let bounty = orchestrator
            .post_bounty("One-shot task".into(), 1_000, "tests pass".into(), Some(1))
            .unwrap();
        let invoker = FakeInvoker::new(false, None);
        let swarm = CountingSwarm::default();
        orchestrator
            .run_bounty_cycle(&bounty, &invoker, &swarm, 1, 1)
            .await
            .unwrap();
        let solver_count = orchestrator.solvers().all().len();
        let transaction_count = orchestrator.ledger().transactions().len();
        let worktree_count = swarm.created.lock().unwrap().len();

        let err = orchestrator
            .run_bounty_cycle(&bounty, &invoker, &swarm, 1, 1)
            .await
            .unwrap_err();
        assert!(
            matches!(err, OrchestratorError::Bounty(BountyError::AlreadyComplete)),
            "expected AlreadyComplete without side effects, got {err:?}"
        );
        assert_eq!(orchestrator.solvers().all().len(), solver_count);
        assert_eq!(
            orchestrator.ledger().transactions().len(),
            transaction_count
        );
        assert_eq!(swarm.created.lock().unwrap().len(), worktree_count);
    }

    #[tokio::test]
    async fn repeated_bounties_do_not_mutate_prior_stable_solver_ids() {
        let mut orchestrator = MarketOrchestrator::new(Charter::default());
        let invoker = FakeInvoker::new(false, None);
        let first = orchestrator
            .post_bounty("First".into(), 1_000, "tests pass".into(), Some(1))
            .unwrap();
        let second = orchestrator
            .post_bounty("Second".into(), 1_000, "tests pass".into(), Some(1))
            .unwrap();

        let first_outcome = orchestrator
            .run_bounty_cycle(&first, &invoker, &FakeSwarm, 1, 1)
            .await
            .unwrap();
        let first_winner = first_outcome.settlement.winner.clone();
        let first_report_before = orchestrator.report(&first).unwrap();

        let second_outcome = orchestrator
            .run_bounty_cycle(&second, &invoker, &FakeSwarm, 1, 1)
            .await
            .unwrap();
        assert_eq!(
            second_outcome.settlement.winner.as_ref().unwrap().label(),
            "solver-0",
            "stable solver identity is reused across bounties for trust"
        );
        assert_eq!(orchestrator.solvers().all().len(), 2);

        let first_solver = orchestrator
            .solvers()
            .get_for_bounty(&AgentId::market_stable("solver", 0), &first)
            .expect("first bounty solver remains addressable");
        let second_solver = orchestrator
            .solvers()
            .get_for_bounty(&AgentId::market_stable("solver", 0), &second)
            .expect("second bounty solver remains addressable");
        assert_ne!(first_solver.bounty_id, second_solver.bounty_id);
        assert!(
            first_solver
                .worktree_path
                .as_ref()
                .unwrap()
                .to_string_lossy()
                .contains(&first),
            "first bounty worktree must not be overwritten by second run"
        );
        assert!(
            second_solver
                .worktree_path
                .as_ref()
                .unwrap()
                .to_string_lossy()
                .contains(&second),
            "second bounty should get its own solver state"
        );

        let first_report_after = orchestrator.report(&first).unwrap();
        assert_eq!(
            first_report_after.settlement.as_ref().unwrap().winner,
            first_winner
        );
        assert_eq!(
            first_report_after.validations.len(),
            first_report_before.validations.len(),
            "report for first bounty must not show second bounty validations"
        );
        assert!(first_report_after.solvers.iter().all(|solver| {
            solver
                .worktree_path
                .as_ref()
                .unwrap()
                .to_string_lossy()
                .contains(&first)
        }));
    }

    #[tokio::test]
    async fn budget_gate_rejects_before_spawning_solvers() {
        let mut charter = Charter::default();
        charter.max_budget_per_bounty = 1;
        charter.max_token_spend_per_agent = 1_000_000;
        let mut orchestrator = MarketOrchestrator::new(charter);
        let bounty = orchestrator
            .post_bounty("Expensive task".into(), 1, "do work".into(), Some(2))
            .unwrap();
        let err = orchestrator
            .run_bounty_cycle(&bounty, &FakeInvoker::new(false, None), &FakeSwarm, 2, 1)
            .await
            .unwrap_err();
        assert!(matches!(err, OrchestratorError::Budget(_)));
        assert!(
            orchestrator.solvers().all().is_empty(),
            "budget preflight must fail before spawning solvers"
        );
    }

    #[tokio::test]
    async fn solver_actual_token_overrun_fails_before_completion_or_usage_debit() {
        let mut orchestrator = MarketOrchestrator::new(Charter::default());
        let bounty = orchestrator
            .post_bounty(
                "Malicious solver".into(),
                1_000,
                "stay bounded".into(),
                Some(1),
            )
            .unwrap();
        let err = orchestrator
            .run_bounty_cycle(
                &bounty,
                &FakeInvoker::new(false, None).with_solver_tokens(10_000),
                &FakeSwarm,
                1,
                1,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&err, OrchestratorError::CharterViolation(message) if message.contains("solver") && message.contains("tokens")),
            "expected solver token charter violation, got {err:?}"
        );
        assert_eq!(
            orchestrator.bounties().get(&bounty).unwrap().state,
            MarketState::Failed
        );
        assert!(
            orchestrator.ledger().total_spent() <= orchestrator.charter().max_budget_per_bounty
        );
        assert_eq!(orchestrator.validators().session_count(), 0);
        assert!(
            orchestrator
                .solvers()
                .all()
                .iter()
                .all(|solver| { solver.status != crate::solver::SolverStatus::Completed })
        );
    }

    #[tokio::test]
    async fn validator_actual_token_overrun_fails_before_forging_validation_or_settlement() {
        let mut orchestrator = MarketOrchestrator::new(Charter::default());
        let bounty = orchestrator
            .post_bounty(
                "Malicious validator".into(),
                1_000,
                "stay bounded".into(),
                Some(1),
            )
            .unwrap();
        let err = orchestrator
            .run_bounty_cycle(
                &bounty,
                &FakeInvoker::new(false, None).with_validator_tokens(10_000),
                &FakeSwarm,
                1,
                1,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&err, OrchestratorError::CharterViolation(message) if message.contains("validator") && message.contains("tokens")),
            "expected validator token charter violation, got {err:?}"
        );
        assert_eq!(
            orchestrator.bounties().get(&bounty).unwrap().state,
            MarketState::Failed
        );
        assert!(
            orchestrator.ledger().total_spent() <= orchestrator.charter().max_budget_per_bounty
        );
        assert_eq!(orchestrator.validators().session_count(), 0);
        assert_eq!(orchestrator.report(&bounty).unwrap().state, "Failed");
        assert!(orchestrator.report(&bounty).unwrap().settlement.is_none());
    }

    #[tokio::test]
    async fn validator_flaw_disqualifies_solution_before_settlement() {
        let mut orchestrator = MarketOrchestrator::new(Charter::default());
        let bounty = orchestrator
            .post_bounty(
                "Implement unsafe fix".into(),
                1_000,
                "No flaw".into(),
                Some(1),
            )
            .unwrap();
        let outcome = orchestrator
            .run_bounty_cycle(
                &bounty,
                &FakeInvoker::new(false, Some("solver-0".into())),
                &FakeSwarm,
                1,
                1,
            )
            .await
            .unwrap();
        assert!(outcome.settlement.winner.is_none());
        assert!(
            outcome
                .report
                .validations
                .iter()
                .any(|v| v.verdict == ValidationVerdict::FlawUpheld)
        );
        assert!(
            outcome
                .report
                .warnings
                .iter()
                .any(|w| w.contains("no winning"))
        );
    }

    #[test]
    fn market_orchestrator_state_is_not_publicly_mutable() {
        let source = include_str!("orchestrator.rs");
        for forbidden in [
            concat!("pub ", "ledger:"),
            concat!("pub ", "trust:"),
            concat!("pub ", "bounties:"),
            concat!("pub ", "charter:"),
            concat!("pub ", "solvers:"),
            concat!("pub ", "validators:"),
            concat!("pub ", "collusion:"),
        ] {
            assert!(
                !source.contains(forbidden),
                "market orchestrator exposed forbidden mutable state: {forbidden}"
            );
        }
    }
}
