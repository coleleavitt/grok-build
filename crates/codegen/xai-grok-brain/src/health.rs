//! Graph health: is the Brain's relation graph still useful, or has it
//! degenerated?
//!
//! [`BrainStore::graph`](crate::BrainStore::graph) already computes a degree
//! per node, but only to render. Nothing reads it as a signal, and the graph
//! only ever accretes — `add_relation` inserts, `remove_relation` is explicit,
//! and the self-improvement run never touches relations at all. A structure
//! that only grows degenerates in two opposite directions, and both are
//! currently silent:
//!
//! - **Hub dominance / degree explosion.** Everything relates to everything, or
//!   a few pages absorb most edges. `related_page_ids` returns noise and
//!   `brain_get`'s "related: N" stops meaning anything.
//! - **Fragmentation.** Degree approaches zero everywhere and the graph is a
//!   list with extra tables — pages exist but nothing integrates them.
//!
//! # Read the distribution, not the verdict
//!
//! [`GraphHealth::regime`] is a convenience, and the thresholds behind it are a
//! **judgement call, not a derivation** — there is no principled value at which
//! a memory graph becomes unhealthy, and the right one depends on how many
//! pages a user keeps and how liberally the extraction pass links them. Two
//! consequences worth stating plainly:
//!
//! 1. The **trend** matters more than any single reading. A hub share climbing
//!    run over run is evidence; one sample above a hand-picked constant is not.
//! 2. Summary scalars can hide the shape, so [`GraphHealth::degrees`] exposes
//!    the sorted degree sequence. A mean degree of 2 is a very different graph
//!    when every node has 2 edges than when one node has 200 and the rest have
//!    none, and no single number distinguishes those.

use crate::types::MemoryGraph;

/// Below this node count a top-k endpoint share cannot distinguish a star from
/// a path, so no hub verdict is issued. See [`GraphHealth::regime`].
const MIN_NODES_FOR_HUB_VERDICT: usize = 5;

/// Coarse classification of a graph's shape. See the module docs: this is a
/// prompt to look at [`GraphHealth::degrees`], not a measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphRegime {
    /// Most pages have no relations. Pages exist but nothing integrates them.
    Fragmented,
    /// A small number of pages hold most of the edges, so `related` is
    /// dominated by whatever those hubs touch.
    HubDominated,
    /// Neither degenerate direction is pronounced.
    Healthy,
}

/// A bounded health summary over the Brain's relation graph.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphHealth {
    pub nodes: usize,
    pub edges: usize,
    /// Mean undirected degree. Each edge contributes 2 endpoints, so this is
    /// `2 * edges / nodes` and can exceed 1 even in a sparse graph.
    pub mean_degree: f64,
    /// Fraction of nodes with no relations at all, in `[0, 1]`.
    pub isolated_fraction: f64,
    /// Fraction of all edge endpoints held by the top-`hub_k` nodes, in
    /// `[0, 1]`. Endpoints rather than edges, because an edge between two hubs
    /// would otherwise be counted once while contributing to both.
    ///
    /// Raw share, reported for inspection but NOT thresholded — see
    /// [`Self::hub_excess`] for why.
    pub hub_share: f64,
    /// Concentration ABOVE what an even distribution would produce, in
    /// `[0, 1]`.
    ///
    /// The raw [`Self::hub_share`] cannot be thresholded, because any top-`k`
    /// of `n` nodes holds at least `k/n` of the endpoints by arithmetic alone.
    /// A perfectly uniform graph therefore scores exactly `k/n`, so a fixed
    /// 0.5 cutoff on the raw share labels EVERY uniform graph with
    /// `n <= 2*hub_k` as hub-dominated — a complete graph on 3, 4, 5 or 6
    /// nodes, or a 5-node ring, all of which are the healthiest shapes
    /// possible. A fresh Brain is exactly that size, so the first readings it
    /// ever produced would have been false.
    ///
    /// Rescaling against that floor makes a uniform graph score 0 at every size,
    /// and keeps a star at or above 0.375 from `n = 5` upward — the smallest
    /// size at which a hub verdict is issued at all (its hub sits in every
    /// edge, so it holds about half the endpoints).
    pub hub_excess: f64,
    /// How many top nodes `hub_share` covers.
    pub hub_k: usize,
    /// Every node's degree, descending. The shape the scalars summarise.
    pub degrees: Vec<usize>,
    /// The thresholds this summary was measured with, so [`Self::regime`]
    /// cannot be evaluated against a `hub_k` the numbers were not computed for.
    thresholds: HealthThresholds,
}

