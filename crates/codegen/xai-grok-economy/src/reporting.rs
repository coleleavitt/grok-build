use crate::types::{AgentId, Settlement, Solution, ValidationVerdict};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct SolverPrompt {
    pub bounty_id: String,
    pub bounty_description: String,
    pub acceptance_criteria: String,
    pub agent_id: AgentId,
    pub worktree: Option<PathBuf>,
    pub max_tokens: u64,
}

#[derive(Debug, Clone)]
pub struct ValidatorPrompt {
    pub bounty_id: String,
    pub bounty_description: String,
    pub solution: Solution,
    pub validator_id: AgentId,
    pub max_tokens: u64,
}

#[derive(Debug, Clone)]
pub struct ValidatorOutcome {
    pub flaw: Option<String>,
    pub test_code: Option<String>,
    pub confidence: f32,
    pub tokens_consumed: u64,
}

#[async_trait::async_trait]
pub trait AgentInvoker: Send + Sync {
    async fn invoke_solver(&self, prompt: SolverPrompt) -> Result<Solution, String>;
    async fn invoke_validator(&self, prompt: ValidatorPrompt) -> Result<ValidatorOutcome, String>;
    async fn adjudicate_test(
        &self,
        test_code: &str,
        solver_worktree: Option<&std::path::Path>,
    ) -> bool;
}

#[async_trait::async_trait]
pub trait SwarmProvider: Send + Sync {
    async fn create_worktree(&self, bounty_id: &str, agent_id: &AgentId) -> Option<PathBuf>;
    async fn remove_worktree(&self, path: &std::path::Path);
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolverReport {
    pub agent_id: AgentId,
    pub status: String,
    pub tokens_consumed: u64,
    pub compiles: Option<bool>,
    pub tests_pass: Option<bool>,
    pub suspicious: bool,
    pub worktree_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationReport {
    pub validator_id: AgentId,
    pub solution_agent_id: AgentId,
    pub verdict: ValidationVerdict,
    pub flaw: Option<String>,
    pub test_code: Option<String>,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BountyReport {
    pub bounty_id: String,
    pub description: String,
    pub acceptance_criteria: String,
    pub state: String,
    pub solvers: Vec<SolverReport>,
    pub validations: Vec<ValidationReport>,
    pub settlement: Option<Settlement>,
    pub total_spent: u64,
    pub remaining_budget: u64,
    pub warnings: Vec<String>,
}

impl BountyReport {
    pub fn to_markdown(&self) -> String {
        let mut out = format!("# Bounty {}\n\n{}\n\n", self.bounty_id, self.description);
        out.push_str(&format!("State: `{}`\n\n", self.state));
        out.push_str("## Acceptance criteria\n\n");
        out.push_str(&self.acceptance_criteria);
        out.push_str("\n\n## Solvers\n\n");
        for solver in &self.solvers {
            out.push_str(&format!(
                "- `{}`: {} tokens={}, compiles={:?}, tests={:?}, suspicious={}\n",
                solver.agent_id,
                solver.status,
                solver.tokens_consumed,
                solver.compiles,
                solver.tests_pass,
                solver.suspicious
            ));
        }
        out.push_str("\n## Validations\n\n");
        for validation in &self.validations {
            out.push_str(&format!(
                "- `{}` on `{}`: {:?} confidence={} flaw={:?}\n",
                validation.validator_id,
                validation.solution_agent_id,
                validation.verdict,
                validation.confidence,
                validation.flaw
            ));
        }
        if let Some(settlement) = &self.settlement {
            out.push_str("\n## Settlement\n\n");
            out.push_str(&format!("Winner: {:?}\n", settlement.winner));
            out.push_str(&format!("Payouts: {:?}\n", settlement.payouts));
        }
        if !self.warnings.is_empty() {
            out.push_str("\n## Warnings\n\n");
            for warning in &self.warnings {
                out.push_str(&format!("- {warning}\n"));
            }
        }
        out
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}

#[derive(Debug, Clone)]
pub struct CycleOutcome {
    pub settlement: Settlement,
    pub winning_solution: Option<Solution>,
    pub report: BountyReport,
}
