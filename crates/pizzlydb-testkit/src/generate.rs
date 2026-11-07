//! Seeded histories and a proptest strategy.
//!
//! [`generate_history`] is deterministic: the same seed, length, and bias
//! produce the same operations. [`history_strategy`] is the shrinkable
//! proptest source. A failing [`crate::run_history`] prints
//! `PIZZLYDB_SEED=<seed>`. Replaying a seed from [`generate_history`] uses the
//! same length and [`WorkloadBias`].

use pizzlydb_core::{Value, ValueType};
use proptest::prelude::*;
use rand::RngExt;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;

use crate::reference::{
    GraphSnapshot, History, ModelScope, NAME_KEY, NAME_LABEL, NAME_TYPE, Operation, ReferenceGraph,
};

/// Number of operations in the graph property test.
pub const PROPERTY_STEPS: usize = 8;

/// Weights for interesting graph shapes.
///
/// Each field is a probability in `0.0..=1.0`. Values outside that range are
/// clamped by the generator.
///
/// # Examples
///
/// ```
/// use pizzlydb_testkit::WorkloadBias;
///
/// let bias = WorkloadBias::default();
/// assert!(bias.hubs > 0.0);
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct WorkloadBias {
    /// Repeat an existing endpoint pair.
    pub parallel_edges: f64,
    /// Use the source as the target.
    pub self_loops: f64,
    /// Point an edge back at the oldest node.
    pub cycles: f64,
    /// Prefer the oldest node as an endpoint.
    pub hubs: f64,
    /// Delete a node that has no edges, then keep allocating.
    pub delete_recreate: f64,
    /// Add and remove labels.
    pub label_churn: f64,
    /// Give two nodes the same value and create a unique index.
    pub unique_collisions: f64,
    /// Store null properties.
    pub nulls: f64,
}

impl Default for WorkloadBias {
    fn default() -> Self {
        Self {
            parallel_edges: 0.35,
            self_loops: 0.2,
            cycles: 0.25,
            hubs: 0.45,
            delete_recreate: 0.2,
            label_churn: 0.25,
            unique_collisions: 0.15,
            nulls: 0.2,
        }
    }
}

/// Build `len` operations from `seed`.
///
/// # Examples
///
/// ```
/// use pizzlydb_testkit::{WorkloadBias, generate_history};
///
/// let once = generate_history(9, 5, &WorkloadBias::default());
/// let twice = generate_history(9, 5, &WorkloadBias::default());
/// assert_eq!(once, twice);
/// assert_eq!(once.seed, 9);
/// ```
#[must_use]
pub fn generate_history(seed: u64, len: usize, bias: &WorkloadBias) -> History {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut model = ReferenceGraph::new();
    let mut names = 0u32;
    let mut ops = Vec::with_capacity(len);

    while ops.len() < len {
        if rng.random::<f64>() < bias.unique_collisions && len - ops.len() >= 9 {
            let script = unique_script(&model, &mut names);
            for op in script {
                if ops.len() == len {
                    break;
                }
                let _ = model.apply(&op);
                ops.push(op);
            }
            continue;
        }

        let op = propose(&mut rng, &model, bias, &mut names);
        let _ = model.apply(&op);
        ops.push(op);
    }

    History { seed, ops }
}

/// Shrinkable histories of 0 to 12 operations.
///
/// The history seed is a hash of the raw draw. Shrinking changes the
/// operations. [`generate_history`] is the path that `PIZZLYDB_SEED` replays.
///
/// # Examples
///
/// ```
/// use pizzlydb_testkit::history_strategy;
/// use proptest::strategy::{Strategy, ValueTree};
/// use proptest::test_runner::TestRunner;
///
/// let mut runner = TestRunner::default();
/// let history = history_strategy().new_tree(&mut runner).unwrap().current();
/// assert!(history.ops.len() <= 12);
/// ```
pub fn history_strategy() -> impl Strategy<Value = History> {
    proptest::collection::vec(raw_strategy(), 0..=12).prop_map(|raw| History {
        seed: hash_raw(&raw),
        ops: raw.iter().map(compile_raw).collect(),
    })
}

