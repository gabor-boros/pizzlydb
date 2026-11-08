//! Schema, namespace, detach, index build, and statistics.

mod common;

use std::sync::{Arc, Barrier};
use std::thread;

use common::{create, graph};
use pizzlydb_core::{LabelId, PropertyKeyId, TypeId, Value, ValueType};
use pizzlydb_graph::{
    ConstraintKind, DetachLimits, Direction, GraphBatch, GraphError, IndexPhase, NamespaceOptions,
    NamespaceRegistry, NamespaceState, PropertyDeclaration, Scope, verify_full, verify_registered,
};
use pizzlydb_storage::MemoryVfs;
use pizzlydb_testkit::power_law_graph;

fn declaration(value_type: ValueType, nullable: bool, required: bool) -> PropertyDeclaration {
    PropertyDeclaration {
        value_type,
        nullable,
        required,
    }
}

#[test]
fn required_properties_are_checked_at_commit() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);

    let mut txn = GraphBatch::begin(&storage).unwrap();
    let person: LabelId = graph.intern(&mut txn, "Person").unwrap();
    let name: PropertyKeyId = graph.intern(&mut txn, "name").unwrap();
    graph
        .declare(
            &mut txn,
            Scope::Label(person),
            name,
            declaration(ValueType::String, false, true),
        )
        .unwrap();
    txn.commit().unwrap();

    let mut txn = GraphBatch::begin(&storage).unwrap();
    let node = graph.create_node(&mut txn).unwrap();
    graph.add_label(&mut txn, node, person).unwrap();
    assert!(matches!(
        txn.commit().unwrap_err(),
        GraphError::ConstraintViolation {
            kind: ConstraintKind::RequiredMissing,
            ..
        }
    ));

    let mut txn = GraphBatch::begin(&storage).unwrap();
    let node = graph.create_node(&mut txn).unwrap();
    graph.add_label(&mut txn, node, person).unwrap();
    graph
        .set_property(&mut txn, node, name, &Value::from("Ada"))
        .unwrap();
    txn.commit().unwrap();
    let view = storage.snapshot().unwrap();
    assert_eq!(
        graph.get_property(&view, node, name).unwrap(),
        Some(Value::from("Ada"))
    );
}

#[test]
fn adding_a_label_rechecks_the_node_at_commit() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);
    let mut txn = GraphBatch::begin(&storage).unwrap();
    let person: LabelId = graph.intern(&mut txn, "Person").unwrap();
    let age: PropertyKeyId = graph.intern(&mut txn, "age").unwrap();
    graph
        .declare(
            &mut txn,
            Scope::Label(person),
            age,
            declaration(ValueType::Int, false, true),
        )
        .unwrap();
    let node = graph.create_node(&mut txn).unwrap();
    graph
        .set_property(&mut txn, node, age, &Value::from("nope"))
        .unwrap();
    graph.add_label(&mut txn, node, person).unwrap();
    assert!(matches!(
        txn.commit().unwrap_err(),
        GraphError::ConstraintViolation {
            kind: ConstraintKind::TypeMismatch,
            ..
        }
    ));
}

#[test]
fn concurrent_tightening_and_a_violating_write_admit_one() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);
    let mut txn = GraphBatch::begin(&storage).unwrap();
    let person: LabelId = graph.intern(&mut txn, "Person").unwrap();
    let nick: PropertyKeyId = graph.intern(&mut txn, "nick").unwrap();
    graph
        .declare(
            &mut txn,
            Scope::Label(person),
            nick,
            declaration(ValueType::String, true, false),
        )
        .unwrap();
    let node = graph.create_node(&mut txn).unwrap();
    graph.add_label(&mut txn, node, person).unwrap();
    txn.commit().unwrap();

    let mut writer = GraphBatch::begin(&storage).unwrap();
    graph
        .set_property(&mut writer, node, nick, &Value::Null)
        .unwrap();
    writer.commit().unwrap();
    let mut tighten = GraphBatch::begin(&storage).unwrap();
    assert!(matches!(
        graph.declare(
            &mut tighten,
            Scope::Label(person),
            nick,
            declaration(ValueType::String, false, false),
        ),
        Err(GraphError::ConstraintViolation { .. })
    ));

    let mut clear = GraphBatch::begin(&storage).unwrap();
    graph.clear_property(&mut clear, node, nick).unwrap();
    clear.commit().unwrap();

    let mut tighten = GraphBatch::begin(&storage).unwrap();
    graph
        .declare(
            &mut tighten,
            Scope::Label(person),
            nick,
            declaration(ValueType::String, false, false),
        )
        .unwrap();
    let mut writer = GraphBatch::begin(&storage).unwrap();
    graph
        .set_property(&mut writer, node, nick, &Value::Null)
        .unwrap();
    tighten.commit().unwrap();
    assert!(matches!(
        writer.commit().unwrap_err(),
        GraphError::Transaction(_)
    ));
}