/// Thresholds for [`GraphHealth::regime`]. Defaults are judgement calls;
/// override them rather than treating them as discovered constants.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HealthThresholds {
    /// At or above this isolated fraction, the graph reads as fragmented.
    pub fragmented_at_isolated: f64,
    /// At or above this [`GraphHealth::hub_excess`], the graph reads as
    /// hub-dominated. Compared against the EXCESS, never the raw share.
    ///
    /// Hub verdicts begin at [`MIN_NODES_FOR_HUB_VERDICT`] nodes, so this value
    /// only has to separate shapes at or above that size. 0.25 sits above the 0
    /// a uniform graph scores at ANY size and below the 0.375 of the smallest
    /// judgeable star (5 nodes), with the tightest healthy shape found — a
    /// 6-node path at 0.2 — below it. Both sides are pinned by
    /// `the_threshold_boundary_is_pinned_from_both_sides`.
    ///
    /// An earlier 0.35 sat inside the star ramp and let stars read healthy at
    /// sizes that ARE judgeable, which is the false negative this replaced.
    /// Still a judgement call, just one that is no longer a function of graph
    /// size.
    pub hub_dominated_at_excess: f64,
    /// How many top nodes count as "the hubs".
    pub hub_k: usize,
}

impl Default for HealthThresholds {
    fn default() -> Self {
        Self {
            fragmented_at_isolated: 0.5,
            hub_dominated_at_excess: 0.25,
            hub_k: 3,
        }
    }
}

impl GraphHealth {
    /// Summarise `graph`. Returns `None` for an empty graph: a store with no
    /// pages is not unhealthy, it is unused, and reporting a regime for it
    /// would put a verdict on nothing.
    pub fn measure(graph: &MemoryGraph, thresholds: HealthThresholds) -> Option<Self> {
        let nodes = graph.nodes.len();
        if nodes == 0 {
            return None;
        }
        let edges = graph.edges.len();

        let mut degrees: Vec<usize> = graph.nodes.iter().map(|node| node.degree).collect();
        degrees.sort_unstable_by(|a, b| b.cmp(a));

        let endpoints: usize = degrees.iter().sum();
        let isolated = degrees.iter().filter(|degree| **degree == 0).count();
        // Clamped to at most half the graph. Letting `hub_k` reach `nodes`
        // drives the floor to 1.0, which forces `hub_excess` to 0 and silently
        // DISABLES hub detection — widening the knob would turn the metric off
        // instead of making it more sensitive. Half also keeps the floor at or
        // below 0.5 so a star's excess stops depending on graph size.
        let hub_k = thresholds.hub_k.max(1).min((nodes / 2).max(1));
        let hub_endpoints: usize = degrees.iter().take(hub_k).sum();

        // An edgeless graph has no concentration to report. Guarding here keeps
        // 0/0 out of the ratio, which would otherwise be NaN and compare false
        // against every threshold — silently "healthy".
        let hub_share = if endpoints == 0 {
            0.0
        } else {
            hub_endpoints as f64 / endpoints as f64
        };
        // The arithmetic floor any top-k holds by construction.
        let floor = hub_k as f64 / nodes as f64;
        let hub_excess = if endpoints == 0 || floor >= 1.0 {
            0.0
        } else {
            ((hub_share - floor) / (1.0 - floor)).clamp(0.0, 1.0)
        };

        Some(Self {
            nodes,
            edges,
            mean_degree: endpoints as f64 / nodes as f64,
            isolated_fraction: isolated as f64 / nodes as f64,
            hub_share,
            hub_excess,
            hub_k,
            degrees,
            thresholds,
        })
    }

