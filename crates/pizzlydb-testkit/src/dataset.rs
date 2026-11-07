//! Seeded graphs for benchmarks.
//!
//! Identifiers are `1..=nodes`. Edges are directed pairs. The generators do
//! not open a database.

use rand::RngExt;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;

/// Nodes and directed edges.
///
/// Node identifiers are `1..=node_count`. An edge is `(source, target)`.
///
/// # Examples
///
/// ```
/// use pizzlydb_testkit::uniform_graph;
///
/// let graph = uniform_graph(1, 4, 3);
/// assert_eq!(graph.nodes.len(), 4);
/// assert_eq!(graph.edges.len(), 3);
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dataset {
    /// Node identifiers.
    pub nodes: Vec<u128>,
    /// Directed edges.
    pub edges: Vec<(u128, u128)>,
}

/// `nodes` vertices and `edges` endpoints chosen uniformly.
///
/// # Examples
///
/// ```
/// use pizzlydb_testkit::uniform_graph;
///
/// let graph = uniform_graph(2, 1, 1);
/// assert_eq!(graph.edges, vec![(1, 1)]);
/// ```
#[must_use]
pub fn uniform_graph(seed: u64, nodes: usize, edges: usize) -> Dataset {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    with_edges(
        nodes,
        edges,
        |rng_edges| rng_edges.random_range(1..=nodes as u128),
        &mut rng,
    )
}

/// `nodes` vertices and `edges` whose targets prefer a few hubs.
///
/// A target is `1 + floor(u^3 * nodes)` for `u` uniform in `0.0..1.0`, so low
/// identifiers are chosen much more often than high ones.
///
/// # Examples
///
/// ```
/// use pizzlydb_testkit::power_law_graph;
///
/// let graph = power_law_graph(3, 20, 40);
/// assert_eq!(graph.edges.len(), 40);
/// ```
#[must_use]
pub fn power_law_graph(seed: u64, nodes: usize, edges: usize) -> Dataset {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    with_edges(
        nodes,
        edges,
        |rng_edges| {
            let unit = rng_edges.random::<f64>();
            let offset = (unit * unit * unit) * (nodes as f64);
            1 + (offset as u128).min(nodes as u128 - 1)
        },
        &mut rng,
    )
}

/// Communities of `community_size` nodes, dense inside and sparse between.
///
/// Each community is a directed cycle plus `community_size` extra internal
/// edges. `bridge_edges` edges connect different communities. The result is
/// empty when `communities` or `community_size` is zero.
///
/// # Examples
///
/// ```
/// use pizzlydb_testkit::social_graph;
///
/// let graph = social_graph(4, 2, 3, 1);
/// assert_eq!(graph.nodes.len(), 6);
/// assert!(graph.edges.len() > 1);
/// ```
#[must_use]
pub fn social_graph(
    seed: u64,
    communities: usize,
    community_size: usize,
    bridge_edges: usize,
) -> Dataset {
    if communities == 0 || community_size == 0 {
        return Dataset {
            nodes: Vec::new(),
            edges: Vec::new(),
        };
    }

    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let node_count = communities.saturating_mul(community_size);
    let nodes: Vec<u128> = (1..=node_count as u128).collect();
    let mut edges = Vec::new();

    for community in 0..communities {
        let start = 1 + (community * community_size) as u128;
        let end = start + community_size as u128;

        for node in start..end {
            let next = if node + 1 == end { start } else { node + 1 };
            edges.push((node, next));
        }

        for _ in 0..community_size {
            let source = rng.random_range(start..end);
            let target = rng.random_range(start..end);
            edges.push((source, target));
        }
    }

    if communities > 1 {
        for _ in 0..bridge_edges {
            let left = rng.random_range(0..communities);
            let mut right = rng.random_range(0..communities - 1);
            if right >= left {
                right += 1;
            }
            let source = community_node(&mut rng, left, community_size);
            let target = community_node(&mut rng, right, community_size);
            edges.push((source, target));
        }
    }

    Dataset { nodes, edges }
}

fn with_edges(
    nodes: usize,
    edges: usize,
    target: impl Fn(&mut ChaCha8Rng) -> u128,
    rng: &mut ChaCha8Rng,
) -> Dataset {
    if nodes == 0 {
        return Dataset {
            nodes: Vec::new(),
            edges: Vec::new(),
        };
    }

    let ids: Vec<u128> = (1..=nodes as u128).collect();
    let mut pairs = Vec::with_capacity(edges);

    for _ in 0..edges {
        let source = rng.random_range(1..=nodes as u128);
        pairs.push((source, target(rng)));
    }

    Dataset {
        nodes: ids,
        edges: pairs,
    }
}

fn community_node(rng: &mut ChaCha8Rng, community: usize, community_size: usize) -> u128 {
    let start = 1 + (community * community_size) as u128;
    rng.random_range(start..start + community_size as u128)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{power_law_graph, social_graph, uniform_graph};

    #[test]
    fn uniform_targets_are_not_a_single_hub() {
        let graph = uniform_graph(8, 16, 400);
        let mut counts = BTreeMap::<u128, usize>::new();
        for (_, target) in graph.edges {
            *counts.entry(target).or_default() += 1;
        }
        let busiest = counts.values().copied().max().unwrap_or(0);
        assert!(busiest < 80, "busiest {busiest}");
    }

    #[test]
    fn power_law_has_a_hub() {
        let graph = power_law_graph(8, 32, 400);
        let mut counts = vec![0usize; 33];
        for (_, target) in graph.edges {
            counts[target as usize] += 1;
        }
        let hub = counts[1];
        let tail = counts[32];
        assert!(hub > tail * 3, "hub {hub} tail {tail}");
    }

    #[test]
    fn social_edges_stay_mostly_inside_communities() {
        let graph = social_graph(1, 4, 5, 2);
        let mut internal = 0usize;
        let mut bridge = 0usize;
        for (source, target) in graph.edges {
            let left = (source - 1) / 5;
            let right = (target - 1) / 5;
            if left == right {
                internal += 1;
            } else {
                bridge += 1;
            }
        }
        assert!(internal > bridge, "internal {internal} bridge {bridge}");
    }
}
