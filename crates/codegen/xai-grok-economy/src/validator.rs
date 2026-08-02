use crate::types::{AgentId, ValidationChallenge, ValidationVerdict};

const EARLY_TERMINATION_CONFIDENCE: f32 = 0.95;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ValidationError {
    #[error("self-validation forbidden: {0}")]
    SelfValidationForbidden(AgentId),
    #[error("validation already complete")]
    AlreadyComplete,
    #[error("invalid round transition: expected {expected:?}, current {current:?}")]
    InvalidRound {
        expected: ValidationRound,
        current: ValidationRound,
    },
    #[error("validation challenge does not match session participants or bounty")]
    ChallengeMismatch,
    #[error("validation session not found: {0}")]
    SessionNotFound(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationRound {
    Challenge,
    Defense,
    Adjudication,
    Done,
}

#[derive(Debug, Clone)]
pub struct ValidationSession {
    validator_id: AgentId,
    solution_agent_id: AgentId,
    bounty_id: String,
    current_round: ValidationRound,
    challenge: Option<ValidationChallenge>,
    defense: Option<String>,
    verdict: Option<ValidationVerdict>,
    early_terminated: bool,
}

impl ValidationSession {
    pub fn new(
        validator_id: AgentId,
        solution_agent_id: AgentId,
        bounty_id: String,
    ) -> Result<Self, ValidationError> {
        if validator_id == solution_agent_id {
            return Err(ValidationError::SelfValidationForbidden(validator_id));
        }
        Ok(Self {
            validator_id,
            solution_agent_id,
            bounty_id,
            current_round: ValidationRound::Challenge,
            challenge: None,
            defense: None,
            verdict: None,
            early_terminated: false,
        })
    }

    pub fn validator_id(&self) -> &AgentId {
        &self.validator_id
    }

    pub fn solution_agent_id(&self) -> &AgentId {
        &self.solution_agent_id
    }

    pub fn bounty_id(&self) -> &str {
        &self.bounty_id
    }

    pub fn current_round(&self) -> ValidationRound {
        self.current_round
    }

    pub fn challenge(&self) -> Option<&ValidationChallenge> {
        self.challenge.as_ref()
    }

    pub fn defense(&self) -> Option<&str> {
        self.defense.as_deref()
    }

    pub fn verdict(&self) -> Option<ValidationVerdict> {
        self.verdict
    }

    pub fn early_terminated(&self) -> bool {
        self.early_terminated
    }

    pub fn submit_challenge(
        &mut self,
        challenge: ValidationChallenge,
    ) -> Result<(), ValidationError> {
        if self.current_round == ValidationRound::Done {
            return Err(ValidationError::AlreadyComplete);
        }
        if self.current_round != ValidationRound::Challenge {
            return Err(ValidationError::InvalidRound {
                expected: ValidationRound::Challenge,
                current: self.current_round,
            });
        }
        if challenge.validator_id != self.validator_id
            || challenge.solution_agent_id != self.solution_agent_id
            || challenge.bounty_id != self.bounty_id
        {
            return Err(ValidationError::ChallengeMismatch);
        }
        if challenge.confidence >= EARLY_TERMINATION_CONFIDENCE
            && challenge.proposed_flaw.trim().is_empty()
        {
            self.challenge = Some(challenge);
            self.verdict = Some(ValidationVerdict::NoFlawFound);
            self.current_round = ValidationRound::Done;
            self.early_terminated = true;
            return Ok(());
        }
        self.challenge = Some(challenge);
        self.current_round = ValidationRound::Defense;
        Ok(())
    }

    pub fn submit_defense(&mut self, defense: String) -> Result<(), ValidationError> {
        if self.current_round == ValidationRound::Done {
            return Err(ValidationError::AlreadyComplete);
        }
        if self.current_round != ValidationRound::Defense {
            return Err(ValidationError::InvalidRound {
                expected: ValidationRound::Defense,
                current: self.current_round,
            });
        }
        self.defense = Some(defense);
        self.current_round = ValidationRound::Adjudication;
        Ok(())
    }

    pub fn adjudicate(&mut self, test_fails: bool) -> Result<ValidationVerdict, ValidationError> {
        if self.current_round == ValidationRound::Done {
            return Err(ValidationError::AlreadyComplete);
        }
        if self.current_round != ValidationRound::Adjudication {
            return Err(ValidationError::InvalidRound {
                expected: ValidationRound::Adjudication,
                current: self.current_round,
            });
        }
        let verdict = if test_fails {
            ValidationVerdict::FlawUpheld
        } else {
            ValidationVerdict::FlawDismissed
        };
        self.verdict = Some(verdict);
        self.current_round = ValidationRound::Done;
        Ok(verdict)
    }

    pub fn is_complete(&self) -> bool {
        self.current_round == ValidationRound::Done
    }
}

#[derive(Debug, Default)]
pub struct ValidationPool {
    sessions: Vec<ValidationSession>,
}

impl ValidationPool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn start_session(
        &mut self,
        validator_id: AgentId,
        solution_agent_id: AgentId,
        bounty_id: String,
    ) -> Result<usize, ValidationError> {
        self.sessions.push(ValidationSession::new(
            validator_id,
            solution_agent_id,
            bounty_id,
        )?);
        Ok(self.sessions.len() - 1)
    }

    pub fn session(&self, index: usize) -> Option<&ValidationSession> {
        self.sessions.get(index)
    }

    pub fn submit_challenge(
        &mut self,
        index: usize,
        challenge: ValidationChallenge,
    ) -> Result<(), ValidationError> {
        self.session_mut(index)?.submit_challenge(challenge)
    }

    pub fn submit_defense(&mut self, index: usize, defense: String) -> Result<(), ValidationError> {
        self.session_mut(index)?.submit_defense(defense)
    }

    pub fn adjudicate(
        &mut self,
        index: usize,
        test_fails: bool,
    ) -> Result<ValidationVerdict, ValidationError> {
        self.session_mut(index)?.adjudicate(test_fails)
    }

    pub fn verdicts(&self) -> Vec<(AgentId, ValidationVerdict)> {
        self.sessions
            .iter()
            .filter(|s| s.is_complete())
            .filter_map(|s| s.verdict.map(|verdict| (s.validator_id.clone(), verdict)))
            .collect()
    }

    pub fn flaws_found(&self) -> usize {
        self.sessions
            .iter()
            .filter(|s| s.verdict == Some(ValidationVerdict::FlawUpheld))
            .count()
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    fn session_mut(&mut self, index: usize) -> Result<&mut ValidationSession, ValidationError> {
        self.sessions
            .get_mut(index)
            .ok_or(ValidationError::SessionNotFound(index))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(label: &str) -> AgentId {
        AgentId::from_label(label)
    }

    fn challenge() -> ValidationChallenge {
        ValidationChallenge {
            validator_id: id("v"),
            solution_agent_id: id("s"),
            bounty_id: "b1".into(),
            proposed_flaw: "fails edge case".into(),
            test_code: Some("test".into()),
            confidence: 0.8,
        }
    }

    #[test]
    fn self_validation_is_forbidden() {
        let err = ValidationSession::new(id("same"), id("same"), "b1".into()).unwrap_err();
        assert!(matches!(err, ValidationError::SelfValidationForbidden(_)));
    }

    #[test]
    fn three_round_protocol_upholds_reproducible_flaw() {
        let mut session = ValidationSession::new(id("v"), id("s"), "b1".into()).unwrap();
        session.submit_challenge(challenge()).unwrap();
        session.submit_defense("defense".into()).unwrap();
        assert_eq!(
            session.adjudicate(true).unwrap(),
            ValidationVerdict::FlawUpheld
        );
        assert!(session.is_complete());
        assert_eq!(session.verdict(), Some(ValidationVerdict::FlawUpheld));
    }

    #[test]
    fn high_confidence_no_flaw_early_terminates() {
        let mut session = ValidationSession::new(id("v"), id("s"), "b1".into()).unwrap();
        session
            .submit_challenge(ValidationChallenge {
                validator_id: id("v"),
                solution_agent_id: id("s"),
                bounty_id: "b1".into(),
                proposed_flaw: String::new(),
                test_code: None,
                confidence: 0.99,
            })
            .unwrap();
        assert_eq!(session.verdict(), Some(ValidationVerdict::NoFlawFound));
        assert!(session.early_terminated());
    }

    #[test]
    fn pool_protocol_api_rejects_forged_adjudication_without_challenge() {
        let mut pool = ValidationPool::new();
        let session = pool.start_session(id("v"), id("s"), "b1".into()).unwrap();
        let err = pool.adjudicate(session, true).unwrap_err();
        assert!(matches!(err, ValidationError::InvalidRound { .. }));
        assert_eq!(pool.session(session).unwrap().verdict(), None);
    }

    #[test]
    fn challenge_must_match_session_identity() {
        let mut pool = ValidationPool::new();
        let session = pool.start_session(id("v"), id("s"), "b1".into()).unwrap();
        let err = pool
            .submit_challenge(
                session,
                ValidationChallenge {
                    validator_id: id("attacker"),
                    ..challenge()
                },
            )
            .unwrap_err();
        assert_eq!(err, ValidationError::ChallengeMismatch);
        assert_eq!(
            pool.session(session).unwrap().current_round(),
            ValidationRound::Challenge
        );
    }

    #[test]
    fn validation_protocol_state_is_not_publicly_mutable() {
        let source = include_str!("validator.rs");
        for forbidden in [
            concat!("pub ", "validator_id:"),
            concat!("pub ", "solution_agent_id:"),
            concat!("pub ", "bounty_id:"),
            concat!("pub ", "current_round:"),
            concat!("pub ", "challenge:"),
            concat!("pub ", "defense:"),
            concat!("pub ", "verdict:"),
            concat!("pub ", "early_terminated:"),
            concat!("pub fn ", "get_mut"),
        ] {
            assert!(
                !source.contains(forbidden),
                "validation protocol exposed forbidden mutable surface: {forbidden}"
            );
        }
    }
}
