//! Node and edge records: lifecycle, endpoint rules, atomicity, and snapshots.

mod common;

use std::collections::BTreeMap;

use common::{Rng, crash_and_reopen, create, dump, graph};
use pizzlydb_core::{EdgeId, NodeId, TypeId};
use pizzlydb_graph::{Direction, GraphBatch, GraphError, GraphStore, KeyRead};
use pizzlydb_storage::MemoryVfs;

fn node(id: u128) -> NodeId {
    NodeId::from_u128(id).expect("node id")
}

fn edge(id: u128) -> EdgeId {
    EdgeId::from_u128(id).expect("edge id")
}

#[test]
fn never_created_and_deleted_ids_differ() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let knows: TypeId = graph.intern(&mut batch, "KNOWS").unwrap();
    let a = graph.create_node(&mut batch).unwrap();
    let b = graph.create_node(&mut batch).unwrap();
    let ab = graph.create_edge(&mut batch, a, b, knows).unwrap();
    batch.commit().unwrap();

    let mut batch = GraphBatch::begin(&storage).unwrap();
    graph.delete_edge(&mut batch, ab).unwrap();
    graph.delete_node(&mut batch, a).unwrap();
    batch.commit().unwrap();

    let storage = crash_and_reopen(&vfs, storage);
    let view = storage.snapshot().unwrap();
    assert!(matches!(graph.get_node(&view, a), Err(GraphError::Deleted)));
    assert!(matches!(
        graph.get_edge(&view, ab),
        Err(GraphError::Deleted)
    ));
    assert!(graph.get_node(&view, b).is_ok());
    assert!(matches!(
        graph.get_node(&view, node(99)),
        Err(GraphError::NotFound)
    ));
    assert!(matches!(
        graph.get_edge(&view, edge(99)),
        Err(GraphError::NotFound)
    ));
    drop(view);

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let before = dump(&batch);
    assert!(matches!(
        graph.delete_node(&mut batch, a),
        Err(GraphError::Deleted)
    ));
    assert!(matches!(
        graph.delete_edge(&mut batch, ab),
        Err(GraphError::Deleted)
    ));
    assert!(matches!(
        graph.delete_node(&mut batch, node(99)),
        Err(GraphError::NotFound)
    ));
    assert_eq!(dump(&batch), before);
}

#[test]
fn identifiers_are_not_reused_after_commit() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let first = graph.create_node(&mut batch).unwrap();
    graph.delete_node(&mut batch, first).unwrap();
    batch.commit().unwrap();

    let storage = crash_and_reopen(&vfs, storage);
    let mut batch = GraphBatch::begin(&storage).unwrap();
    let second = graph.create_node(&mut batch).unwrap();
    assert!(second > first);
    drop(batch);

    let mut batch = GraphBatch::begin(&storage).unwrap();
    assert_eq!(graph.create_node(&mut batch).unwrap(), second);
}

#[test]
fn edge_to_missing_or_deleted_endpoint_writes_nothing() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let knows: TypeId = graph.intern(&mut batch, "KNOWS").unwrap();
    let live = graph.create_node(&mut batch).unwrap();
    let gone = graph.create_node(&mut batch).unwrap();
    graph.delete_node(&mut batch, gone).unwrap();

    let before = dump(&batch);
    for (source, target) in [
        (live, node(99)),
        (node(99), live),
        (live, gone),
        (gone, gone),
    ] {
        assert!(matches!(
            graph.create_edge(&mut batch, source, target, knows),
            Err(GraphError::EndpointNotLive)
        ));
    }
    let unknown_type = TypeId::from_u128(77).unwrap();
    assert!(matches!(
        graph.create_edge(&mut batch, live, live, unknown_type),
        Err(GraphError::InvalidIdentity)
    ));
    assert_eq!(dump(&batch), before);
    batch.commit().unwrap();

    let view = storage.snapshot().unwrap();
    assert_eq!(
        graph
            .neighbors(&view, live, Direction::Out)
            .unwrap()
            .count(),
        0
    );
    assert!(matches!(
        graph.get_edge(&view, edge(1)),
        Err(GraphError::NotFound)
    ));
}

#[test]
fn node_with_incident_edges_is_not_deleted() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let knows: TypeId = graph.intern(&mut batch, "KNOWS").unwrap();
    let a = graph.create_node(&mut batch).unwrap();
    let b = graph.create_node(&mut batch).unwrap();
    let c = graph.create_node(&mut batch).unwrap();
    let ab = graph.create_edge(&mut batch, a, b, knows).unwrap();
    let loop_c = graph.create_edge(&mut batch, c, c, knows).unwrap();

    let before = dump(&batch);
    for node in [a, b, c] {
        assert!(matches!(
            graph.delete_node(&mut batch, node),
            Err(GraphError::EndpointInUse)
        ));
    }
    assert_eq!(dump(&batch), before);
    batch.commit().unwrap();

    let mut batch = GraphBatch::begin(&storage).unwrap();
    graph.delete_edge(&mut batch, ab).unwrap();
    graph.delete_edge(&mut batch, loop_c).unwrap();
    for node in [a, b, c] {
        graph.delete_node(&mut batch, node).unwrap();
    }
    batch.commit().unwrap();
}