    /// Classify against the thresholds this summary was measured with.
    ///
    /// Takes no argument on purpose: passing a different `hub_k` here than
    /// `measure` used would compare a top-10 share against a cutoff calibrated
    /// for top-3, silently.
    ///
    /// Fragmentation is checked first: a graph whose pages are mostly isolated
    /// is already failing to integrate anything, and the handful of edges that
    /// remain will trivially concentrate in a few nodes. Reporting that as
    /// hub-dominated would name the symptom instead of the cause.
    pub fn regime(&self) -> GraphRegime {
        if self.nodes < MIN_NODES_FOR_HUB_VERDICT
            && self.isolated_fraction < self.thresholds.fragmented_at_isolated
        {
            // Below this size a top-k endpoint share cannot separate shapes: a
            // 4-node path (degrees [2,2,1,1]) and a 4-node star ([3,1,1,1])
            // both score exactly 0.3333, so any hub verdict here would flag one
            // of them wrongly. Refusing is the same principle as returning
            // `None` for an empty graph — no verdict beats a meaningless one.
            // Fragmentation is still decidable, so it is checked first.
            return GraphRegime::Healthy;
        }
        if self.isolated_fraction >= self.thresholds.fragmented_at_isolated {
            GraphRegime::Fragmented
        } else if self.hub_excess >= self.thresholds.hub_dominated_at_excess {
            GraphRegime::HubDominated
        } else {
            GraphRegime::Healthy
        }
    }

    /// The thresholds this summary was measured with.
    pub fn thresholds(&self) -> HealthThresholds {
        self.thresholds
    }

    /// The highest degree in the graph, or 0 when there are no relations.
    pub fn max_degree(&self) -> usize {
        self.degrees.first().copied().unwrap_or(0)
    }
}

impl crate::BrainStore {
    /// Measure the relation graph's health. `None` when the store has no pages.
    pub fn graph_health(&self, thresholds: HealthThresholds) -> crate::Result<Option<GraphHealth>> {
        Ok(GraphHealth::measure(&self.graph()?, thresholds))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BrainStore, MemoryCategory, NewPage};

    fn store() -> BrainStore {
        BrainStore::open_in_memory().expect("open in-memory brain")
    }

    fn page(store: &BrainStore, title: &str) -> i64 {
        store
            .create_page(NewPage {
                title: Some(title.to_owned()),
                memory_text: format!("body of {title}"),
                category: MemoryCategory::Notes,
                source: None,
            })
            .expect("create page")
            .id
    }

    /// A store with no pages is unused, not unhealthy. Putting a verdict on
    /// nothing would make an empty Brain look like a problem to fix.
    #[test]
    fn an_empty_graph_yields_no_verdict() {
        let store = store();
        assert!(
            store
                .graph_health(HealthThresholds::default())
                .unwrap()
                .is_none()
        );
    }

    /// Pages with no relations at all: the graph is a list with extra tables.
    #[test]
    fn a_fully_disconnected_set_reads_as_fragmented() {
        let store = store();
        for i in 0..6 {
            page(&store, &format!("page {i}"));
        }
        let health = store
            .graph_health(HealthThresholds::default())
            .unwrap()
            .expect("non-empty graph");

        assert_eq!(health.nodes, 6);
        assert_eq!(health.edges, 0);
        assert_eq!(health.mean_degree, 0.0);
        assert_eq!(health.isolated_fraction, 1.0);
        assert_eq!(
            health.hub_share, 0.0,
            "an edgeless graph has no concentration; 0/0 must not surface as NaN",
        );
        assert_eq!(health.regime(), GraphRegime::Fragmented);
    }

