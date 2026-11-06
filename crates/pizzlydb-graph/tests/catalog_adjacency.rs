//! Catalog names, node labels, and both adjacency directions.

mod common;

use std::collections::BTreeMap;

use common::{ADJACENCY, Rng, count_kind, crash_and_reopen, create, dump, graph};
use pizzlydb_core::{LabelId, NodeId, PropertyKeyId, TypeId};
use pizzlydb_graph::{Direction, GraphBatch, GraphError, MAX_NAME_LEN, Neighbor};
use pizzlydb_storage::MemoryVfs;

#[test]
fn names_are_exact_bytes_per_kind() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let upper: LabelId = graph.intern(&mut batch, "A").unwrap();
    let lower: LabelId = graph.intern(&mut batch, "a").unwrap();
    let composed: LabelId = graph.intern(&mut batch, "\u{e9}").unwrap();
    let decomposed: LabelId = graph.intern(&mut batch, "e\u{301}").unwrap();
    let edge_type: TypeId = graph.intern(&mut batch, "A").unwrap();
    let key: PropertyKeyId = graph.intern(&mut batch, "A").unwrap();
    assert_ne!(upper, lower);
    assert_ne!(composed, decomposed);
    assert_eq!(upper.to_u128(), edge_type.to_u128());
    assert_eq!(upper.to_u128(), key.to_u128());

    let staged = dump(&batch);
    assert_eq!(graph.intern::<LabelId, _>(&mut batch, "A").unwrap(), upper);
    assert_eq!(
        graph.intern::<TypeId, _>(&mut batch, b"A").unwrap(),
        edge_type
    );
    assert_eq!(dump(&batch), staged);
    batch.commit().unwrap();

    let storage = crash_and_reopen(&vfs, storage);
    let view = storage.snapshot().unwrap();
    assert_eq!(graph.lookup(&view, "A").unwrap(), Some(upper));
    assert_eq!(graph.lookup(&view, "a").unwrap(), Some(lower));
    assert_eq!(graph.lookup(&view, "A").unwrap(), Some(edge_type));
    assert_eq!(graph.lookup::<TypeId, _>(&view, "a").unwrap(), None);
    assert_eq!(graph.name(&view, decomposed).unwrap(), "e\u{301}");
    assert_eq!(graph.name(&view, key).unwrap(), "A");
}

#[test]
fn invalid_names_write_nothing() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let overlong = vec![b'x'; MAX_NAME_LEN + 1];
    let multibyte_overlong = "\u{e9}".repeat(MAX_NAME_LEN / 2) + "x";
    let invalid: [&[u8]; 6] = [
        b"",
        b"nul\0inside",
        &[0xC3, 0x28],
        &[0xFF],
        &overlong,
        multibyte_overlong.as_bytes(),
    ];
    for name in invalid {
        assert!(matches!(
            graph.intern::<LabelId, _>(&mut batch, name),
            Err(GraphError::InvalidName)
        ));
        assert!(matches!(
            graph.lookup::<TypeId, _>(&batch, name),
            Err(GraphError::InvalidName)
        ));
    }
    assert!(batch.is_empty());

    let longest = "\u{e9}".repeat(MAX_NAME_LEN / 2);
    let id: PropertyKeyId = graph.intern(&mut batch, &longest).unwrap();
    assert_eq!(graph.name(&batch, id).unwrap(), longest);
}

#[test]
fn rename_moves_the_name() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let person: LabelId = graph.intern(&mut batch, "Person").unwrap();
    let robot: LabelId = graph.intern(&mut batch, "Robot").unwrap();
    batch.commit().unwrap();

    let mut batch = GraphBatch::begin(&storage).unwrap();
    graph.rename(&mut batch, person, "Person").unwrap();
    assert!(batch.is_empty());
    assert!(matches!(
        graph.rename(&mut batch, person, "Robot"),
        Err(GraphError::NameConflict)
    ));
    assert!(matches!(
        graph.rename(&mut batch, person, ""),
        Err(GraphError::InvalidName)
    ));
    assert!(matches!(
        graph.rename(&mut batch, LabelId::from_u128(50).unwrap(), "Ghost"),
        Err(GraphError::InvalidIdentity)
    ));
    assert!(batch.is_empty());

    graph.rename(&mut batch, person, "Human").unwrap();
    let reused: LabelId = graph.intern(&mut batch, "Person").unwrap();
    assert_ne!(reused, person);
    batch.commit().unwrap();

    let view = storage.snapshot().unwrap();
    assert_eq!(graph.name(&view, person).unwrap(), "Human");
    assert_eq!(graph.lookup(&view, "Human").unwrap(), Some(person));
    assert_eq!(graph.lookup(&view, "Person").unwrap(), Some(reused));
    assert_eq!(graph.lookup(&view, "Robot").unwrap(), Some(robot));
}