fn propose(
    rng: &mut ChaCha8Rng,
    model: &ReferenceGraph,
    bias: &WorkloadBias,
    names: &mut u32,
) -> Operation {
    let snapshot = model.snapshot().expect("reference snapshot");
    let namespace = if rng.random::<f64>() < 0.08 { 2 } else { 1 };
    let nodes = ids_nodes(&snapshot, namespace);
    let types = catalog(&snapshot, namespace, NAME_TYPE);
    let labels = catalog(&snapshot, namespace, NAME_LABEL);
    let keys = catalog(&snapshot, namespace, NAME_KEY);
    let roll = rng.random_range(0..100u32);

    if nodes.is_empty() || roll < 12 {
        return Operation::CreateNode { namespace };
    }

    if types.is_empty() || roll < 18 {
        return Operation::InternType {
            namespace,
            name: fresh(names, "T"),
        };
    }

    if keys.is_empty() || roll < 24 {
        return Operation::InternKey {
            namespace,
            name: fresh(names, "K"),
        };
    }

    if labels.is_empty() || (roll < 32 && rng.random::<f64>() < bias.label_churn) {
        return Operation::InternLabel {
            namespace,
            name: fresh(names, "L"),
        };
    }

    if roll < 55 {
        return edge_op(rng, bias, namespace, &nodes, &types, &snapshot);
    }

    if roll < 64
        && rng.random::<f64>() < bias.delete_recreate
        && let Some(node) = nodes
            .iter()
            .copied()
            .find(|node| !incident(&snapshot, namespace, *node))
    {
        return Operation::DeleteNode { namespace, node };
    }

    if roll < 74 && rng.random::<f64>() < bias.label_churn {
        let node = pick(rng, &nodes);
        let label = pick(rng, &labels);
        if let (Some(node), Some(label)) = (node, label) {
            if rng.random::<bool>() {
                return Operation::AddLabel {
                    namespace,
                    node,
                    label,
                };
            }
            return Operation::RemoveLabel {
                namespace,
                node,
                label,
            };
        }
    }

    if roll < 88 {
        let node = pick(rng, &nodes).unwrap_or(1);
        let key = pick(rng, &keys).unwrap_or(1);
        return Operation::SetNodeProperty {
            namespace,
            node,
            key,
            value: value_for(rng, bias),
        };
    }

    if roll < 94 {
        return Operation::CreateIndex {
            namespace,
            scope: ModelScope::Label(pick(rng, &labels).unwrap_or(1)),
            key: pick(rng, &keys).unwrap_or(1),
            unique: rng.random::<bool>(),
        };
    }

    if roll < 97 {
        return Operation::Declare {
            namespace,
            scope: ModelScope::Label(pick(rng, &labels).unwrap_or(1)),
            key: pick(rng, &keys).unwrap_or(1),
            value_type: ValueType::Int,
            nullable: rng.random::<bool>(),
        };
    }

    Operation::SetNodeProperty {
        namespace,
        node: pick(rng, &nodes).unwrap_or(99),
        key: pick(rng, &keys).unwrap_or(99),
        value: Value::Float(f64::NAN),
    }
}

fn edge_op(
    rng: &mut ChaCha8Rng,
    bias: &WorkloadBias,
    namespace: u128,
    nodes: &[u128],
    types: &[u128],
    snapshot: &GraphSnapshot,
) -> Operation {
    let edge_type = pick(rng, types).unwrap_or(1);

    if rng.random::<f64>() < bias.parallel_edges
        && let Some((source, target)) = existing_pair(snapshot, namespace)
    {
        return Operation::CreateEdge {
            namespace,
            source,
            target,
            edge_type,
        };
    }

    let source = if rng.random::<f64>() < bias.hubs {
        nodes[0]
    } else {
        pick(rng, nodes).unwrap_or(1)
    };

    let target = if rng.random::<f64>() < bias.self_loops {
        source
    } else if rng.random::<f64>() < bias.cycles {
        *nodes.last().unwrap_or(&source)
    } else if rng.random::<f64>() < bias.hubs {
        nodes[0]
    } else {
        pick(rng, nodes).unwrap_or(source)
    };

    Operation::CreateEdge {
        namespace,
        source,
        target,
        edge_type,
    }
}

