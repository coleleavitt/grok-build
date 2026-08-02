#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenEstimate {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl TokenEstimate {
    pub fn total(self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }
}

pub fn split_budget(total: u64, participants: usize) -> u64 {
    if participants == 0 {
        total
    } else {
        (total / participants as u64).max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split_budget_never_returns_zero_for_nonzero_participants() {
        assert_eq!(split_budget(1, 10), 1);
        assert_eq!(split_budget(100, 4), 25);
    }
}