#[test]
fn dropped_batch_and_crash_before_commit_publish_nothing() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let knows: TypeId = graph.intern(&mut batch, "KNOWS").unwrap();
    let a = graph.create_node(&mut batch).unwrap();
    let b = graph.create_node(&mut batch).unwrap();
    graph.create_edge(&mut batch, a, b, knows).unwrap();
    drop(batch);
    assert!(dump(&storage.snapshot().unwrap()).is_empty());

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let knows: TypeId = graph.intern(&mut batch, "KNOWS").unwrap();
    let a = graph.create_node(&mut batch).unwrap();
    let b = graph.create_node(&mut batch).unwrap();
    graph.create_edge(&mut batch, a, b, knows).unwrap();
    drop(batch);
    let storage = crash_and_reopen(&vfs, storage);
    assert!(dump(&storage.snapshot().unwrap()).is_empty());

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let knows: TypeId = graph.intern(&mut batch, "KNOWS").unwrap();
    let a = graph.create_node(&mut batch).unwrap();
    let b = graph.create_node(&mut batch).unwrap();
    let ab = graph.create_edge(&mut batch, a, b, knows).unwrap();
    let staged = dump(&batch);
    batch.commit().unwrap();
    let storage = crash_and_reopen(&vfs, storage);
    let view = storage.snapshot().unwrap();
    assert_eq!(dump(&view), staged);
    assert_eq!(graph.get_edge(&view, ab).unwrap().source, a);
}

#[test]
fn readers_see_whole_batches() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);

    let before = storage.snapshot().unwrap();
    let mut batch = GraphBatch::begin(&storage).unwrap();
    let knows: TypeId = graph.intern(&mut batch, "KNOWS").unwrap();
    let a = graph.create_node(&mut batch).unwrap();
    let b = graph.create_node(&mut batch).unwrap();
    let ab = graph.create_edge(&mut batch, a, b, knows).unwrap();
    let during = storage.snapshot().unwrap();
    assert!(matches!(
        graph.get_node(&during, a),
        Err(GraphError::NotFound)
    ));
    batch.commit().unwrap();
    let after = storage.snapshot().unwrap();

    for view in [&before, &during] {
        assert!(dump(view).is_empty());
        assert!(matches!(
            graph.get_edge(view, ab),
            Err(GraphError::NotFound)
        ));
    }
    assert!(graph.get_node(&after, a).is_ok());
    assert!(graph.get_node(&after, b).is_ok());
    assert!(graph.get_edge(&after, ab).is_ok());
}

#[test]
fn two_graph_transactions_can_begin() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let first = GraphBatch::begin(&storage).unwrap();
    let second = GraphBatch::begin(&storage);
    assert!(second.is_ok());
    drop(first);
    drop(second);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Ok,
    NotFound,
    Deleted,
    EndpointNotLive,
    EndpointInUse,
}

fn outcome<T>(result: &Result<T, GraphError>) -> Outcome {
    match result {
        Ok(_) => Outcome::Ok,
        Err(GraphError::NotFound) => Outcome::NotFound,
        Err(GraphError::Deleted) => Outcome::Deleted,
        Err(GraphError::EndpointNotLive) => Outcome::EndpointNotLive,
        Err(GraphError::EndpointInUse) => Outcome::EndpointInUse,
        Err(err) => panic!("unexpected error: {err:?}"),
    }
}

/// Reference state: `None` is a tombstone.
#[derive(Clone, Default)]
struct Model {
    nodes: BTreeMap<u128, bool>,
    edges: BTreeMap<u128, Option<(u128, u128)>>,
}

impl Model {
    fn node_state(&self, id: u128) -> Outcome {
        match self.nodes.get(&id) {
            None => Outcome::NotFound,
            Some(false) => Outcome::Deleted,
            Some(true) => Outcome::Ok,
        }
    }

    fn edge_state(&self, id: u128) -> Outcome {
        match self.edges.get(&id) {
            None => Outcome::NotFound,
            Some(None) => Outcome::Deleted,
            Some(Some(_)) => Outcome::Ok,
        }
    }

    fn live_edges(&self) -> impl Iterator<Item = (u128, u128, u128)> + '_ {
        self.edges
            .iter()
            .filter_map(|(&id, state)| state.map(|(source, target)| (id, source, target)))
    }

    fn next_node(&self) -> u128 {
        self.nodes.len() as u128 + 1
    }

    fn next_edge(&self) -> u128 {
        self.edges.len() as u128 + 1
    }
}