#[test]
fn labels_and_membership_change_together() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let a: LabelId = graph.intern(&mut batch, "A").unwrap();
    let b: LabelId = graph.intern(&mut batch, "B").unwrap();
    let first = graph.create_node(&mut batch).unwrap();
    let second = graph.create_node(&mut batch).unwrap();
    graph.set_labels(&mut batch, first, &[b, a, b]).unwrap();
    graph.add_label(&mut batch, second, a).unwrap();

    let staged = dump(&batch);
    graph.add_label(&mut batch, second, a).unwrap();
    graph.remove_label(&mut batch, second, b).unwrap();
    assert_eq!(dump(&batch), staged);
    assert!(matches!(
        graph.add_label(&mut batch, second, LabelId::from_u128(99).unwrap()),
        Err(GraphError::InvalidIdentity)
    ));
    assert!(matches!(
        graph.add_label(&mut batch, NodeId::from_u128(99).unwrap(), a),
        Err(GraphError::NotFound)
    ));
    assert_eq!(dump(&batch), staged);
    batch.commit().unwrap();

    let view = storage.snapshot().unwrap();
    let labels: Vec<LabelId> = graph
        .labels(&view, first)
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(labels, vec![a, b]);
    let members: Vec<NodeId> = graph
        .nodes_with_label(&view, a)
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(members, vec![first, second]);
    drop(view);

    let mut batch = GraphBatch::begin(&storage).unwrap();
    graph.set_labels(&mut batch, first, &[b]).unwrap();
    graph.delete_node(&mut batch, second).unwrap();
    batch.commit().unwrap();

    let view = storage.snapshot().unwrap();
    assert_eq!(graph.nodes_with_label(&view, a).unwrap().count(), 0);
    assert_eq!(graph.labels(&view, second).unwrap().count(), 0);
    assert!(graph.has_label(&view, first, b).unwrap());
    assert!(!graph.has_label(&view, first, a).unwrap());
}

#[test]
fn self_loops_and_parallel_edges_are_ordinary_keys() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let knows: TypeId = graph.intern(&mut batch, "KNOWS").unwrap();
    let likes: TypeId = graph.intern(&mut batch, "LIKES").unwrap();
    let a = graph.create_node(&mut batch).unwrap();
    let b = graph.create_node(&mut batch).unwrap();
    let lonely = graph.create_node(&mut batch).unwrap();
    let loop_a = graph.create_edge(&mut batch, a, a, knows).unwrap();
    let first = graph.create_edge(&mut batch, a, b, likes).unwrap();
    let second = graph.create_edge(&mut batch, a, b, likes).unwrap();
    batch.commit().unwrap();

    let view = storage.snapshot().unwrap();
    let out: Vec<Neighbor> = graph
        .neighbors(&view, a, Direction::Out)
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        out,
        vec![
            Neighbor {
                edge: loop_a,
                edge_type: knows,
                neighbor: a
            },
            Neighbor {
                edge: first,
                edge_type: likes,
                neighbor: b
            },
            Neighbor {
                edge: second,
                edge_type: likes,
                neighbor: b
            },
        ]
    );
    let incoming: Vec<Neighbor> = graph
        .neighbors(&view, a, Direction::In)
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        incoming,
        vec![Neighbor {
            edge: loop_a,
            edge_type: knows,
            neighbor: a
        }]
    );
    assert_eq!(
        graph
            .neighbors_of_type(&view, b, Direction::In, likes)
            .unwrap()
            .count(),
        2
    );
    assert_eq!(
        graph
            .neighbors_of_type(&view, b, Direction::In, knows)
            .unwrap()
            .count(),
        0
    );
    for direction in [Direction::Out, Direction::In] {
        assert_eq!(
            graph.neighbors(&view, lonely, direction).unwrap().count(),
            0
        );
        let never = NodeId::from_u128(1_000).unwrap();
        assert_eq!(graph.neighbors(&view, never, direction).unwrap().count(), 0);
    }
    assert_eq!(count_kind(&view, ADJACENCY), 6);
}

#[test]
fn dropped_edge_batch_leaves_no_rows() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let knows: TypeId = graph.intern(&mut batch, "KNOWS").unwrap();
    let a = graph.create_node(&mut batch).unwrap();
    let b = graph.create_node(&mut batch).unwrap();
    batch.commit().unwrap();
    let committed = dump(&storage.snapshot().unwrap());

    let mut batch = GraphBatch::begin(&storage).unwrap();
    graph.create_edge(&mut batch, a, b, knows).unwrap();
    assert_eq!(count_kind(&batch, ADJACENCY), 2);
    drop(batch);
    let storage = crash_and_reopen(&vfs, storage);
    let view = storage.snapshot().unwrap();
    assert_eq!(dump(&view), committed);
    assert_eq!(count_kind(&view, ADJACENCY), 0);
}

