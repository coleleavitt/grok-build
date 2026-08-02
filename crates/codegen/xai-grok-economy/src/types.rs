use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub struct AgentId(String);

impl AgentId {
    pub fn from_label(label: impl Into<String>) -> Self {
        Self(label.into())
    }

    pub fn market_unique(role: &str) -> Self {
        Self(format!("{role}_{}", uuid::Uuid::new_v4().as_simple()))
    }

    pub fn market_stable(role: &str, index: usize) -> Self {
        Self(format!("{role}-{index}"))
    }

    pub fn label(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentRole {
    Solver,
    Validator,
    Auditor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarketState {
    Posting,
    Open,
    Bidding,
    Executing,
    Validating,
    Settling,
    Complete,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bounty {
    pub id: String,
    pub description: String,
    pub reward: u64,
    pub acceptance_criteria: String,
    pub deadline: Duration,
    pub max_solvers: u8,
    pub state: MarketState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bid {
    pub agent_id: AgentId,
    pub bounty_id: String,
    pub price: u64,
    pub approach: String,
    pub estimated_time: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Solution {
    pub agent_id: AgentId,
    pub bounty_id: String,
    pub patch: String,
    pub explanation: String,
    pub self_assessment: f32,
    pub tokens_consumed: u64,
    pub compiles: Option<bool>,
    pub tests_pass: Option<bool>,
    pub suspicious: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationChallenge {
    pub validator_id: AgentId,
    pub solution_agent_id: AgentId,
    pub bounty_id: String,
    pub proposed_flaw: String,
    pub test_code: Option<String>,
    pub confidence: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValidationVerdict {
    FlawUpheld,
    FlawDismissed,
    NoFlawFound,
    EarlyTermination,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settlement {
    pub bounty_id: String,
    pub winner: Option<AgentId>,
    pub payouts: Vec<(AgentId, i64)>,
    pub trust_updates: Vec<(AgentId, i8)>,
    pub total_cost: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub timestamp_ms: u64,
    pub bounty_id: String,
    pub event: AuditEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AuditEvent {
    BountyPosted {
        reward: u64,
    },
    BidReceived {
        agent_id: AgentId,
        price: u64,
    },
    SolverSelected {
        agent_id: AgentId,
    },
    SolutionSubmitted {
        agent_id: AgentId,
        compiles: bool,
    },
    ValidationStarted {
        validator_id: AgentId,
        solution_agent_id: AgentId,
    },
    ValidationVerdict {
        verdict: ValidationVerdict,
    },
    SettlementComplete {
        winner: Option<AgentId>,
        total_cost: u64,
    },
    StateTransition {
        from: MarketState,
        to: MarketState,
    },
    CharterViolation {
        agent_id: AgentId,
        violation: String,
        penalty: i8,
    },
    SurgePricing {
        old_reward: u64,
        new_reward: u64,
    },
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_agent_id_is_deterministic() {
        assert_eq!(
            AgentId::market_stable("solver", 1),
            AgentId::from_label("solver-1")
        );
        assert_ne!(
            AgentId::market_unique("solver"),
            AgentId::market_unique("solver")
        );
    }

    #[test]
    fn market_state_roundtrips() {
        let json = serde_json::to_string(&MarketState::Validating).unwrap();
        let parsed: MarketState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, MarketState::Validating);
    }
}
