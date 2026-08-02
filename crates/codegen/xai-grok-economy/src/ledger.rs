use crate::types::{AgentId, now_ms};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    pub timestamp_ms: u64,
    pub agent_id: AgentId,
    pub amount: i64,
    pub purpose: TransactionPurpose,
    pub model: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TransactionPurpose {
    SpawnFee,
    Execution,
    BountyReward,
    ValidationReward,
    Penalty,
    Refund,
}

#[derive(Debug, Clone)]
pub struct PriceOracle {
    default_input: u64,
    default_output: u64,
}

impl PriceOracle {
    pub fn new(default_input: u64, default_output: u64) -> Self {
        Self {
            default_input,
            default_output,
        }
    }

    pub fn estimate_cost(&self, _model: &str, input_tokens: u64, output_tokens: u64) -> u64 {
        input_tokens
            .saturating_mul(self.default_input)
            .saturating_add(output_tokens.saturating_mul(self.default_output))
            / 1_000_000
    }
}

impl Default for PriceOracle {
    fn default() -> Self {
        Self::new(3, 15)
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BudgetError {
    #[error("budget exhausted: remaining={remaining}, requested={requested}")]
    Exhausted { remaining: u64, requested: u64 },
    #[error("daily burn cap exceeded: cap={cap}, today={today}")]
    DailyCapExceeded { cap: u64, today: u64 },
}

#[derive(Debug, Clone)]
pub struct TokenLedger {
    total_budget: u64,
    total_spent: u64,
    daily_burn_cap: u64,
    today_spent: u64,
    spawn_fee: u64,
    balances: HashMap<AgentId, i64>,
    transactions: Vec<Transaction>,
    oracle: PriceOracle,
}

impl TokenLedger {
    pub fn new(total_budget: u64, daily_burn_cap: u64, spawn_fee: u64) -> Self {
        Self {
            total_budget,
            total_spent: 0,
            daily_burn_cap,
            today_spent: 0,
            spawn_fee,
            balances: HashMap::new(),
            transactions: Vec::new(),
            oracle: PriceOracle::default(),
        }
    }

    pub fn gate_check(
        &self,
        model: &str,
        estimated_input: u64,
        estimated_output: u64,
    ) -> Result<u64, BudgetError> {
        let cost = self
            .oracle
            .estimate_cost(model, estimated_input, estimated_output);
        self.gate_amount(cost)?;
        Ok(cost)
    }

    pub fn gate_amount(&self, requested: u64) -> Result<(), BudgetError> {
        let remaining = self.remaining();
        if requested > remaining {
            return Err(BudgetError::Exhausted {
                remaining,
                requested,
            });
        }
        if self.today_spent.saturating_add(requested) > self.daily_burn_cap {
            return Err(BudgetError::DailyCapExceeded {
                cap: self.daily_burn_cap,
                today: self.today_spent,
            });
        }
        Ok(())
    }

    pub fn debit_spawn(&mut self, agent_id: &AgentId) -> Result<(), BudgetError> {
        self.gate_amount(self.spawn_fee)?;
        self.debit(
            agent_id,
            self.spawn_fee,
            TransactionPurpose::SpawnFee,
            None,
            None,
            None,
        );
        Ok(())
    }

    pub fn record_usage(
        &mut self,
        agent_id: &AgentId,
        model: &str,
        input_tokens: u64,
        output_tokens: u64,
    ) -> Result<u64, BudgetError> {
        let cost = self
            .oracle
            .estimate_cost(model, input_tokens, output_tokens);
        self.gate_amount(cost)?;
        self.debit(
            agent_id,
            cost,
            TransactionPurpose::Execution,
            Some(model.to_owned()),
            Some(input_tokens),
            Some(output_tokens),
        );
        Ok(cost)
    }

    pub fn credit(&mut self, agent_id: &AgentId, amount: u64, purpose: TransactionPurpose) {
        let amount_i64 = amount_as_i64(amount);
        *self.balances.entry(agent_id.clone()).or_insert(0) += amount_i64;
        self.transactions.push(Transaction {
            timestamp_ms: now_ms(),
            agent_id: agent_id.clone(),
            amount: amount_i64,
            purpose,
            model: None,
            input_tokens: None,
            output_tokens: None,
        });
    }

    fn debit(
        &mut self,
        agent_id: &AgentId,
        amount: u64,
        purpose: TransactionPurpose,
        model: Option<String>,
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
    ) {
        self.total_spent = self.total_spent.saturating_add(amount);
        self.today_spent = self.today_spent.saturating_add(amount);
        let amount_i64 = amount_as_i64(amount);
        *self.balances.entry(agent_id.clone()).or_insert(0) -= amount_i64;
        self.transactions.push(Transaction {
            timestamp_ms: now_ms(),
            agent_id: agent_id.clone(),
            amount: -amount_i64,
            purpose,
            model,
            input_tokens,
            output_tokens,
        });
    }

    pub fn remaining(&self) -> u64 {
        self.total_budget.saturating_sub(self.total_spent)
    }

    pub fn total_spent(&self) -> u64 {
        self.total_spent
    }

    pub fn agent_balance(&self, agent_id: &AgentId) -> i64 {
        self.balances.get(agent_id).copied().unwrap_or(0)
    }

    pub fn transactions(&self) -> &[Transaction] {
        &self.transactions
    }
}

fn amount_as_i64(amount: u64) -> i64 {
    i64::try_from(amount).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_rejects_when_budget_exhausted() {
        let ledger = TokenLedger::new(1, 100, 0);
        let err = ledger
            .gate_check("model", 1_000_000, 1_000_000)
            .unwrap_err();
        assert!(matches!(err, BudgetError::Exhausted { .. }));
    }

    #[test]
    fn gate_amount_rejects_projected_spawn_fees() {
        let ledger = TokenLedger::new(40, 40, 50);
        let err = ledger.gate_amount(50).unwrap_err();
        assert_eq!(
            err,
            BudgetError::Exhausted {
                remaining: 40,
                requested: 50,
            }
        );
        assert!(ledger.transactions().is_empty());
    }

    #[test]
    fn usage_and_rewards_are_audited() {
        let agent = AgentId::from_label("solver-0");
        let mut ledger = TokenLedger::new(10_000, 10_000, 50);
        ledger.debit_spawn(&agent).unwrap();
        ledger.record_usage(&agent, "model", 1_000_000, 0).unwrap();
        ledger.credit(&agent, 500, TransactionPurpose::BountyReward);
        assert_eq!(ledger.transactions().len(), 3);
        assert!(ledger.agent_balance(&agent) > 0);
    }

    #[test]
    fn usage_debit_is_checked_before_mutating_ledger() {
        let agent = AgentId::from_label("solver-0");
        let mut ledger = TokenLedger::new(10, 10, 0);
        let err = ledger
            .record_usage(&agent, "model", 4_000_000, 0)
            .unwrap_err();
        assert_eq!(
            err,
            BudgetError::Exhausted {
                remaining: 10,
                requested: 12,
            }
        );
        assert_eq!(ledger.total_spent(), 0);
        assert!(ledger.transactions().is_empty());
        assert_eq!(ledger.agent_balance(&agent), 0);
    }

    #[test]
    fn spawn_fee_debit_uses_daily_cap_gate() {
        let agent = AgentId::from_label("solver-0");
        let mut ledger = TokenLedger::new(100, 40, 50);
        let err = ledger.debit_spawn(&agent).unwrap_err();
        assert_eq!(err, BudgetError::DailyCapExceeded { cap: 40, today: 0 });
        assert_eq!(ledger.total_spent(), 0);
        assert!(ledger.transactions().is_empty());
    }
}