#[test]
fn detach_delete_removes_every_incident_edge() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);
    let mut txn = GraphBatch::begin(&storage).unwrap();
    let knows: TypeId = graph.intern(&mut txn, "KNOWS").unwrap();
    let a = graph.create_node(&mut txn).unwrap();
    let b = graph.create_node(&mut txn).unwrap();
    let out = graph.create_edge(&mut txn, a, b, knows).unwrap();
    let parallel = graph.create_edge(&mut txn, a, b, knows).unwrap();
    let incoming = graph.create_edge(&mut txn, b, a, knows).unwrap();
    let looped = graph.create_edge(&mut txn, a, a, knows).unwrap();
    txn.commit().unwrap();

    let mut txn = GraphBatch::begin(&storage).unwrap();
    let removed = graph
        .detach_delete_node(&mut txn, a, DetachLimits { max_edges: 1 })
        .unwrap_err();
    assert!(matches!(
        removed,
        GraphError::TooManyIncidentEdges { limit: 1, .. }
    ));
    assert!(graph.get_node(&txn, a).is_ok());

    let removed = graph
        .detach_delete_node(&mut txn, a, DetachLimits { max_edges: 8 })
        .unwrap();
    assert_eq!(removed, 4);
    let deletes = txn
        .mutations()
        .iter()
        .filter(|mutation| {
            matches!(
                mutation,
                pizzlydb_core::GraphMutation::EdgeDelete { .. }
                    | pizzlydb_core::GraphMutation::NodeDelete { .. }
            )
        })
        .count();
    assert_eq!(deletes, 5);
    txn.commit().unwrap();
    let view = storage.snapshot().unwrap();
    verify_full(&graph, &view).unwrap();
    assert!(matches!(graph.get_node(&view, a), Err(GraphError::Deleted)));
    assert!(graph.get_node(&view, b).is_ok());
    for edge in [out, parallel, incoming, looped] {
        assert!(matches!(
            graph.get_edge(&view, edge),
            Err(GraphError::Deleted)
        ));
    }
    assert_eq!(
        graph.neighbors(&view, b, Direction::Out).unwrap().count(),
        0
    );
}

#[test]
fn schema_version_increments_once_per_transaction() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);
    let mut txn = GraphBatch::begin(&storage).unwrap();
    let person: LabelId = graph.intern(&mut txn, "Person").unwrap();
    let other: LabelId = graph.intern(&mut txn, "Other").unwrap();
    let age: PropertyKeyId = graph.intern(&mut txn, "age").unwrap();
    graph
        .declare(
            &mut txn,
            Scope::Label(person),
            age,
            declaration(ValueType::Int, true, false),
        )
        .unwrap();
    graph
        .declare(
            &mut txn,
            Scope::Label(other),
            age,
            declaration(ValueType::Int, false, false),
        )
        .unwrap();
    let version_records = txn
        .mutations()
        .iter()
        .filter(|mutation| matches!(mutation, pizzlydb_core::GraphMutation::SchemaChanged { .. }))
        .count();
    assert!(version_records >= 2);
    txn.commit().unwrap();
    let view = storage.snapshot().unwrap();
    assert_eq!(graph.schema_version(&view).unwrap(), 1);

    let mut txn = GraphBatch::begin(&storage).unwrap();
    let _: LabelId = graph.intern(&mut txn, "Person").unwrap();
    txn.commit().unwrap();
    assert_eq!(
        graph.schema_version(&storage.snapshot().unwrap()).unwrap(),
        1
    );
}