    /// A star: one page absorbs every relation. `related` for the hub returns
    /// everything, which is the same as returning nothing useful.
    #[test]
    fn a_star_topology_reads_as_hub_dominated() {
        let store = store();
        let hub = page(&store, "hub");
        for i in 0..5 {
            let leaf = page(&store, &format!("leaf {i}"));
            store.add_relation(hub, leaf).unwrap();
        }
        let health = store
            .graph_health(HealthThresholds::default())
            .unwrap()
            .expect("non-empty graph");

        assert_eq!(health.nodes, 6);
        assert_eq!(health.edges, 5);
        assert_eq!(health.max_degree(), 5, "the hub touches every edge");
        assert_eq!(
            health.isolated_fraction, 0.0,
            "every page is connected, so this is not fragmentation",
        );
        // Pins the VALUE, not the verdict. Asserting `hub_excess >= threshold`
        // would only re-derive the classifier's own decision — with no
        // fragmentation and n >= the verdict floor, `regime()` reduces to
        // exactly that comparison, so it restates the assertion below it and
        // survives a mutation of the excess formula. 0.4 = (0.7 - 0.5) / 0.5
        // for this fixture, and a change to the formula moves it.
        //
        // Epsilon rather than `assert_eq!` because neither 0.7 nor 0.4 is
        // binary-exact, unlike the ring's exact 0.0.
        assert!(
            (health.hub_excess - 0.4).abs() < 1e-9,
            "expected hub excess ~0.4 for a 6-node star, got {}",
            health.hub_excess,
        );
        assert_eq!(health.regime(), GraphRegime::HubDominated);
    }

    /// An evenly-linked ring: every page connected, no page dominant.
    #[test]
    fn an_evenly_connected_graph_reads_healthy() {
        let store = store();
        let ids: Vec<i64> = (0..8).map(|i| page(&store, &format!("page {i}"))).collect();
        for window in ids.windows(2) {
            store.add_relation(window[0], window[1]).unwrap();
        }
        store.add_relation(ids[ids.len() - 1], ids[0]).unwrap();

        let health = store
            .graph_health(HealthThresholds::default())
            .unwrap()
            .expect("non-empty graph");

        assert_eq!(health.nodes, 8);
        assert_eq!(health.edges, 8);
        assert_eq!(health.mean_degree, 2.0, "a ring gives every node degree 2");
        assert_eq!(health.isolated_fraction, 0.0);
        // A ring is uniform, so its concentration above the k/n floor is zero at
        // every size — the property the rescaling exists for. The raw share
        // here is 0.375 only because hub_k clamps to 3 of 8, which is the
        // size-dependence this metric removed.
        assert_eq!(
            health.hub_excess, 0.0,
            "a uniform graph has no excess concentration (raw share {})",
            health.hub_share,
        );
        assert_eq!(health.regime(), GraphRegime::Healthy);
    }

    /// Fragmentation is reported ahead of hub dominance. A mostly-isolated
    /// graph will always concentrate its few remaining edges, so reporting the
    /// concentration would name the symptom instead of the cause.
    #[test]
    fn fragmentation_is_diagnosed_ahead_of_the_concentration_it_causes() {
        let store = store();
        let a = page(&store, "linked a");
        let b = page(&store, "linked b");
        store.add_relation(a, b).unwrap();
        for i in 0..8 {
            page(&store, &format!("orphan {i}"));
        }

        let health = store
            .graph_health(HealthThresholds::default())
            .unwrap()
            .expect("non-empty graph");
        assert!(health.isolated_fraction >= 0.5);
        assert_eq!(
            health.hub_share, 1.0,
            "the only two endpoints are both in the top-k",
        );
        assert_eq!(
            health.regime(),
            GraphRegime::Fragmented,
            "the cause, not the symptom",
        );
    }

