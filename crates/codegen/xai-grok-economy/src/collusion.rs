use crate::types::{AgentId, ValidationVerdict};
use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct AgentStats {
    pub total_validations: u32,
    pub approvals: u32,
    pub rejections: u32,
    pub dismissals: u32,
}

impl AgentStats {
    pub fn approval_rate(&self) -> f32 {
        if self.total_validations == 0 {
            0.5
        } else {
            self.approvals as f32 / self.total_validations as f32
        }
    }

    pub fn rejection_rate(&self) -> f32 {
        if self.total_validations == 0 {
            0.0
        } else {
            self.rejections as f32 / self.total_validations as f32
        }
    }
}

#[derive(Debug)]
pub struct CollusionDetector {
    stats: HashMap<AgentId, AgentStats>,
    rubber_stamp_threshold: f32,
    griefing_threshold: f32,
    min_samples: u32,
}

impl CollusionDetector {
    pub fn new() -> Self {
        Self {
            stats: HashMap::new(),
            rubber_stamp_threshold: 0.9,
            griefing_threshold: 0.8,
            min_samples: 5,
        }
    }

    pub fn record(&mut self, validator_id: &AgentId, verdict: ValidationVerdict) {
        let stats = self.stats.entry(validator_id.clone()).or_default();
        stats.total_validations += 1;
        match verdict {
            ValidationVerdict::NoFlawFound | ValidationVerdict::EarlyTermination => {
                stats.approvals += 1
            }
            ValidationVerdict::FlawUpheld => stats.rejections += 1,
            ValidationVerdict::FlawDismissed => stats.dismissals += 1,
        }
    }

    pub fn is_rubber_stamping(&self, agent_id: &AgentId) -> bool {
        self.stats
            .get(agent_id)
            .map(|s| {
                s.total_validations >= self.min_samples
                    && s.approval_rate() > self.rubber_stamp_threshold
            })
            .unwrap_or(false)
    }

    pub fn is_griefing(&self, agent_id: &AgentId) -> bool {
        self.stats
            .get(agent_id)
            .map(|s| {
                s.total_validations >= self.min_samples
                    && s.rejection_rate() > self.griefing_threshold
            })
            .unwrap_or(false)
    }

    pub fn flagged_agents(&self) -> Vec<(AgentId, String)> {
        let mut out = Vec::new();
        for id in self.stats.keys() {
            if self.is_rubber_stamping(id) {
                out.push((id.clone(), "rubber-stamping".into()));
            }
            if self.is_griefing(id) {
                out.push((id.clone(), "griefing".into()));
            }
        }
        out
    }
}

impl Default for CollusionDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn flags_rubber_stamping_and_griefing_after_min_samples() {
        let mut detector = CollusionDetector::new();
        let rubber = AgentId::from_label("rubber");
        for _ in 0..6 {
            detector.record(&rubber, ValidationVerdict::NoFlawFound);
        }
        assert!(detector.is_rubber_stamping(&rubber));

        let griefer = AgentId::from_label("griefer");
        for _ in 0..5 {
            detector.record(&griefer, ValidationVerdict::FlawUpheld);
        }
        assert!(detector.is_griefing(&griefer));
    }
}