fn unique_script(model: &ReferenceGraph, names: &mut u32) -> Vec<Operation> {
    let snapshot = model.snapshot().expect("reference snapshot");
    let counters = snapshot
        .counters
        .get(&1)
        .copied()
        .unwrap_or(crate::Counters {
            next_node: 1,
            next_edge: 1,
            next_label: 1,
            next_type: 1,
            next_key: 1,
            next_index: 1,
        });

    let label = counters.next_label;
    let key = counters.next_key;
    let first = counters.next_node;
    let second = counters.next_node.saturating_add(1);

    vec![
        Operation::InternLabel {
            namespace: 1,
            name: fresh(names, "U"),
        },
        Operation::InternKey {
            namespace: 1,
            name: fresh(names, "E"),
        },
        Operation::CreateNode { namespace: 1 },
        Operation::CreateNode { namespace: 1 },
        Operation::SetNodeProperty {
            namespace: 1,
            node: first,
            key,
            value: Value::from("same"),
        },
        Operation::SetNodeProperty {
            namespace: 1,
            node: second,
            key,
            value: Value::from("same"),
        },
        Operation::AddLabel {
            namespace: 1,
            node: first,
            label,
        },
        Operation::AddLabel {
            namespace: 1,
            node: second,
            label,
        },
        Operation::CreateIndex {
            namespace: 1,
            scope: ModelScope::Label(label),
            key,
            unique: true,
        },
    ]
}

fn value_for(rng: &mut ChaCha8Rng, bias: &WorkloadBias) -> Value {
    if rng.random::<f64>() < bias.nulls {
        return Value::Null;
    }

    match rng.random_range(0..4u8) {
        0 => Value::Bool(rng.random::<bool>()),
        1 => Value::Int(i64::from(rng.random_range(-3..6i16))),
        2 => Value::from(format!("v{}", rng.random_range(0..4u8))),
        _ => Value::Bytes(vec![rng.random::<u8>()]),
    }
}

fn fresh(names: &mut u32, prefix: &str) -> String {
    let name = format!("{prefix}{names}");
    *names += 1;
    name
}

fn ids_nodes(snapshot: &GraphSnapshot, namespace: u128) -> Vec<u128> {
    snapshot
        .nodes
        .iter()
        .filter(|(ns, _)| *ns == namespace)
        .map(|(_, id)| *id)
        .collect()
}

fn catalog(snapshot: &GraphSnapshot, namespace: u128, kind: u8) -> Vec<u128> {
    snapshot
        .names
        .iter()
        .filter(|((ns, name_kind, _), _)| *ns == namespace && *name_kind == kind)
        .map(|((_, _, id), _)| *id)
        .collect()
}

fn incident(snapshot: &GraphSnapshot, namespace: u128, node: u128) -> bool {
    snapshot
        .edges
        .iter()
        .any(|((ns, _), edge)| *ns == namespace && (edge.source == node || edge.target == node))
}

fn existing_pair(snapshot: &GraphSnapshot, namespace: u128) -> Option<(u128, u128)> {
    snapshot
        .edges
        .iter()
        .find_map(|((ns, _), edge)| (*ns == namespace).then_some((edge.source, edge.target)))
}

fn pick(rng: &mut ChaCha8Rng, items: &[u128]) -> Option<u128> {
    if items.is_empty() {
        None
    } else {
        Some(items[rng.random_range(0..items.len())])
    }
}

#[derive(Clone, Debug)]
struct Raw {
    kind: u8,
    a: u16,
    b: u16,
    c: u16,
    name: u8,
    number: i16,
    flag: u8,
}

fn raw_strategy() -> impl Strategy<Value = Raw> {
    (
        any::<u8>(),
        any::<u16>(),
        any::<u16>(),
        any::<u16>(),
        any::<u8>(),
        any::<i16>(),
        any::<u8>(),
    )
        .prop_map(|(kind, a, b, c, name, number, flag)| Raw {
            kind,
            a,
            b,
            c,
            name,
            number,
            flag,
        })
}

