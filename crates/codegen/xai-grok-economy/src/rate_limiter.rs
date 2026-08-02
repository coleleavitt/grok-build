use std::collections::HashMap;

use crate::types::AgentId;

#[derive(Debug, Default)]
pub struct AgentRateLimiter {
    max_active_per_agent: usize,
    active: HashMap<AgentId, usize>,
}

impl AgentRateLimiter {
    pub fn new(max_active_per_agent: usize) -> Self {
        Self {
            max_active_per_agent,
            active: HashMap::new(),
        }
    }

    pub fn try_acquire(&mut self, agent_id: &AgentId) -> bool {
        let count = self.active.entry(agent_id.clone()).or_insert(0);
        if *count >= self.max_active_per_agent {
            return false;
        }
        *count += 1;
        true
    }

    pub fn release(&mut self, agent_id: &AgentId) {
        if let Some(count) = self.active.get_mut(agent_id) {
            *count = count.saturating_sub(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn limits_active_work_per_agent() {
        let mut limiter = AgentRateLimiter::new(1);
        let agent = AgentId::from_label("a");
        assert!(limiter.try_acquire(&agent));
        assert!(!limiter.try_acquire(&agent));
        limiter.release(&agent);
        assert!(limiter.try_acquire(&agent));
    }
}
