//! Property cells, declarations, value sizes, and traversal isolation.

mod common;

use common::{
    ADJACENCY, DECLARED_CELL, KIND_AT, Tracing, UNDECLARED_CELL, count_kind, crash_and_reopen,
    create, dump, graph, undeclared_node_cell,
};
use pizzlydb_core::{LabelId, PropertyKeyId, TypeId, Value, ValueType};
use pizzlydb_graph::{Direction, GraphBatch, GraphError, PropertyDeclaration, Scope};
use pizzlydb_storage::{MAX_VALUE_LEN, MemoryVfs};

fn declaration(value_type: ValueType, nullable: bool) -> PropertyDeclaration {
    PropertyDeclaration {
        value_type,
        nullable,
        required: false,
    }
}

#[test]
fn absence_and_null_are_different() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let key: PropertyKeyId = graph.intern(&mut batch, "nickname").unwrap();
    let absent = graph.create_node(&mut batch).unwrap();
    let null = graph.create_node(&mut batch).unwrap();
    graph
        .set_property(&mut batch, null, key, &Value::Null)
        .unwrap();
    batch.commit().unwrap();

    let storage = crash_and_reopen(&vfs, storage);
    let view = storage.snapshot().unwrap();
    assert_eq!(graph.get_property(&view, absent, key).unwrap(), None);
    assert_eq!(
        graph.get_property(&view, null, key).unwrap(),
        Some(Value::Null)
    );
}

#[test]
fn scalars_round_trip_across_reopen() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);
    let values = [
        Value::Null,
        Value::Bool(false),
        Value::Bool(true),
        Value::Int(i64::MIN),
        Value::Int(-1),
        Value::Int(0),
        Value::Int(i64::MAX),
        Value::Float(f64::NEG_INFINITY),
        Value::Float(-1.5),
        Value::Float(f64::MIN_POSITIVE),
        Value::Float(f64::MAX),
        Value::Float(f64::INFINITY),
        Value::from(""),
        Value::from("caf\u{e9} \0 nul"),
        Value::from(&[][..]),
        Value::from(&[0_u8, 0, 0xFF, 1][..]),
    ];

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let knows: TypeId = graph.intern(&mut batch, "KNOWS").unwrap();
    let node = graph.create_node(&mut batch).unwrap();
    let edge = graph.create_edge(&mut batch, node, node, knows).unwrap();
    let mut keys = Vec::new();
    for (index, value) in values.iter().enumerate() {
        let key: PropertyKeyId = graph.intern(&mut batch, format!("p{index}")).unwrap();
        graph.set_property(&mut batch, node, key, value).unwrap();
        graph.set_property(&mut batch, edge, key, value).unwrap();
        keys.push(key);
    }
    let zero: PropertyKeyId = graph.intern(&mut batch, "zero").unwrap();
    graph
        .set_property(&mut batch, node, zero, &Value::Float(-0.0))
        .unwrap();
    assert!(matches!(
        graph.set_property(&mut batch, node, zero, &Value::Float(f64::NAN)),
        Err(GraphError::InvalidValue)
    ));
    batch.commit().unwrap();

    let storage = crash_and_reopen(&vfs, storage);
    let view = storage.snapshot().unwrap();
    for (key, value) in keys.iter().zip(&values) {
        assert_eq!(
            graph.get_property(&view, node, *key).unwrap().as_ref(),
            Some(value)
        );
        assert_eq!(
            graph.get_property(&view, edge, *key).unwrap().as_ref(),
            Some(value)
        );
    }
    match graph.get_property(&view, node, zero).unwrap() {
        Some(Value::Float(stored)) => {
            assert_eq!(stored, 0.0);
            assert!(stored.is_sign_positive());
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn declaration_changes_follow_the_conflict_rules() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let person: LabelId = graph.intern(&mut batch, "Person").unwrap();
    let age: PropertyKeyId = graph.intern(&mut batch, "age").unwrap();
    let scope = Scope::Label(person);
    graph
        .declare(&mut batch, scope, age, declaration(ValueType::Int, false))
        .unwrap();
    batch.commit().unwrap();

    let mut batch = GraphBatch::begin(&storage).unwrap();
    graph
        .declare(&mut batch, scope, age, declaration(ValueType::Int, false))
        .unwrap();
    assert!(batch.is_empty());
    assert!(matches!(
        graph.declare(
            &mut batch,
            Scope::Label(LabelId::from_u128(99).unwrap()),
            age,
            declaration(ValueType::Int, false)
        ),
        Err(GraphError::InvalidIdentity)
    ));
    assert!(batch.is_empty());

    graph
        .declare(
            &mut batch,
            scope,
            age,
            declaration(ValueType::String, false),
        )
        .unwrap();
    graph
        .declare(&mut batch, scope, age, declaration(ValueType::Int, true))
        .unwrap();
    batch.commit().unwrap();

    let view = storage.snapshot().unwrap();
    assert_eq!(
        graph.declaration(&view, scope, age).unwrap(),
        Some(declaration(ValueType::Int, true))
    );

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let node = graph.create_node(&mut batch).unwrap();
    graph.add_label(&mut batch, node, person).unwrap();
    graph
        .set_property(&mut batch, node, age, &Value::Null)
        .unwrap();
    assert!(matches!(
        graph.declare(&mut batch, scope, age, declaration(ValueType::Int, false)),
        Err(GraphError::ConstraintViolation { .. })
    ));
}

#[test]
fn a_node_must_satisfy_every_label_declaration() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let first: LabelId = graph.intern(&mut batch, "First").unwrap();
    let second: LabelId = graph.intern(&mut batch, "Second").unwrap();
    let key: PropertyKeyId = graph.intern(&mut batch, "code").unwrap();
    graph
        .declare(
            &mut batch,
            Scope::Label(first),
            key,
            declaration(ValueType::Int, false),
        )
        .unwrap();
    assert!(matches!(
        graph.declare(
            &mut batch,
            Scope::Label(second),
            key,
            declaration(ValueType::String, true),
        ),
        Err(GraphError::ConstraintViolation { .. })
    ));
    graph
        .declare(
            &mut batch,
            Scope::Label(second),
            key,
            declaration(ValueType::Int, true),
        )
        .unwrap();
    let node = graph.create_node(&mut batch).unwrap();
    graph
        .set_labels(&mut batch, node, &[second, first])
        .unwrap();
    assert!(matches!(
        graph.set_property(&mut batch, node, key, &Value::Null),
        Err(GraphError::ConstraintViolation { .. })
    ));
    graph
        .set_property(&mut batch, node, key, &Value::Int(7))
        .unwrap();
    assert_eq!(count_kind(&batch, DECLARED_CELL), 1);
    assert_eq!(count_kind(&batch, UNDECLARED_CELL), 0);
}