#[test]
fn old_snapshots_do_not_see_new_neighbors() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let knows: TypeId = graph.intern(&mut batch, "KNOWS").unwrap();
    let a = graph.create_node(&mut batch).unwrap();
    let b = graph.create_node(&mut batch).unwrap();
    batch.commit().unwrap();

    let old = storage.snapshot().unwrap();
    let mut batch = GraphBatch::begin(&storage).unwrap();
    let edge = graph.create_edge(&mut batch, a, b, knows).unwrap();
    batch.commit().unwrap();
    let new = storage.snapshot().unwrap();

    let mut batch = GraphBatch::begin(&storage).unwrap();
    graph.delete_edge(&mut batch, edge).unwrap();
    batch.commit().unwrap();
    let latest = storage.snapshot().unwrap();

    assert_eq!(graph.neighbors(&old, a, Direction::Out).unwrap().count(), 0);
    assert_eq!(graph.neighbors(&old, b, Direction::In).unwrap().count(), 0);
    assert_eq!(graph.neighbors(&new, a, Direction::Out).unwrap().count(), 1);
    assert_eq!(graph.neighbors(&new, b, Direction::In).unwrap().count(), 1);
    assert_eq!(count_kind(&latest, ADJACENCY), 0);
}

#[test]
fn namespaces_do_not_share_keys() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let left = graph(1);
    let right = graph(2);

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let left_type: TypeId = left.intern(&mut batch, "KNOWS").unwrap();
    let right_type: TypeId = right.intern(&mut batch, "KNOWS").unwrap();
    let a = left.create_node(&mut batch).unwrap();
    let b = right.create_node(&mut batch).unwrap();
    assert_eq!(a, b);
    left.create_edge(&mut batch, a, a, left_type).unwrap();
    batch.commit().unwrap();

    let view = storage.snapshot().unwrap();
    assert_eq!(left.neighbors(&view, a, Direction::Out).unwrap().count(), 1);
    assert_eq!(
        right.neighbors(&view, b, Direction::Out).unwrap().count(),
        0
    );
    assert_eq!(right.edges_with_type(&view, right_type).unwrap().count(), 0);
}

#[test]
fn every_edge_appears_once_in_each_direction() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(4);
    let mut rng = Rng::new(0x5EED_0010);

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let types: Vec<TypeId> = ["A", "B", "C"]
        .iter()
        .map(|name| graph.intern(&mut batch, name).unwrap())
        .collect();
    let nodes: Vec<NodeId> = (0..12)
        .map(|_| graph.create_node(&mut batch).unwrap())
        .collect();
    batch.commit().unwrap();

    let mut live: BTreeMap<u128, (NodeId, NodeId, TypeId)> = BTreeMap::new();
    for _ in 0..8 {
        let mut batch = GraphBatch::begin(&storage).unwrap();
        for _ in 0..20 {
            let delete = rng.chance(30) && !live.is_empty();
            if delete {
                let ids: Vec<u128> = live.keys().copied().collect();
                let id = *rng.pick(&ids).unwrap();
                live.remove(&id);
                graph
                    .delete_edge(&mut batch, pizzlydb_core::EdgeId::from_u128(id).unwrap())
                    .unwrap();
            } else {
                let source = *rng.pick(&nodes).unwrap();
                let target = *rng.pick(&nodes).unwrap();
                let edge_type = *rng.pick(&types).unwrap();
                let id = graph
                    .create_edge(&mut batch, source, target, edge_type)
                    .unwrap();
                live.insert(id.to_u128(), (source, target, edge_type));
            }
        }
        batch.commit().unwrap();

        let view = storage.snapshot().unwrap();
        for &node in &nodes {
            for direction in [Direction::Out, Direction::In] {
                let mut expected: Vec<Neighbor> = live
                    .iter()
                    .filter_map(|(&id, &(source, target, edge_type))| {
                        let (from, to) = match direction {
                            Direction::Out => (source, target),
                            Direction::In => (target, source),
                        };
                        (from == node).then(|| Neighbor {
                            edge: pizzlydb_core::EdgeId::from_u128(id).unwrap(),
                            edge_type,
                            neighbor: to,
                        })
                    })
                    .collect();
                expected.sort_by_key(|n| (n.edge_type, n.neighbor, n.edge));
                let got: Vec<Neighbor> = graph
                    .neighbors(&view, node, direction)
                    .unwrap()
                    .map(Result::unwrap)
                    .collect();
                assert_eq!(got, expected);
            }
        }
        assert_eq!(count_kind(&view, ADJACENCY), live.len() * 2);
        for &edge_type in &types {
            let expected = live.values().filter(|(_, _, t)| *t == edge_type).count();
            assert_eq!(
                graph.edges_with_type(&view, edge_type).unwrap().count(),
                expected
            );
        }
    }
}