    /// The scalars hide the shape, which is why the distribution is exposed.
    /// These two graphs share a mean degree and are nothing alike.
    #[test]
    fn the_degree_sequence_distinguishes_graphs_the_mean_cannot() {
        let ring = store();
        let ring_ids: Vec<i64> = (0..4).map(|i| page(&ring, &format!("r{i}"))).collect();
        for window in ring_ids.windows(2) {
            ring.add_relation(window[0], window[1]).unwrap();
        }
        ring.add_relation(ring_ids[3], ring_ids[0]).unwrap();

        let star = store();
        let hub = page(&star, "hub");
        let spokes: Vec<i64> = (0..3).map(|i| page(&star, &format!("s{i}"))).collect();
        for spoke in &spokes {
            star.add_relation(hub, *spoke).unwrap();
        }
        // Close a triangle among spokes so both graphs have 4 nodes / 4 edges.
        star.add_relation(spokes[0], spokes[1]).unwrap();

        let ring_health = ring
            .graph_health(HealthThresholds::default())
            .unwrap()
            .unwrap();
        let star_health = star
            .graph_health(HealthThresholds::default())
            .unwrap()
            .unwrap();

        assert_eq!(ring_health.mean_degree, star_health.mean_degree);
        assert_ne!(
            ring_health.degrees, star_health.degrees,
            "identical means, different shapes — this is why degrees is public",
        );
        assert_eq!(ring_health.degrees, vec![2, 2, 2, 2]);
        assert_eq!(star_health.degrees, vec![3, 2, 2, 1]);
    }

    /// Generation-1 red-team finding: every graph with `n <= 2*hub_k` and at
    /// least one edge was forced to HubDominated regardless of topology,
    /// because any top-k holds at least `k/n` of the endpoints by arithmetic.
    /// Complete graphs are the healthiest shape there is; a fresh Brain is
    /// exactly this size, so the first readings would have been false.
    #[test]
    fn small_uniform_graphs_are_not_mistaken_for_hubs() {
        for n in 3..=7usize {
            let store = store();
            let ids: Vec<i64> = (0..n).map(|i| page(&store, &format!("p{i}"))).collect();
            // Complete graph: every node has identical degree n-1.
            for i in 0..n {
                for j in (i + 1)..n {
                    store.add_relation(ids[i], ids[j]).unwrap();
                }
            }
            let health = store
                .graph_health(HealthThresholds::default())
                .unwrap()
                .expect("non-empty graph");

            assert_eq!(
                health.hub_excess, 0.0,
                "K{n}: a uniform graph has zero concentration above the k/n floor                  (raw share was {})",
                health.hub_share,
            );
            assert_eq!(
                health.regime(),
                GraphRegime::Healthy,
                "K{n} is the healthiest possible shape; raw share {} would have                  tripped a share-based threshold",
                health.hub_share,
            );
        }
    }

    /// The same defect on rings, which is how the red-team found it: n=5 and
    /// n=6 read HubDominated while n=7 and n=8 read Healthy — a verdict that
    /// changed with graph SIZE rather than graph SHAPE.
    #[test]
    fn ring_verdicts_no_longer_depend_on_graph_size() {
        for n in 5..=8usize {
            let store = store();
            let ids: Vec<i64> = (0..n).map(|i| page(&store, &format!("r{i}"))).collect();
            for window in ids.windows(2) {
                store.add_relation(window[0], window[1]).unwrap();
            }
            store.add_relation(ids[n - 1], ids[0]).unwrap();

            let health = store
                .graph_health(HealthThresholds::default())
                .unwrap()
                .unwrap();
            assert_eq!(
                health.regime(),
                GraphRegime::Healthy,
                "ring of {n} must read healthy at every size (excess {})",
                health.hub_excess,
            );
        }
    }

    /// The star must still be caught at every size, or the fix traded a false
    /// positive for a false negative.
    #[test]
    fn stars_are_still_caught_at_every_size() {
        for leaves in [5usize, 10, 20] {
            let store = store();
            let hub = page(&store, "hub");
            for i in 0..leaves {
                let leaf = page(&store, &format!("leaf {i}"));
                store.add_relation(hub, leaf).unwrap();
            }
            let health = store
                .graph_health(HealthThresholds::default())
                .unwrap()
                .unwrap();
            assert_eq!(
                health.regime(),
                GraphRegime::HubDominated,
                "star with {leaves} leaves must stay hub-dominated (excess {})",
                health.hub_excess,
            );
        }
    }

