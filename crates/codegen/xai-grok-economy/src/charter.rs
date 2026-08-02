use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ViolationType {
    DoubleSubmission,
    SelfValidation,
    ExceedMaxSolvers,
    BelowMinTrust,
    BudgetExceeded,
    TimeoutExceeded,
    SandboxEscape,
    TestDeletion,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sanction {
    pub trust_penalty: u8,
    pub description: String,
    pub blocks_action: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Charter {
    pub max_budget_per_bounty: u64,
    pub max_solvers: u8,
    pub max_validators: u8,
    pub min_trust_for_solver: u8,
    pub min_trust_for_validator: u8,
    pub validation_rounds: u8,
    pub self_validation_allowed: bool,
    pub max_token_spend_per_agent: u64,
    pub early_termination_confidence: f32,
    pub spawn_fee: u64,
    pub surge_multiplier: f32,
    pub payment_floor: f32,
    pub sanctions: HashMap<ViolationType, Sanction>,
}

impl Charter {
    pub fn from_yaml(yaml: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(yaml)
    }

    pub fn can_solve(&self, trust_score: u8) -> bool {
        trust_score >= self.min_trust_for_solver
    }

    pub fn can_validate(&self, trust_score: u8) -> bool {
        trust_score >= self.min_trust_for_validator
    }

    pub fn sanction(&self, violation: &ViolationType) -> Option<&Sanction> {
        self.sanctions.get(violation)
    }
}

impl Default for Charter {
    fn default() -> Self {
        let mut sanctions = HashMap::new();
        sanctions.insert(
            ViolationType::SelfValidation,
            Sanction {
                trust_penalty: 30,
                description: "Agent attempted to validate own solution".into(),
                blocks_action: true,
            },
        );
        sanctions.insert(
            ViolationType::SandboxEscape,
            Sanction {
                trust_penalty: 50,
                description: "Agent attempted to escape worktree sandbox".into(),
                blocks_action: true,
            },
        );
        sanctions.insert(
            ViolationType::TestDeletion,
            Sanction {
                trust_penalty: 25,
                description: "Solution deletes or weakens tests".into(),
                blocks_action: false,
            },
        );
        sanctions.insert(
            ViolationType::BudgetExceeded,
            Sanction {
                trust_penalty: 0,
                description: "Budget gate rejected the requested spend".into(),
                blocks_action: true,
            },
        );
        Self {
            max_budget_per_bounty: 100_000,
            max_solvers: 3,
            max_validators: 2,
            min_trust_for_solver: 30,
            min_trust_for_validator: 40,
            validation_rounds: 3,
            self_validation_allowed: false,
            max_token_spend_per_agent: 5_000,
            early_termination_confidence: 0.95,
            spawn_fee: 50,
            surge_multiplier: 1.15,
            payment_floor: 0.5,
            sanctions,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_charter_blocks_self_validation() {
        let charter = Charter::default();
        assert!(!charter.self_validation_allowed);
        assert!(
            charter
                .sanction(&ViolationType::SelfValidation)
                .unwrap()
                .blocks_action
        );
        assert!(charter.can_solve(30));
        assert!(!charter.can_validate(39));
    }

    #[test]
    fn charter_yaml_roundtrips() {
        let yaml = r#"
max_budget_per_bounty: 5000
max_solvers: 2
max_validators: 1
min_trust_for_solver: 25
min_trust_for_validator: 35
validation_rounds: 3
self_validation_allowed: false
max_token_spend_per_agent: 1000
early_termination_confidence: 0.9
spawn_fee: 10
surge_multiplier: 1.2
payment_floor: 0.6
sanctions: {}
"#;
        let charter = Charter::from_yaml(yaml).unwrap();
        assert_eq!(charter.max_solvers, 2);
        assert_eq!(charter.payment_floor, 0.6);
    }
}
