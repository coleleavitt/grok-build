//! Grok Build bounty economy.
//!
//! A Grok-native port of JFC's economy/bounty flow. The core lifecycle is:
//! post → bid/select → execute solvers → sealed validation → settlement → report.
//!
//! The crate deliberately keeps the market core independent from the shell. Grok
//! integration happens through [`reporting::AgentInvoker`] and
//! [`reporting::SwarmProvider`], which can be backed by real TaskTool/subagent
//! worktrees in production and deterministic fakes in tests.

pub mod auction;
pub mod bounty;
pub mod charter;
pub mod collusion;
pub mod cost;
pub mod ledger;
pub mod orchestrator;
pub mod rate_limiter;
pub mod reporting;
pub mod settlement;
pub mod solver;
pub mod trust;
pub mod types;
pub mod validator;

pub use auction::AuctionEngine;
pub use bounty::{BountyError, BountyManager};
pub use charter::{Charter, Sanction, ViolationType};
pub use collusion::{AgentStats, CollusionDetector};
pub use cost::{TokenEstimate, split_budget};
pub use ledger::{BudgetError, TokenLedger, Transaction, TransactionPurpose};
pub use orchestrator::{MarketOrchestrator, OrchestratorError};
pub use rate_limiter::AgentRateLimiter;
pub use reporting::{
    AgentInvoker, BountyReport, CycleOutcome, SolverPrompt, SolverReport, SwarmProvider,
    ValidationReport, ValidatorOutcome, ValidatorPrompt,
};
pub use settlement::SettlementEngine;
pub use solver::{SolverAgent, SolverPool, SolverStatus, mechanically_accepted};
pub use trust::{TrustChange, TrustRegistry, TrustScore, TrustTier};
pub use types::*;
pub use validator::{ValidationError, ValidationPool, ValidationRound, ValidationSession};
