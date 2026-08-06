//! The Graph of Operations (GoO): the static execution plan, and the metrics
//! the paper defines over it.

use crate::operations::OpKind;
use crate::thought::Thought;
use std::collections::{HashSet, VecDeque};

/// Index into a [`GraphOfOperations`]'s arena.
pub type OpId = usize;

/// One operation plus its place in the graph and the thoughts it produced.
///
/// `thoughts` is the operation's slice of the Graph Reasoning State: the GoO
/// is fixed before the run, the thoughts accumulate during it.
pub struct OperationNode {
    pub id: OpId,
    pub kind: OpKind,
    pub predecessors: Vec<OpId>,
    pub successors: Vec<OpId>,
    pub executed: bool,
    pub thoughts: Vec<Thought>,
}

/// The task's graph decomposition: which transformations run, in what order,
/// with what dependencies.
///
/// Arena-backed rather than a web of reference-counted nodes. The reference
/// implementation stores predecessors AND successors as object references,
/// which is a reference cycle; indices give the same bidirectional navigation,
/// keep the whole graph `Send`, and make it trivially serializable for the
/// durable-run case the paper never has to handle.
#[derive(Default)]
pub struct GraphOfOperations {
    nodes: Vec<OperationNode>,
    roots: Vec<OpId>,
    leaves: Vec<OpId>,
}

impl GraphOfOperations {
    pub fn new() -> Self {
        Self::default()
    }

    /// Chain `kind` after every current leaf, making it the sole leaf.
    ///
    /// The common case: a linear pipeline stage that consumes everything the
    /// previous stage produced.
    pub fn append(&mut self, kind: OpKind) -> OpId {
        let id = self.push(kind);
        if self.roots.is_empty() {
            self.roots.push(id);
        } else {
            for leaf in std::mem::take(&mut self.leaves) {
                self.link(leaf, id);
            }
        }
        self.leaves = vec![id];
        id
    }

    /// Add `kind` with explicit predecessors, for fan-in/fan-out shapes that a
    /// linear append cannot express (a merge tree, a side branch).
    ///
    /// An empty `predecessors` makes it another root, so several independent
    /// chains can start in one graph. Any predecessor that was a leaf stops
    /// being one; the new operation always becomes a leaf.
    pub fn add(&mut self, kind: OpKind, predecessors: &[OpId]) -> OpId {
        let id = self.push(kind);
        if predecessors.is_empty() {
            self.roots.push(id);
        }
        for &predecessor in predecessors {
            self.link(predecessor, id);
            self.leaves.retain(|&leaf| leaf != predecessor);
        }
        self.leaves.push(id);
        id
    }

    fn push(&mut self, kind: OpKind) -> OpId {
        let id = self.nodes.len();
        self.nodes.push(OperationNode {
            id,
            kind,
            predecessors: Vec::new(),
            successors: Vec::new(),
            executed: false,
            thoughts: Vec::new(),
        });
        id
    }