#[test]
fn clearing_removes_both_cells() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let person: LabelId = graph.intern(&mut batch, "Person").unwrap();
    let key: PropertyKeyId = graph.intern(&mut batch, "name").unwrap();
    let node = graph.create_node(&mut batch).unwrap();
    graph
        .set_property(&mut batch, node, key, &Value::from("old"))
        .unwrap();
    graph
        .declare(
            &mut batch,
            Scope::Label(person),
            key,
            declaration(ValueType::String, false),
        )
        .unwrap();
    graph.add_label(&mut batch, node, person).unwrap();
    assert_eq!(
        graph.get_property(&batch, node, key).unwrap(),
        Some(Value::from("old"))
    );
    assert_eq!(count_kind(&batch, DECLARED_CELL), 1);
    assert_eq!(count_kind(&batch, UNDECLARED_CELL), 0);

    graph
        .set_property(&mut batch, node, key, &Value::from("new"))
        .unwrap();
    assert_eq!(count_kind(&batch, UNDECLARED_CELL), 0);
    assert_eq!(count_kind(&batch, DECLARED_CELL), 1);
    graph.remove_label(&mut batch, node, person).unwrap();
    assert_eq!(
        graph.get_property(&batch, node, key).unwrap(),
        Some(Value::from("new"))
    );

    graph.clear_property(&mut batch, node, key).unwrap();
    assert_eq!(graph.get_property(&batch, node, key).unwrap(), None);
    let staged = dump(&batch);
    graph.clear_property(&mut batch, node, key).unwrap();
    assert_eq!(dump(&batch), staged);
    batch.commit().unwrap();

    let view = storage.snapshot().unwrap();
    assert_eq!(count_kind(&view, DECLARED_CELL), 0);
    assert_eq!(count_kind(&view, UNDECLARED_CELL), 0);
}