fn assert_matches_model(graph: &GraphStore, read: &impl KeyRead, model: &Model) {
    for id in 1..=model.next_node() {
        assert_eq!(
            outcome(&graph.get_node(read, node(id))),
            model.node_state(id),
            "node {id}"
        );
    }
    for id in 1..=model.next_edge() {
        let got = graph.get_edge(read, edge(id));
        assert_eq!(outcome(&got), model.edge_state(id), "edge {id}");
        if let (Ok(record), Some(Some((source, target)))) = (got, model.edges.get(&id)) {
            assert_eq!(
                (record.source, record.target),
                (node(*source), node(*target))
            );
        }
    }
    for (&id, &live) in &model.nodes {
        for direction in [Direction::Out, Direction::In] {
            let mut expected: Vec<(u128, u128)> = model
                .live_edges()
                .filter_map(|(edge_id, source, target)| match direction {
                    Direction::Out if live && source == id => Some((target, edge_id)),
                    Direction::In if live && target == id => Some((source, edge_id)),
                    _ => None,
                })
                .collect();
            expected.sort_unstable();
            let got: Vec<(u128, u128)> = graph
                .neighbors(read, node(id), direction)
                .unwrap()
                .map(|entry| {
                    let entry = entry.unwrap();
                    (entry.neighbor.to_u128(), entry.edge.to_u128())
                })
                .collect();
            assert_eq!(got, expected, "node {id} {direction:?}");
        }
    }
}

#[test]
fn random_histories_match_a_reference_model() {
    let vfs = MemoryVfs::new();
    let mut storage = create(&vfs);
    let graph = graph(3);
    let mut rng = Rng::new(0x5EED_0009);

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let knows: TypeId = graph.intern(&mut batch, "KNOWS").unwrap();
    batch.commit().unwrap();

    let mut committed = Model::default();
    let mut rejected = 0_usize;
    for round in 0..60 {
        let mut model = committed.clone();
        let mut batch = GraphBatch::begin(&storage).unwrap();
        for _ in 0..rng.below(12) + 1 {
            let before = dump(&batch);
            let failed = match rng.below(4) {
                0 => {
                    let id = graph.create_node(&mut batch).unwrap();
                    assert_eq!(id.to_u128(), model.next_node());
                    model.nodes.insert(id.to_u128(), true);
                    false
                }
                1 => {
                    let id = rng.below(model.next_node() as usize) as u128 + 1;
                    let in_use = model
                        .live_edges()
                        .any(|(_, source, target)| source == id || target == id);
                    let expected = match model.node_state(id) {
                        Outcome::Ok if in_use => Outcome::EndpointInUse,
                        state => state,
                    };
                    let got = outcome(&graph.delete_node(&mut batch, node(id)));
                    assert_eq!(got, expected, "delete node {id}");
                    if got == Outcome::Ok {
                        model.nodes.insert(id, false);
                    }
                    got != Outcome::Ok
                }
                2 => {
                    let bound = model.next_node() as usize;
                    let source = rng.below(bound) as u128 + 1;
                    let target = rng.below(bound) as u128 + 1;
                    let live = |id| model.node_state(id) == Outcome::Ok;
                    let expected = if live(source) && live(target) {
                        Outcome::Ok
                    } else {
                        Outcome::EndpointNotLive
                    };
                    let result = graph.create_edge(&mut batch, node(source), node(target), knows);
                    assert_eq!(outcome(&result), expected, "edge {source}->{target}");
                    if let Ok(id) = result {
                        assert_eq!(id.to_u128(), model.next_edge());
                        model.edges.insert(id.to_u128(), Some((source, target)));
                    }
                    expected != Outcome::Ok
                }
                _ => {
                    let id = rng.below(model.next_edge() as usize) as u128 + 1;
                    let expected = model.edge_state(id);
                    let got = outcome(&graph.delete_edge(&mut batch, edge(id)));
                    assert_eq!(got, expected, "delete edge {id}");
                    if got == Outcome::Ok {
                        model.edges.insert(id, None);
                    }
                    got != Outcome::Ok
                }
            };
            if failed {
                assert_eq!(dump(&batch), before, "failed call staged keys");
                rejected += 1;
            }
        }
        assert_matches_model(&graph, &batch, &model);

        if rng.chance(70) {
            batch.commit().unwrap();
            committed = model;
        } else {
            drop(batch);
        }
        if round % 20 == 19 {
            storage = crash_and_reopen(&vfs, storage);
        }
        assert_matches_model(&graph, &storage.snapshot().unwrap(), &committed);
    }
    assert!(rejected > 0, "history never hit an endpoint rule");
}