    /// `GraphRegime` crosses the `BrainStatus` wire, so its spelling is a
    /// contract and must be pinned in the crate that owns it — the same
    /// discipline this change set applies to the event enum on the other side
    /// of the fence. Without this a rename alters the wire with the suite green.
    #[test]
    fn graph_regime_wire_spelling_is_pinned() {
        for (regime, wire) in [
            (GraphRegime::Fragmented, "fragmented"),
            (GraphRegime::HubDominated, "hub_dominated"),
            (GraphRegime::Healthy, "healthy"),
        ] {
            assert_eq!(serde_json::to_value(regime).unwrap(), wire);
            assert_eq!(
                serde_json::from_value::<GraphRegime>(serde_json::json!(wire)).unwrap(),
                regime,
                "the wire value must round-trip",
            );
        }
    }

    /// Pin the threshold from both sides using the TIGHTEST real shapes, so a
    /// future nudge of the cutoff cannot slip past a test with slack in it.
    #[test]
    fn the_threshold_boundary_is_pinned_from_both_sides() {
        // Tightest caught shape: the smallest star above the verdict floor.
        let star = store();
        let hub = page(&star, "hub");
        for i in 0..4 {
            let leaf = page(&star, &format!("leaf {i}"));
            star.add_relation(hub, leaf).unwrap();
        }
        let star_health = star
            .graph_health(HealthThresholds::default())
            .unwrap()
            .unwrap();
        assert!(
            (star_health.hub_excess - 0.375).abs() < 1e-9,
            "expected ~0.375, got {}",
            star_health.hub_excess,
        );
        assert_eq!(star_health.regime(), GraphRegime::HubDominated);

        // Tightest healthy shape: a 6-node path, the closest a plausible chain
        // gets to the cutoff from below.
        let path = store();
        let ids: Vec<i64> = (0..6).map(|i| page(&path, &format!("p{i}"))).collect();
        for window in ids.windows(2) {
            path.add_relation(window[0], window[1]).unwrap();
        }
        let path_health = path
            .graph_health(HealthThresholds::default())
            .unwrap()
            .unwrap();
        assert!(
            (path_health.hub_excess - 0.2).abs() < 1e-9,
            "expected ~0.2, got {}",
            path_health.hub_excess,
        );
        assert_eq!(
            path_health.regime(),
            GraphRegime::Healthy,
            "the tightest healthy shape must stay healthy",
        );
    }

    /// Generation-2 BLOCKER: the 0.35 cutoff sat inside the star ramp, so
    /// small stars read Healthy. Every star at or above the verdict floor must
    /// be caught.
    #[test]
    fn the_smallest_meaningful_stars_are_still_caught() {
        for leaves in 4..=8usize {
            let store = store();
            let hub = page(&store, "hub");
            for i in 0..leaves {
                let leaf = page(&store, &format!("leaf {i}"));
                store.add_relation(hub, leaf).unwrap();
            }
            let health = store
                .graph_health(HealthThresholds::default())
                .unwrap()
                .unwrap();
            assert_eq!(
                health.regime(),
                GraphRegime::HubDominated,
                "a star with {leaves} leaves must be caught (excess {}, k {})",
                health.hub_excess,
                health.hub_k,
            );
        }
    }

