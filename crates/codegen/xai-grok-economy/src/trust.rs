use crate::types::{AgentId, now_ms};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrustTier {
    Restricted,
    Standard,
    Trusted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustChange {
    pub timestamp_ms: u64,
    pub delta: i8,
    pub reason: String,
    pub score_after: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustScore {
    score: u8,
    history: Vec<TrustChange>,
}

impl TrustScore {
    pub fn new() -> Self {
        Self {
            score: 50,
            history: Vec::new(),
        }
    }

    pub fn score(&self) -> u8 {
        self.score
    }

    pub fn tier(&self) -> TrustTier {
        match self.score {
            0..=29 => TrustTier::Restricted,
            30..=70 => TrustTier::Standard,
            _ => TrustTier::Trusted,
        }
    }

    pub fn record_success(&mut self, reason: &str) {
        self.apply(5, reason);
    }

    pub fn record_failure(&mut self, reason: &str) {
        self.apply(-15, reason);
    }

    pub fn record_penalty(&mut self, amount: u8, reason: &str) {
        self.apply(-(amount as i8), reason);
    }

    fn apply(&mut self, delta: i8, reason: &str) {
        if delta >= 0 {
            self.score = self.score.saturating_add(delta as u8).min(100);
        } else {
            self.score = self.score.saturating_sub(delta.unsigned_abs());
        }
        self.history.push(TrustChange {
            timestamp_ms: now_ms(),
            delta,
            reason: reason.to_owned(),
            score_after: self.score,
        });
    }

    pub fn history(&self) -> &[TrustChange] {
        &self.history
    }
}

impl Default for TrustScore {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Default)]
pub struct TrustRegistry {
    scores: HashMap<AgentId, TrustScore>,
}

impl TrustRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, agent_id: AgentId) {
        self.scores.entry(agent_id).or_default();
    }

    pub fn get(&self, agent_id: &AgentId) -> Option<&TrustScore> {
        self.scores.get(agent_id)
    }
    pub fn get_mut(&mut self, agent_id: &AgentId) -> Option<&mut TrustScore> {
        self.scores.get_mut(agent_id)
    }

    pub fn meets_minimum(&self, agent_id: &AgentId, min_trust: u8) -> bool {
        self.scores
            .get(agent_id)
            .map(|s| s.score() >= min_trust)
            .unwrap_or(false)
    }

    pub fn mean_trust(&self) -> f32 {
        if self.scores.is_empty() {
            return 50.0;
        }
        self.scores.values().map(|s| s.score() as u32).sum::<u32>() as f32
            / self.scores.len() as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn trust_is_asymmetric_and_tiered() {
        let mut score = TrustScore::new();
        score.record_success("won");
        assert_eq!(score.score(), 55);
        score.record_failure("bad");
        assert_eq!(score.score(), 40);
        score.record_penalty(20, "violation");
        assert_eq!(score.tier(), TrustTier::Restricted);
        assert_eq!(score.history().len(), 3);
    }
}
