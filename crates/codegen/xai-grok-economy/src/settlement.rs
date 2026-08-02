use crate::charter::Charter;
use crate::ledger::{TokenLedger, TransactionPurpose};
use crate::trust::TrustRegistry;
use crate::types::{AgentId, Settlement, ValidationVerdict};

pub struct SettlementEngine;

impl SettlementEngine {
    pub fn settle(
        bounty_id: &str,
        reward: u64,
        winner: Option<&AgentId>,
        validators: &[(AgentId, ValidationVerdict)],
        charter: &Charter,
        ledger: &mut TokenLedger,
        trust: &mut TrustRegistry,
    ) -> Settlement {
        let mut payouts = Vec::new();
        let mut trust_updates = Vec::new();
        let mut total_cost = 0;
        if let Some(winner) = winner {
            let payment =
                (reward as f64 * f64::from(charter.payment_floor.max(0.5))).round() as u64;
            ledger.credit(winner, payment, TransactionPurpose::BountyReward);
            payouts.push((winner.clone(), payment as i64));
            total_cost += payment;
            if let Some(score) = trust.get_mut(winner) {
                score.record_success("Won bounty");
            }
            trust_updates.push((winner.clone(), 5));
        }
        for (validator, verdict) in validators {
            match verdict {
                ValidationVerdict::FlawUpheld => {
                    let reward = reward / 10;
                    ledger.credit(validator, reward, TransactionPurpose::ValidationReward);
                    payouts.push((validator.clone(), reward as i64));
                    total_cost += reward;
                    if let Some(score) = trust.get_mut(validator) {
                        score.record_success("Found valid flaw");
                    }
                    trust_updates.push((validator.clone(), 5));
                }
                ValidationVerdict::FlawDismissed => {
                    if let Some(score) = trust.get_mut(validator) {
                        score.record_failure("Invalid challenge dismissed");
                    }
                    trust_updates.push((validator.clone(), -15));
                }
                ValidationVerdict::NoFlawFound | ValidationVerdict::EarlyTermination => {}
            }
        }
        Settlement {
            bounty_id: bounty_id.to_owned(),
            winner: winner.cloned(),
            payouts,
            trust_updates,
            total_cost,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settlement_pays_winner_and_flaw_validators() {
        let charter = Charter::default();
        let mut ledger = TokenLedger::new(10_000, 10_000, 0);
        let mut trust = TrustRegistry::new();
        let winner = AgentId::from_label("winner");
        let validator = AgentId::from_label("validator");
        trust.register(winner.clone());
        trust.register(validator.clone());
        let settlement = SettlementEngine::settle(
            "b1",
            1_000,
            Some(&winner),
            &[(validator.clone(), ValidationVerdict::FlawUpheld)],
            &charter,
            &mut ledger,
            &mut trust,
        );
        assert_eq!(settlement.winner, Some(winner));
        assert!(
            settlement
                .payouts
                .iter()
                .any(|(id, amount)| id == &validator && *amount == 100)
        );
    }
}
