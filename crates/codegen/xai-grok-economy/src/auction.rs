use crate::types::{AgentId, Bid};

#[derive(Debug, Default)]
pub struct AuctionEngine {
    bids: Vec<Bid>,
}

impl AuctionEngine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn submit_bid(&mut self, bid: Bid) {
        self.bids.push(bid);
    }

    pub fn bids_for(&self, bounty_id: &str) -> Vec<&Bid> {
        self.bids
            .iter()
            .filter(|bid| bid.bounty_id == bounty_id)
            .collect()
    }

    pub fn select_lowest_price(&self, bounty_id: &str, max: usize) -> Vec<AgentId> {
        let mut bids = self.bids_for(bounty_id);
        bids.sort_by_key(|bid| bid.price);
        bids.into_iter()
            .take(max)
            .map(|bid| bid.agent_id.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn selects_lowest_price_bids() {
        let mut auction = AuctionEngine::new();
        auction.submit_bid(Bid {
            agent_id: AgentId::from_label("a"),
            bounty_id: "b".into(),
            price: 10,
            approach: "x".into(),
            estimated_time: Duration::from_secs(1),
        });
        auction.submit_bid(Bid {
            agent_id: AgentId::from_label("b"),
            bounty_id: "b".into(),
            price: 5,
            approach: "x".into(),
            estimated_time: Duration::from_secs(1),
        });
        assert_eq!(auction.select_lowest_price("b", 1)[0].label(), "b");
    }
}