#[test]
fn online_index_build_and_drop_are_resumable() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);
    let mut txn = GraphBatch::begin(&storage).unwrap();
    let person: LabelId = graph.intern(&mut txn, "Person").unwrap();
    let city: PropertyKeyId = graph.intern(&mut txn, "city").unwrap();
    let mut nodes = Vec::new();
    for name in ["a", "b", "c", "a"] {
        let node = graph.create_node(&mut txn).unwrap();
        graph.add_label(&mut txn, node, person).unwrap();
        graph
            .set_property(&mut txn, node, city, &Value::from(name))
            .unwrap();
        nodes.push(node);
    }
    txn.commit().unwrap();

    let mut txn = GraphBatch::begin(&storage).unwrap();
    let index = graph
        .start_index(&mut txn, Scope::Label(person), city, true)
        .unwrap();
    assert!(matches!(
        graph.backfill_index(&mut txn, index, 10).unwrap_err(),
        GraphError::DuplicateValues { .. }
    ));
    txn.commit().unwrap();

    let mut txn = GraphBatch::begin(&storage).unwrap();
    graph.clear_property(&mut txn, nodes[3], city).unwrap();
    graph
        .set_property(&mut txn, nodes[3], city, &Value::from("d"))
        .unwrap();
    txn.commit().unwrap();

    let mut txn = GraphBatch::begin(&storage).unwrap();
    let index = graph
        .start_index(&mut txn, Scope::Label(person), city, false)
        .unwrap();
    assert!(!graph.backfill_index(&mut txn, index, 2).unwrap());
    txn.commit().unwrap();
    let storage = common::crash_and_reopen(&vfs, storage);
    let mut txn = GraphBatch::begin(&storage).unwrap();
    assert!(graph.backfill_index(&mut txn, index, 10).unwrap());
    graph.finish_index(&mut txn, index).unwrap();
    txn.commit().unwrap();
    let view = storage.snapshot().unwrap();
    assert_eq!(
        graph.index_progress(&view, index).unwrap().phase,
        IndexPhase::Ready
    );
    verify_full(&graph, &view).unwrap();

    let mut txn = GraphBatch::begin(&storage).unwrap();
    graph.drop_index(&mut txn, index).unwrap();
    txn.commit().unwrap();
    let view = storage.snapshot().unwrap();
    assert!(matches!(
        graph.index_lookup(&view, index, &Value::from("a")),
        Err(GraphError::NotFound)
    ));
    let mut txn = GraphBatch::begin(&storage).unwrap();
    assert!(!graph.reclaim_index(&mut txn, index, 1).unwrap());
    txn.commit().unwrap();
    let mut txn = GraphBatch::begin(&storage).unwrap();
    assert!(graph.reclaim_index(&mut txn, index, 100).unwrap());
    txn.commit().unwrap();
    verify_full(&graph, &storage.snapshot().unwrap()).unwrap();
}

#[test]
fn namespace_drop_is_chunked_and_isolated() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let mut txn = GraphBatch::begin(&storage).unwrap();
    let left = NamespaceRegistry::create(&mut txn, "left", NamespaceOptions::default()).unwrap();
    let right = NamespaceRegistry::create(&mut txn, "right", NamespaceOptions::default()).unwrap();
    txn.commit().unwrap();

    let mut txn = GraphBatch::begin(&storage).unwrap();
    assert!(matches!(
        GraphStoreOpen::open(&txn, pizzlydb_core::NamespaceId::from_u128(99).unwrap()),
        Err(GraphError::NamespaceNotFound)
    ));
    let left_graph = pizzlydb_graph::GraphStore::open(&txn, left.id).unwrap();
    let right_graph = pizzlydb_graph::GraphStore::open(&txn, right.id).unwrap();
    let kept = right_graph.create_node(&mut txn).unwrap();
    for _ in 0..5 {
        left_graph.create_node(&mut txn).unwrap();
    }
    txn.commit().unwrap();

    let mut txn = GraphBatch::begin(&storage).unwrap();
    NamespaceRegistry::drop_namespace(&mut txn, left.id).unwrap();
    txn.commit().unwrap();
    let mut txn = GraphBatch::begin(&storage).unwrap();
    assert!(matches!(
        pizzlydb_graph::GraphStore::open(&txn, left.id),
        Err(GraphError::NamespaceDropping)
    ));
    let mut done = false;
    for _ in 0..8 {
        done = NamespaceRegistry::reclaim(&mut txn, left.id, 2).unwrap();
        if done {
            break;
        }
        txn.commit().unwrap();
        txn = GraphBatch::begin(&storage).unwrap();
    }
    assert!(done);
    txn.commit().unwrap();
    let view = storage.snapshot().unwrap();
    assert!(matches!(
        NamespaceRegistry::get(&view, left.id),
        Err(GraphError::NamespaceNotFound)
    ));
    assert_eq!(
        NamespaceRegistry::get(&view, right.id).unwrap().state,
        NamespaceState::Active
    );
    assert!(right_graph.get_node(&view, kept).is_ok());
    verify_registered(&view).unwrap();
}

struct GraphStoreOpen;

impl GraphStoreOpen {
    fn open<R: pizzlydb_graph::KeyRead>(
        read: &R,
        id: pizzlydb_core::NamespaceId,
    ) -> Result<pizzlydb_graph::GraphStore, GraphError> {
        pizzlydb_graph::GraphStore::open(read, id)
    }
}

