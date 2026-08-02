use crate::types::{AuditEntry, AuditEvent, Bounty, MarketState, now_ms};
use std::time::Duration;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BountyError {
    #[error("invalid state transition: {from:?} -> {to:?}")]
    InvalidTransition { from: MarketState, to: MarketState },
    #[error("bounty not found: {0}")]
    NotFound(String),
    #[error("bounty already complete")]
    AlreadyComplete,
}

fn valid_transition(from: MarketState, to: MarketState) -> bool {
    matches!(
        (from, to),
        (MarketState::Posting, MarketState::Open)
            | (MarketState::Open, MarketState::Bidding)
            | (MarketState::Bidding, MarketState::Executing)
            | (MarketState::Bidding, MarketState::Open)
            | (MarketState::Executing, MarketState::Validating)
            | (MarketState::Validating, MarketState::Settling)
            | (MarketState::Settling, MarketState::Complete)
            | (
                MarketState::Bidding | MarketState::Executing | MarketState::Validating,
                MarketState::Failed
            )
    )
}

#[derive(Debug, Default)]
pub struct BountyManager {
    bounties: Vec<Bounty>,
    audit_log: Vec<AuditEntry>,
    surge_multiplier: f32,
}

impl BountyManager {
    pub fn new() -> Self {
        Self {
            bounties: Vec::new(),
            audit_log: Vec::new(),
            surge_multiplier: 1.15,
        }
    }

    pub fn post(
        &mut self,
        description: String,
        reward: u64,
        acceptance_criteria: String,
        deadline: Duration,
        max_solvers: u8,
    ) -> String {
        let id = format!("bounty_{}", uuid::Uuid::new_v4().as_simple());
        self.audit_log.push(AuditEntry {
            timestamp_ms: now_ms(),
            bounty_id: id.clone(),
            event: AuditEvent::BountyPosted { reward },
        });
        self.bounties.push(Bounty {
            id: id.clone(),
            description,
            reward,
            acceptance_criteria,
            deadline,
            max_solvers,
            state: MarketState::Posting,
        });
        id
    }

    pub fn validate_transition(&self, bounty_id: &str, to: MarketState) -> Result<(), BountyError> {
        let bounty = self
            .bounties
            .iter()
            .find(|b| b.id == bounty_id)
            .ok_or_else(|| BountyError::NotFound(bounty_id.to_owned()))?;
        if bounty.state == MarketState::Complete {
            return Err(BountyError::AlreadyComplete);
        }
        if !valid_transition(bounty.state, to) {
            return Err(BountyError::InvalidTransition {
                from: bounty.state,
                to,
            });
        }
        Ok(())
    }

    pub fn transition(&mut self, bounty_id: &str, to: MarketState) -> Result<(), BountyError> {
        self.validate_transition(bounty_id, to)?;
        let bounty = self
            .bounties
            .iter_mut()
            .find(|b| b.id == bounty_id)
            .ok_or_else(|| BountyError::NotFound(bounty_id.to_owned()))?;
        let from = bounty.state;
        bounty.state = to;
        self.audit_log.push(AuditEntry {
            timestamp_ms: now_ms(),
            bounty_id: bounty_id.to_owned(),
            event: AuditEvent::StateTransition { from, to },
        });
        Ok(())
    }

    pub fn surge_price(&mut self, bounty_id: &str) -> Result<u64, BountyError> {
        let bounty = self
            .bounties
            .iter_mut()
            .find(|b| b.id == bounty_id)
            .ok_or_else(|| BountyError::NotFound(bounty_id.to_owned()))?;
        let old_reward = bounty.reward;
        bounty.reward = (bounty.reward as f32 * self.surge_multiplier) as u64;
        self.audit_log.push(AuditEntry {
            timestamp_ms: now_ms(),
            bounty_id: bounty_id.to_owned(),
            event: AuditEvent::SurgePricing {
                old_reward,
                new_reward: bounty.reward,
            },
        });
        Ok(bounty.reward)
    }

    pub fn get(&self, bounty_id: &str) -> Option<&Bounty> {
        self.bounties.iter().find(|b| b.id == bounty_id)
    }

    pub fn audit_log(&self) -> &[AuditEntry] {
        &self.audit_log
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_allows_forward_transitions_and_blocks_invalid() {
        let mut mgr = BountyManager::new();
        let id = mgr.post(
            "fix bug".into(),
            100,
            "tests pass".into(),
            Duration::from_secs(60),
            2,
        );
        assert_eq!(mgr.get(&id).unwrap().state, MarketState::Posting);
        assert!(matches!(
            mgr.transition(&id, MarketState::Complete),
            Err(BountyError::InvalidTransition { .. })
        ));
        for state in [
            MarketState::Open,
            MarketState::Bidding,
            MarketState::Executing,
            MarketState::Validating,
            MarketState::Settling,
            MarketState::Complete,
        ] {
            mgr.transition(&id, state).unwrap();
        }
        assert_eq!(mgr.get(&id).unwrap().state, MarketState::Complete);
    }

    #[test]
    fn surge_pricing_records_audit() {
        let mut mgr = BountyManager::new();
        let id = mgr.post("x".into(), 100, "y".into(), Duration::from_secs(1), 1);
        assert_eq!(mgr.surge_price(&id).unwrap(), 115);
        assert!(
            mgr.audit_log()
                .iter()
                .any(|e| matches!(e.event, AuditEvent::SurgePricing { .. }))
        );
    }
}