    fn link(&mut self, from: OpId, to: OpId) {
        self.nodes[from].successors.push(to);
        self.nodes[to].predecessors.push(from);
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
    pub fn roots(&self) -> &[OpId] {
        &self.roots
    }
    pub fn leaves(&self) -> &[OpId] {
        &self.leaves
    }
    pub fn node(&self, id: OpId) -> &OperationNode {
        &self.nodes[id]
    }
    pub fn node_mut(&mut self, id: OpId) -> &mut OperationNode {
        &mut self.nodes[id]
    }
    pub fn ids(&self) -> impl Iterator<Item = OpId> + use<> {
        0..self.nodes.len()
    }

    /// Every thought produced by `id`'s immediate predecessors, in predecessor
    /// order — the operation's input.
    pub fn previous_thoughts(&self, id: OpId) -> Vec<Thought> {
        self.nodes[id]
            .predecessors
            .iter()
            .flat_map(|&predecessor| self.nodes[predecessor].thoughts.iter().cloned())
            .collect()
    }

    /// Can `id` run — i.e. has every predecessor already run?
    pub fn can_execute(&self, id: OpId) -> bool {
        self.nodes[id]
            .predecessors
            .iter()
            .all(|&predecessor| self.nodes[predecessor].executed)
    }

    /// Operations that can reach `id` by following edges forward.
    fn ancestors(&self, id: OpId) -> HashSet<OpId> {
        let mut seen = HashSet::new();
        let mut queue: VecDeque<OpId> = self.nodes[id].predecessors.iter().copied().collect();
        while let Some(current) = queue.pop_front() {
            if !seen.insert(current) {
                continue;
            }
            queue.extend(self.nodes[current].predecessors.iter().copied());
        }
        seen
    }

    /// The paper's **volume** of the thoughts at `id`: how many earlier
    /// thoughts could have contributed to them (§6).
    ///
    /// Counts thoughts actually produced by ancestor operations, so it is only
    /// meaningful after a run. The paper's headline claim is that aggregation
    /// buys volume `N` at latency `log_k N`, where a tree buys volume
    /// `O(log_k N)` at the same latency — a fan-in reaches back into every
    /// branch, a tree path reaches back only along itself.
    ///
    /// Worth reading as a property of the topology you authored, not as
    /// evidence about output quality: the paper never ties volume to accuracy
    /// empirically, and adding edges raises it for free.
    pub fn thought_volume(&self, id: OpId) -> usize {
        self.ancestors(id)
            .iter()
            .map(|&ancestor| self.nodes[ancestor].thoughts.len())
            .sum()
    }

    /// Ancestor OPERATION count — the same shape measured on the static plan,
    /// so it can be compared across decompositions before spending a token.
    pub fn operation_volume(&self, id: OpId) -> usize {
        self.ancestors(id).len()
    }

    /// The paper's **latency**: the longest root-to-leaf hop count (§6).
    ///
    /// Zero for a single-operation graph, matching "hops to reach the final
    /// thought".
    pub fn latency(&self) -> usize {
        let mut depth = vec![0usize; self.nodes.len()];
        // Indices are assigned in construction order and every edge runs from a
        // lower id to a higher one, so a forward sweep is already topological.
        for id in 0..self.nodes.len() {
            for &successor in &self.nodes[id].successors {
                depth[successor] = depth[successor].max(depth[id] + 1);
            }
        }
        depth.into_iter().max().unwrap_or(0)
    }

    /// Every thought in the graph, in operation order — the full Graph
    /// Reasoning State.
    pub fn all_thoughts(&self) -> impl Iterator<Item = &Thought> {
        self.nodes.iter().flat_map(|node| node.thoughts.iter())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operations::OpKind;

    fn generate() -> OpKind {
        OpKind::Generate {
            branches_prompt: 1,
            branches_response: 1,
        }
    }

    #[test]
    fn append_chains_every_leaf_into_the_new_sole_leaf() {
        let mut graph = GraphOfOperations::new();
        let a = graph.append(generate());
        let b = graph.append(generate());
        assert_eq!(graph.roots(), &[a]);
        assert_eq!(graph.leaves(), &[b]);
        assert_eq!(graph.node(b).predecessors, vec![a]);
        assert_eq!(graph.node(a).successors, vec![b]);
    }

    /// The fan-in shape the paper's whole thesis rests on: two independent
    /// branches merged by one aggregating operation.
    #[test]
    fn add_builds_a_merge_and_retires_the_merged_leaves() {
        let mut graph = GraphOfOperations::new();
        let split = graph.append(generate());
        let left = graph.add(generate(), &[split]);
        let right = graph.add(generate(), &[split]);
        assert_eq!(graph.leaves(), &[left, right]);

        let merge = graph.add(OpKind::Aggregate { num_responses: 1 }, &[left, right]);
        assert_eq!(
            graph.leaves(),
            &[merge],
            "merged branches stop being leaves",
        );
        assert_eq!(graph.node(merge).predecessors, vec![left, right]);
    }

    #[test]
    fn add_without_predecessors_starts_a_second_root() {
        let mut graph = GraphOfOperations::new();
        let first = graph.append(generate());
        let second = graph.add(generate(), &[]);
        assert_eq!(graph.roots(), &[first, second]);
    }

    /// Latency counts hops; volume counts reachable ancestors. This is the
    /// asymmetry the paper's Table 2 turns on — the merge sees BOTH branches
    /// while either branch alone sees only its own chain.
    #[test]
    fn volume_at_a_merge_spans_every_branch_while_latency_stays_shallow() {
        let mut graph = GraphOfOperations::new();
        let split = graph.append(generate());
        let left = graph.add(generate(), &[split]);
        let right = graph.add(generate(), &[split]);
        let merge = graph.add(OpKind::Aggregate { num_responses: 1 }, &[left, right]);

        assert_eq!(graph.operation_volume(merge), 3, "split + both branches");
        assert_eq!(graph.operation_volume(left), 1, "only the split");
        assert_eq!(graph.latency(), 2, "split -> branch -> merge");
    }

    #[test]
    fn can_execute_gates_on_every_predecessor() {
        let mut graph = GraphOfOperations::new();
        let left = graph.append(generate());
        let right = graph.add(generate(), &[]);
        let merge = graph.add(OpKind::Aggregate { num_responses: 1 }, &[left, right]);

        assert!(graph.can_execute(left) && graph.can_execute(right));
        assert!(!graph.can_execute(merge));
        graph.node_mut(left).executed = true;
        assert!(!graph.can_execute(merge), "one predecessor is not enough");
        graph.node_mut(right).executed = true;
        assert!(graph.can_execute(merge));
    }
}