    /// Generation-4 finding: below five nodes a top-k endpoint share provably
    /// cannot tell a star from a path, so any hub verdict there would flag one
    /// of the two wrongly. This pins both halves — that the two shapes really
    /// are indistinguishable, and that the metric refuses rather than guesses.
    #[test]
    fn below_five_nodes_no_hub_verdict_is_issued_because_none_is_decidable() {
        let star = store();
        let hub = page(&star, "hub");
        for i in 0..3 {
            let leaf = page(&star, &format!("leaf {i}"));
            star.add_relation(hub, leaf).unwrap();
        }
        let star_health = star
            .graph_health(HealthThresholds::default())
            .unwrap()
            .unwrap();

        let path = store();
        let ids: Vec<i64> = (0..4).map(|i| page(&path, &format!("p{i}"))).collect();
        for window in ids.windows(2) {
            path.add_relation(window[0], window[1]).unwrap();
        }
        let path_health = path
            .graph_health(HealthThresholds::default())
            .unwrap()
            .unwrap();

        assert_eq!(star_health.degrees, vec![3, 1, 1, 1]);
        assert_eq!(path_health.degrees, vec![2, 2, 1, 1]);
        assert_eq!(
            star_health.hub_excess, path_health.hub_excess,
            "a 4-node star and a 4-node path are the same number to this metric",
        );
        assert_eq!(star_health.regime(), GraphRegime::Healthy);
        assert_eq!(path_health.regime(), GraphRegime::Healthy);

        // Fragmentation is still decidable at any size and is still reported.
        let sparse = store();
        for i in 0..4 {
            page(&sparse, &format!("orphan {i}"));
        }
        assert_eq!(
            sparse
                .graph_health(HealthThresholds::default())
                .unwrap()
                .unwrap()
                .regime(),
            GraphRegime::Fragmented,
            "refusing a HUB verdict must not suppress the fragmentation verdict",
        );
    }

    /// Thresholds are configurable because the defaults are judgement calls.
    #[test]
    fn thresholds_are_configurable() {
        let store = store();
        let hub = page(&store, "hub");
        for i in 0..5 {
            let leaf = page(&store, &format!("leaf {i}"));
            store.add_relation(hub, leaf).unwrap();
        }
        let permissive = HealthThresholds {
            hub_dominated_at_excess: 0.99,
            ..HealthThresholds::default()
        };
        let health = store.graph_health(permissive).unwrap().unwrap();
        assert_eq!(
            health.regime(),
            GraphRegime::Healthy,
            "a star can be acceptable if the caller says so",
        );
        assert_eq!(
            health.thresholds(),
            permissive,
            "the summary carries the thresholds it was measured with",
        );
    }

    /// `hub_k` is clamped to at most half the graph.
    ///
    /// Generation-2 red-team finding: clamping to `nodes` drove the floor to
    /// 1.0, which forced `hub_excess` to 0 and silently turned hub detection
    /// OFF — widening the knob disabled the metric instead of sharpening it.
    #[test]
    fn hub_k_is_clamped_to_half_the_graph() {
        let store = store();
        let a = page(&store, "a");
        let b = page(&store, "b");
        store.add_relation(a, b).unwrap();

        let health = store
            .graph_health(HealthThresholds {
                hub_k: 50,
                ..HealthThresholds::default()
            })
            .unwrap()
            .unwrap();
        assert_eq!(health.hub_k, 1, "half of 2 nodes, never the whole graph");
        assert_eq!(health.hub_share, 0.5);
        assert_eq!(
            health.hub_excess, 0.0,
            "a 2-node edge is uniform, so there is no excess concentration",
        );
    }

    /// Generation-2 BLOCKER: widening `hub_k` past the node count made an
    /// unambiguous star report Healthy. Detection must not be switchable off
    /// by a knob documented as making it more sensitive.
    #[test]
    fn a_large_hub_k_cannot_disable_detection() {
        let store = store();
        let hub = page(&store, "hub");
        for i in 0..5 {
            let leaf = page(&store, &format!("leaf {i}"));
            store.add_relation(hub, leaf).unwrap();
        }
        for hub_k in [3usize, 6, 7, 50, usize::MAX] {
            let health = store
                .graph_health(HealthThresholds {
                    hub_k,
                    ..HealthThresholds::default()
                })
                .unwrap()
                .unwrap();
            assert_eq!(
                health.regime(),
                GraphRegime::HubDominated,
                "a 6-node star must stay hub-dominated at hub_k={hub_k}                  (excess {}, clamped k {})",
                health.hub_excess,
                health.hub_k,
            );
        }
    }
}