#[test]
fn edge_properties_follow_the_type_declaration_and_die_with_the_edge() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let knows: TypeId = graph.intern(&mut batch, "KNOWS").unwrap();
    let since: PropertyKeyId = graph.intern(&mut batch, "since").unwrap();
    let note: PropertyKeyId = graph.intern(&mut batch, "note").unwrap();
    graph
        .declare(
            &mut batch,
            Scope::Type(knows),
            since,
            declaration(ValueType::Int, false),
        )
        .unwrap();
    let a = graph.create_node(&mut batch).unwrap();
    let edge = graph.create_edge(&mut batch, a, a, knows).unwrap();
    assert!(matches!(
        graph.set_property(&mut batch, edge, since, &Value::from("1999")),
        Err(GraphError::ConstraintViolation { .. })
    ));
    graph
        .set_property(&mut batch, edge, since, &Value::Int(1999))
        .unwrap();
    graph
        .set_property(&mut batch, edge, note, &Value::from("x"))
        .unwrap();
    graph
        .set_property(&mut batch, a, note, &Value::from("y"))
        .unwrap();
    batch.commit().unwrap();

    let mut batch = GraphBatch::begin(&storage).unwrap();
    graph.delete_edge(&mut batch, edge).unwrap();
    assert!(matches!(
        graph.set_property(&mut batch, edge, note, &Value::Int(1)),
        Err(GraphError::Deleted)
    ));
    assert!(matches!(
        graph.clear_property(&mut batch, edge, note),
        Err(GraphError::Deleted)
    ));
    graph.delete_node(&mut batch, a).unwrap();
    batch.commit().unwrap();

    let view = storage.snapshot().unwrap();
    assert_eq!(count_kind(&view, DECLARED_CELL), 0);
    assert_eq!(count_kind(&view, UNDECLARED_CELL), 0);
    assert_eq!(graph.get_property(&view, edge, since).unwrap(), None);
}

#[test]
fn large_values_overflow_and_oversized_values_are_rejected() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let key: PropertyKeyId = graph.intern(&mut batch, "blob").unwrap();
    let node = graph.create_node(&mut batch).unwrap();
    let large = Value::Bytes((0..100_000_u32).map(|i| (i % 251) as u8).collect());
    graph.set_property(&mut batch, node, key, &large).unwrap();
    batch.commit().unwrap();

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let oversized = Value::Bytes(vec![1; MAX_VALUE_LEN]);
    assert!(matches!(
        graph.set_property(&mut batch, node, key, &oversized),
        Err(GraphError::ValueTooLarge)
    ));
    assert!(batch.is_empty());
    drop(batch);

    let storage = crash_and_reopen(&vfs, storage);
    let view = storage.snapshot().unwrap();
    assert_eq!(graph.get_property(&view, node, key).unwrap(), Some(large));
}

#[test]
fn malformed_cells_are_corrupt() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let key: PropertyKeyId = graph.intern(&mut batch, "p").unwrap();
    let node = graph.create_node(&mut batch).unwrap();
    batch.commit().unwrap();

    let cell = undeclared_node_cell(&graph, node.as_bytes(), key);
    let malformed: [&[u8]; 6] = [
        &[],
        &[0x00],
        &[0x07],
        &[0x03, 1, 2],
        &[0x05, 0xFF, 0xFF, 0xFF, 0xFF, b'a'],
        &[0x06, 0x00, 0x00, 0x00, 0x02, 1, 2, 3],
    ];
    for bytes in malformed {
        let mut raw = storage.begin_write().unwrap();
        raw.put(&cell, bytes).unwrap();
        storage.atomic_write_sync(raw).unwrap();
        let view = storage.snapshot().unwrap();
        assert!(
            matches!(
                graph.get_property(&view, node, key),
                Err(GraphError::Corrupt)
            ),
            "{bytes:?}"
        );
    }
}

#[test]
fn neighbor_scans_read_no_property_keys() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let knows: TypeId = graph.intern(&mut batch, "KNOWS").unwrap();
    let person: LabelId = graph.intern(&mut batch, "Person").unwrap();
    let name: PropertyKeyId = graph.intern(&mut batch, "name").unwrap();
    let nodes: Vec<_> = (0..4)
        .map(|_| graph.create_node(&mut batch).unwrap())
        .collect();
    for &node in &nodes {
        graph.add_label(&mut batch, node, person).unwrap();
        graph
            .set_property(&mut batch, node, name, &Value::from("n"))
            .unwrap();
    }
    let hub = nodes[0];
    for &target in &nodes[1..] {
        let edge = graph.create_edge(&mut batch, hub, target, knows).unwrap();
        graph
            .set_property(&mut batch, edge, name, &Value::from("e"))
            .unwrap();
    }
    batch.commit().unwrap();

    let traced = Tracing::new(storage.snapshot().unwrap());
    let found = graph
        .neighbors(&traced, hub, Direction::Out)
        .unwrap()
        .map(Result::unwrap)
        .count();
    assert_eq!(found, 3);

    assert!(traced.gets.borrow().is_empty());
    let scans = traced.scans.borrow();
    assert_eq!(scans.len(), 1);
    assert_eq!(scans[0].get(KIND_AT), Some(&ADJACENCY));
    let scanned = traced.scanned.borrow();
    let (_, matched) = scanned.split_last().expect("scan read keys");
    assert_eq!(matched.len(), found);
    assert!(
        matched
            .iter()
            .all(|key| key.get(KIND_AT) == Some(&ADJACENCY))
    );
}
