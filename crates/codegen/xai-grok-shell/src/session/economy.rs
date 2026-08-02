//! Grok shell adapter seam for the bounty economy.
//!
//! The economy core is intentionally shell-independent. This module is the
//! shipped Grok-side boundary: shell code imports the core market orchestrator
//! here, and production wiring can provide TaskTool/subagent-backed
//! `AgentInvoker`/`SwarmProvider` implementations without changing the market
//! state machine.

pub(crate) fn default_market_orchestrator() -> xai_grok_economy::MarketOrchestrator {
    xai_grok_economy::MarketOrchestrator::new(xai_grok_economy::Charter::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_can_construct_grok_economy_orchestrator() {
        let mut orchestrator = default_market_orchestrator();
        let id = orchestrator
            .post_bounty(
                "Fix flaky test".into(),
                100,
                "targeted test passes".into(),
                Some(1),
            )
            .expect("default charter accepts small bounty");
        assert_eq!(
            orchestrator.bounties().get(&id).unwrap().state,
            xai_grok_economy::MarketState::Open
        );
    }
}