fn compile_raw(raw: &Raw) -> Operation {
    let namespace = if raw.a == 0 {
        0
    } else {
        u128::from((raw.a % 3) + 1)
    };

    let left = u128::from(raw.b);
    let right = u128::from(raw.c);
    let name = raw_name(raw);

    match raw.kind % 16 {
        0 => Operation::CreateNode { namespace },
        1 => Operation::DeleteNode {
            namespace,
            node: left,
        },
        2 => Operation::CreateEdge {
            namespace,
            source: left,
            target: right,
            edge_type: u128::from(raw.name),
        },
        3 => Operation::DeleteEdge {
            namespace,
            edge: left,
        },
        4 => Operation::InternLabel { namespace, name },
        5 => Operation::InternType { namespace, name },
        6 => Operation::InternKey { namespace, name },
        7 => Operation::RenameLabel {
            namespace,
            id: left,
            name,
        },
        8 => Operation::AddLabel {
            namespace,
            node: left,
            label: right,
        },
        9 => Operation::RemoveLabel {
            namespace,
            node: left,
            label: right,
        },
        10 => Operation::SetNodeProperty {
            namespace,
            node: left,
            key: right,
            value: raw_value(raw),
        },
        11 => Operation::SetEdgeProperty {
            namespace,
            edge: left,
            key: right,
            value: raw_value(raw),
        },
        12 => Operation::ClearNodeProperty {
            namespace,
            node: left,
            key: right,
        },
        13 => Operation::Declare {
            namespace,
            scope: ModelScope::Label(left),
            key: right,
            value_type: ValueType::Int,
            nullable: raw.flag.is_multiple_of(2),
        },
        14 => Operation::CreateIndex {
            namespace,
            scope: ModelScope::Type(left),
            key: right,
            unique: raw.flag.is_multiple_of(2),
        },
        _ => Operation::ClearEdgeProperty {
            namespace,
            edge: left,
            key: right,
        },
    }
}

fn raw_name(raw: &Raw) -> String {
    if raw.flag == 0 {
        "\0".to_owned()
    } else if raw.flag == 1 {
        String::new()
    } else {
        format!("n{}", raw.name)
    }
}

fn raw_value(raw: &Raw) -> Value {
    if raw.flag == 255 {
        return Value::Float(f64::NAN);
    }

    match raw.flag % 5 {
        0 => Value::Null,
        1 => Value::Bool(raw.number < 0),
        2 => Value::Int(i64::from(raw.number)),
        3 => Value::from(raw_name(raw)),
        _ => Value::Bytes(vec![raw.name]),
    }
}

fn hash_raw(raw: &[Raw]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for item in raw {
        for byte in [item.kind, item.name, item.flag] {
            hash = hash
                .wrapping_mul(0x0000_0100_0000_01b3)
                .wrapping_add(u64::from(byte));
        }
        hash = hash.wrapping_add(u64::from(item.a));
        hash = hash.wrapping_add(u64::from(item.b) << 16);
        hash = hash.wrapping_add(u64::from(item.c) << 32);
        hash = hash.wrapping_add(u64::from(item.number as u16) << 48);
    }

    hash
}

#[cfg(test)]
mod tests {
    use super::{WorkloadBias, generate_history};
    use crate::reference::{DIR_OUT, ReferenceGraph};

    #[test]
    fn the_same_seed_is_the_same_history() {
        let bias = WorkloadBias::default();
        assert_eq!(
            generate_history(42, 20, &bias),
            generate_history(42, 20, &bias)
        );
    }

    #[test]
    fn hubs_receive_more_edges_than_the_tail() {
        let bias = WorkloadBias {
            hubs: 1.0,
            self_loops: 0.0,
            parallel_edges: 0.0,
            cycles: 1.0,
            ..WorkloadBias::default()
        };
        let mut toward_oldest = 0usize;
        let mut edges = 0usize;
        for seed in 0..30 {
            let history = generate_history(seed, 24, &bias);
            let mut model = ReferenceGraph::new();
            for op in &history.ops {
                let _ = model.apply(op);
            }
            for neighbor in &model.snapshot().unwrap().neighbors {
                if neighbor.namespace == 1 && neighbor.direction == DIR_OUT {
                    edges += 1;
                    if neighbor.neighbor == 1 || neighbor.node == 1 {
                        toward_oldest += 1;
                    }
                }
            }
        }
        assert!(edges > 20, "edges {edges}");
        assert!(toward_oldest * 2 > edges, "hub {toward_oldest} of {edges}");
    }
}