#[test]
fn exact_counters_survive_concurrent_writers_and_a_crash() {
    let vfs = MemoryVfs::new();
    let storage = Arc::new(create(&vfs));
    let graph = graph(1);
    let mut txn = GraphBatch::begin(&storage).unwrap();
    let knows: TypeId = graph.intern(&mut txn, "KNOWS").unwrap();
    txn.commit().unwrap();

    let barrier = Arc::new(Barrier::new(2));
    let mut threads = Vec::new();
    for _ in 0..2 {
        let storage = Arc::clone(&storage);
        let barrier = Arc::clone(&barrier);
        threads.push(thread::spawn(move || {
            let graph = common::graph(1);
            let knows = {
                let txn = GraphBatch::begin(&storage).unwrap();
                let knows: TypeId = graph.lookup(&txn, "KNOWS").unwrap().unwrap();
                knows
            };
            barrier.wait();
            for _ in 0..4 {
                let mut txn = GraphBatch::begin(&storage).unwrap();
                let a = graph.create_node(&mut txn).unwrap();
                let b = graph.create_node(&mut txn).unwrap();
                graph.create_edge(&mut txn, a, b, knows).unwrap();
                txn.commit().unwrap();
            }
        }));
    }
    for thread in threads {
        thread.join().unwrap();
    }
    let storage = Arc::try_unwrap(storage).unwrap_or_else(|arc| {
        let vfs = MemoryVfs::new();
        drop(arc);
        let _ = vfs;
        panic!("storage still shared");
    });
    let storage = common::crash_and_reopen(&vfs, storage);
    let view = storage.snapshot().unwrap();
    let stats = graph.stats();
    assert_eq!(stats.node_count(&view).unwrap().value, 16);
    assert_eq!(stats.edge_count(&view).unwrap().value, 8);
    assert_eq!(stats.type_count(&view, knows).unwrap().value, 8);
    assert!(stats.node_count(&view).unwrap().exact);
    verify_full(&graph, &view).unwrap();
}

#[test]
fn degree_samples_match_a_full_scan_of_a_power_law_graph() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);
    let data = power_law_graph(7, 12, 40);
    let mut txn = GraphBatch::begin(&storage).unwrap();
    let knows: TypeId = graph.intern(&mut txn, "KNOWS").unwrap();
    let mut nodes = Vec::new();
    for _ in &data.nodes {
        nodes.push(graph.create_node(&mut txn).unwrap());
    }
    for (source, target) in &data.edges {
        let from = nodes[usize::try_from(source - 1).unwrap()];
        let to = nodes[usize::try_from(target - 1).unwrap()];
        graph.create_edge(&mut txn, from, to, knows).unwrap();
    }
    txn.commit().unwrap();
    let mut txn = GraphBatch::begin(&storage).unwrap();
    graph
        .stats()
        .refresh_degrees(&mut txn, knows, Direction::Out, 100, 1_000)
        .unwrap();
    txn.commit().unwrap();
    let view = storage.snapshot().unwrap();
    let estimate = graph
        .stats()
        .degree_estimate(&view, knows, Direction::Out, 0.5)
        .unwrap();
    assert!(estimate.exact);
    let mut degrees = vec![0_u64; nodes.len()];
    for (source, _) in &data.edges {
        degrees[usize::try_from(source - 1).unwrap()] += 1;
    }
    degrees.sort_unstable();
    let mid = degrees[degrees.len() / 2];
    let reported = estimate.value;
    assert!(
        reported <= mid.max(1) * 2 && mid <= reported.max(1) * 2,
        "sample {reported} vs median {mid}"
    );
}

#[test]
fn mixed_numeric_range_uses_the_shared_order() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);
    let mut txn = GraphBatch::begin(&storage).unwrap();
    let person: LabelId = graph.intern(&mut txn, "Person").unwrap();
    let score: PropertyKeyId = graph.intern(&mut txn, "score").unwrap();
    let index = graph
        .create_index(&mut txn, Scope::Label(person), score, false)
        .unwrap();
    let values = [
        Value::Int(1),
        Value::Float(1.5),
        Value::Decimal(pizzlydb_core::Decimal::from_str_exact("1.50").unwrap()),
        Value::Int(2),
        Value::Float(3.0),
    ];
    for value in &values {
        let node = graph.create_node(&mut txn).unwrap();
        graph.add_label(&mut txn, node, person).unwrap();
        graph.set_property(&mut txn, node, score, value).unwrap();
    }
    txn.commit().unwrap();
    let view = storage.snapshot().unwrap();
    let hits = graph
        .index_range(
            &view,
            index,
            std::ops::Bound::Excluded(&Value::Float(1.5)),
            std::ops::Bound::Included(&Value::Int(3)),
        )
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(hits.len(), 2);
}
