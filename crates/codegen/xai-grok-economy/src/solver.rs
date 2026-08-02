use crate::types::{AgentId, Solution};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SolverStatus {
    Pending,
    Executing,
    Completed,
    TimedOut,
    Abandoned,
}

#[derive(Debug, Clone)]
pub struct SolverAgent {
    pub id: AgentId,
    pub bounty_id: String,
    pub worktree_path: Option<PathBuf>,
    pub status: SolverStatus,
    pub tokens_consumed: u64,
    pub solution: Option<Solution>,
}

impl SolverAgent {
    pub fn with_id(agent_id: AgentId, bounty_id: &str) -> Self {
        Self {
            id: agent_id,
            bounty_id: bounty_id.to_owned(),
            worktree_path: None,
            status: SolverStatus::Pending,
            tokens_consumed: 0,
            solution: None,
        }
    }

    pub fn start(&mut self, worktree: Option<PathBuf>) {
        self.worktree_path = worktree;
        self.status = SolverStatus::Executing;
    }

    pub fn submit(&mut self, mut solution: Solution) {
        if solution.worktree_path.is_none() {
            solution.worktree_path = self.worktree_path.clone();
        }
        if self.worktree_path.is_none() {
            self.worktree_path = solution.worktree_path.clone();
        }
        self.tokens_consumed = self
            .tokens_consumed
            .saturating_add(solution.tokens_consumed);
        self.solution = Some(solution);
        self.status = SolverStatus::Completed;
    }

    pub fn abandon(&mut self) {
        self.status = SolverStatus::Abandoned;
    }
}

#[derive(Debug, Default)]
pub struct SolverPool {
    solvers: Vec<SolverAgent>,
}

impl SolverPool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn spawn_with_id(&mut self, agent_id: AgentId, bounty_id: &str) -> &SolverAgent {
        self.solvers.push(SolverAgent::with_id(agent_id, bounty_id));
        self.solvers.last().unwrap()
    }

    pub fn get_mut_for_bounty(
        &mut self,
        agent_id: &AgentId,
        bounty_id: &str,
    ) -> Option<&mut SolverAgent> {
        self.solvers
            .iter_mut()
            .find(|s| &s.id == agent_id && s.bounty_id == bounty_id)
    }

    pub fn get_for_bounty(&self, agent_id: &AgentId, bounty_id: &str) -> Option<&SolverAgent> {
        self.solvers
            .iter()
            .find(|s| &s.id == agent_id && s.bounty_id == bounty_id)
    }

    pub fn completed_solutions_for_bounty(&self, bounty_id: &str) -> Vec<&Solution> {
        self.solvers
            .iter()
            .filter(|solver| solver.bounty_id == bounty_id)
            .filter(|solver| solver.status == SolverStatus::Completed)
            .filter_map(|solver| solver.solution.as_ref())
            .collect()
    }

    pub fn rank_solutions_for_bounty(&self, bounty_id: &str) -> Vec<&Solution> {
        let mut solutions = self
            .completed_solutions_for_bounty(bounty_id)
            .into_iter()
            .filter(|solution| mechanically_accepted(solution))
            .collect::<Vec<_>>();
        solutions.sort_by(|a, b| {
            solution_score(b)
                .partial_cmp(&solution_score(a))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        solutions
    }

    pub fn all(&self) -> &[SolverAgent] {
        &self.solvers
    }
}

pub fn mechanically_accepted(solution: &Solution) -> bool {
    !solution.suspicious
        && solution.compiles != Some(false)
        && solution.tests_pass != Some(false)
        && (solution.compiles == Some(true) || solution.tests_pass == Some(true))
}

fn solution_score(solution: &Solution) -> f32 {
    let mut score = 0.0;
    if solution.compiles == Some(true) {
        score += 100.0;
    }
    if solution.tests_pass == Some(true) {
        score += 50.0;
    }
    if !solution.suspicious {
        score += 20.0;
    }
    score += 10.0 / (solution.tokens_consumed as f32 + 1.0).ln_1p();
    score
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solution(
        label: &str,
        compiles: Option<bool>,
        tests: Option<bool>,
        suspicious: bool,
    ) -> Solution {
        Solution {
            agent_id: AgentId::from_label(label),
            bounty_id: "b1".into(),
            patch: "diff".into(),
            explanation: "done".into(),
            self_assessment: 0.7,
            tokens_consumed: 100,
            compiles,
            tests_pass: tests,
            suspicious,
            worktree_path: None,
        }
    }

    #[test]
    fn stable_agent_ids_are_scoped_by_bounty() {
        let mut pool = SolverPool::new();
        let id = AgentId::market_stable("solver", 0);
        pool.spawn_with_id(id.clone(), "b1");
        pool.spawn_with_id(id.clone(), "b2");
        pool.get_mut_for_bounty(&id, "b2").unwrap().start(None);
        assert_eq!(
            pool.get_for_bounty(&id, "b1").unwrap().status,
            SolverStatus::Pending
        );
        assert_eq!(
            pool.get_for_bounty(&id, "b2").unwrap().status,
            SolverStatus::Executing
        );
    }

    #[test]
    fn ranks_only_mechanically_accepted_clean_solutions() {
        let mut pool = SolverPool::new();
        for (label, compiles, tests, suspicious) in [
            ("good", Some(true), Some(true), false),
            ("bad", Some(false), Some(false), false),
            ("sus", Some(true), Some(true), true),
        ] {
            let id = AgentId::from_label(label);
            pool.spawn_with_id(id.clone(), "b1");
            pool.get_mut_for_bounty(&id, "b1").unwrap().start(None);
            pool.get_mut_for_bounty(&id, "b1")
                .unwrap()
                .submit(solution(label, compiles, tests, suspicious));
        }
        let ranked = pool.rank_solutions_for_bounty("b1");
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].agent_id.label(), "good");
    }
}
