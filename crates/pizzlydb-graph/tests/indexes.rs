//! Secondary indexes, unique entries, and membership keys.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;

use common::{Rng, count_kind, crash_and_reopen, create, dump, graph};
use pizzlydb_core::{
    EdgeId, IndexId, LabelId, NodeId, PropertyKeyId, TypeId, Value, encode_value_key,
};
use pizzlydb_graph::{GraphBatch, GraphError, GraphStore, IndexDefinition, KeyRead, Owner, Scope};
use pizzlydb_storage::MemoryVfs;

const LABEL_MEMBER: u8 = 0x08;
const TYPE_MEMBER: u8 = 0x09;
const UNIQUE_ENTRY: u8 = 0x0F;
const PROPERTY_ENTRY: u8 = 0x10;

fn encoded(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    encode_value_key(value, &mut out).unwrap();
    out
}

fn owners<R: KeyRead>(
    graph: &GraphStore,
    read: &R,
    index: IndexId,
    lower: Bound<&Value>,
    upper: Bound<&Value>,
) -> Vec<Owner> {
    graph
        .index_range(read, index, lower, upper)
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn lookup<R: KeyRead>(graph: &GraphStore, read: &R, index: IndexId, value: &Value) -> Vec<Owner> {
    graph
        .index_lookup(read, index, value)
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

struct Fixture {
    person: LabelId,
    email: PropertyKeyId,
    city: PropertyKeyId,
    unique: IndexId,
    plain: IndexId,
}

fn fixture(graph: &GraphStore, batch: &mut GraphBatch<'_, MemoryVfs>) -> Fixture {
    let person = graph.intern(batch, "Person").unwrap();
    let email = graph.intern(batch, "email").unwrap();
    let city = graph.intern(batch, "city").unwrap();
    let unique = graph
        .create_index(batch, Scope::Label(person), email, true)
        .unwrap();
    let plain = graph
        .create_index(batch, Scope::Label(person), city, false)
        .unwrap();
    Fixture {
        person,
        email,
        city,
        unique,
        plain,
    }
}

fn person(graph: &GraphStore, batch: &mut GraphBatch<'_, MemoryVfs>, label: LabelId) -> NodeId {
    let node = graph.create_node(batch).unwrap();
    graph.add_label(batch, node, label).unwrap();
    node
}

#[test]
fn unique_checks_see_values_staged_in_the_batch() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let f = fixture(&graph, &mut batch);
    let a = person(&graph, &mut batch, f.person);
    let b = person(&graph, &mut batch, f.person);
    let email = Value::from("a@example.com");
    graph.set_property(&mut batch, a, f.email, &email).unwrap();

    let staged = dump(&batch);
    assert!(matches!(
        graph.set_property(&mut batch, b, f.email, &email),
        Err(GraphError::UniqueConflict)
    ));
    assert_eq!(dump(&batch), staged);

    graph.set_property(&mut batch, a, f.email, &email).unwrap();
    graph
        .set_property(&mut batch, a, f.email, &Value::from("other@example.com"))
        .unwrap();
    graph.set_property(&mut batch, b, f.email, &email).unwrap();
    batch.commit().unwrap();

    let view = storage.snapshot().unwrap();
    assert_eq!(
        lookup(&graph, &view, f.unique, &email),
        vec![Owner::Node(b)]
    );
}

#[test]
fn rollback_releases_a_unique_value() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let f = fixture(&graph, &mut batch);
    let a = person(&graph, &mut batch, f.person);
    let b = person(&graph, &mut batch, f.person);
    batch.commit().unwrap();

    let email = Value::from("taken@example.com");
    let mut batch = GraphBatch::begin(&storage).unwrap();
    graph.set_property(&mut batch, a, f.email, &email).unwrap();
    drop(batch);

    let storage = crash_and_reopen(&vfs, storage);
    let mut batch = GraphBatch::begin(&storage).unwrap();
    graph.set_property(&mut batch, b, f.email, &email).unwrap();
    batch.commit().unwrap();

    let view = storage.snapshot().unwrap();
    assert_eq!(
        lookup(&graph, &view, f.unique, &email),
        vec![Owner::Node(b)]
    );
    assert_eq!(graph.get_property(&view, a, f.email).unwrap(), None);
}

#[test]
fn null_is_indexed_and_absence_is_not() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let f = fixture(&graph, &mut batch);
    let absent = person(&graph, &mut batch, f.person);
    let first_null = person(&graph, &mut batch, f.person);
    let second_null = person(&graph, &mut batch, f.person);
    for node in [first_null, second_null] {
        graph
            .set_property(&mut batch, node, f.city, &Value::Null)
            .unwrap();
        graph
            .set_property(&mut batch, node, f.email, &Value::Null)
            .unwrap();
    }
    batch.commit().unwrap();

    let view = storage.snapshot().unwrap();
    let all = owners(&graph, &view, f.plain, Bound::Unbounded, Bound::Unbounded);
    assert_eq!(all, vec![Owner::Node(first_null), Owner::Node(second_null)]);
    assert!(!all.contains(&Owner::Node(absent)));
    assert_eq!(lookup(&graph, &view, f.plain, &Value::Null).len(), 2);
    assert!(lookup(&graph, &view, f.unique, &Value::Null).is_empty());
    assert!(owners(&graph, &view, f.unique, Bound::Unbounded, Bound::Unbounded).is_empty());
}

#[test]
fn old_snapshots_do_not_see_new_entries() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let f = fixture(&graph, &mut batch);
    let node = person(&graph, &mut batch, f.person);
    graph
        .set_property(&mut batch, node, f.city, &Value::from("Oslo"))
        .unwrap();
    batch.commit().unwrap();

    let old = storage.snapshot().unwrap();
    let mut batch = GraphBatch::begin(&storage).unwrap();
    graph
        .set_property(&mut batch, node, f.city, &Value::from("Rome"))
        .unwrap();
    let other = person(&graph, &mut batch, f.person);
    graph
        .set_property(&mut batch, other, f.city, &Value::from("Oslo"))
        .unwrap();
    batch.commit().unwrap();
    let new = storage.snapshot().unwrap();

    let oslo = Value::from("Oslo");
    let rome = Value::from("Rome");
    assert_eq!(
        lookup(&graph, &old, f.plain, &oslo),
        vec![Owner::Node(node)]
    );
    assert!(lookup(&graph, &old, f.plain, &rome).is_empty());
    assert_eq!(
        lookup(&graph, &new, f.plain, &oslo),
        vec![Owner::Node(other)]
    );
    assert_eq!(
        lookup(&graph, &new, f.plain, &rome),
        vec![Owner::Node(node)]
    );
    assert_eq!(graph.nodes_with_label(&old, f.person).unwrap().count(), 1);
}

#[test]
fn building_an_index_covers_existing_members() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let person: LabelId = graph.intern(&mut batch, "Person").unwrap();
    let robot: LabelId = graph.intern(&mut batch, "Robot").unwrap();
    let age: PropertyKeyId = graph.intern(&mut batch, "age").unwrap();
    let mut people = Vec::new();
    for years in [30_i64, 20, 30] {
        let node = person_with(&graph, &mut batch, person, age, Value::Int(years));
        people.push(node);
    }
    person_with(&graph, &mut batch, robot, age, Value::Int(30));
    batch.commit().unwrap();

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let staged = dump(&batch);
    assert!(matches!(
        graph.create_index(&mut batch, Scope::Label(person), age, true),
        Err(GraphError::UniqueConflict)
    ));
    assert_eq!(dump(&batch), staged);

    let index = graph
        .create_index(&mut batch, Scope::Label(person), age, false)
        .unwrap();
    let staged = dump(&batch);
    assert_eq!(
        graph
            .create_index(&mut batch, Scope::Label(person), age, false)
            .unwrap(),
        index
    );
    assert_eq!(dump(&batch), staged);
    assert!(matches!(
        graph.create_index(
            &mut batch,
            Scope::Type(TypeId::from_u128(99).unwrap()),
            age,
            false
        ),
        Err(GraphError::InvalidIdentity)
    ));

    let old = storage.snapshot().unwrap();
    batch.commit().unwrap();
    assert_eq!(
        graph
            .find_index(&old, Scope::Label(person), age, false)
            .unwrap(),
        None
    );

    let view = storage.snapshot().unwrap();
    assert_eq!(
        graph.index(&view, index).unwrap(),
        IndexDefinition {
            scope: Scope::Label(person),
            key: age,
            unique: false
        }
    );
    assert_eq!(
        owners(&graph, &view, index, Bound::Unbounded, Bound::Unbounded),
        vec![
            Owner::Node(people[1]),
            Owner::Node(people[0]),
            Owner::Node(people[2])
        ]
    );
}

fn person_with(
    graph: &GraphStore,
    batch: &mut GraphBatch<'_, MemoryVfs>,
    label: LabelId,
    key: PropertyKeyId,
    value: Value,
) -> NodeId {
    let node = person(graph, batch, label);
    graph.set_property(batch, node, key, &value).unwrap();
    node
}

#[test]
fn label_changes_move_entries() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let f = fixture(&graph, &mut batch);
    let email = Value::from("same@example.com");
    let member = person(&graph, &mut batch, f.person);
    graph
        .set_property(&mut batch, member, f.email, &email)
        .unwrap();
    let outsider = graph.create_node(&mut batch).unwrap();
    graph
        .set_property(&mut batch, outsider, f.email, &email)
        .unwrap();
    graph
        .set_property(&mut batch, outsider, f.city, &Value::from("Oslo"))
        .unwrap();
    assert!(lookup(&graph, &batch, f.plain, &Value::from("Oslo")).is_empty());

    let staged = dump(&batch);
    assert!(matches!(
        graph.add_label(&mut batch, outsider, f.person),
        Err(GraphError::UniqueConflict)
    ));
    assert!(matches!(
        graph.set_labels(&mut batch, outsider, &[f.person]),
        Err(GraphError::UniqueConflict)
    ));
    assert_eq!(dump(&batch), staged);

    graph.remove_label(&mut batch, member, f.person).unwrap();
    assert!(lookup(&graph, &batch, f.unique, &email).is_empty());
    graph.add_label(&mut batch, outsider, f.person).unwrap();
    batch.commit().unwrap();

    let view = storage.snapshot().unwrap();
    assert_eq!(
        lookup(&graph, &view, f.unique, &email),
        vec![Owner::Node(outsider)]
    );
    assert_eq!(
        lookup(&graph, &view, f.plain, &Value::from("Oslo")),
        vec![Owner::Node(outsider)]
    );
}

#[test]
fn deletes_remove_entries_and_membership() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let f = fixture(&graph, &mut batch);
    let knows: TypeId = graph.intern(&mut batch, "KNOWS").unwrap();
    let edge_index = graph
        .create_index(&mut batch, Scope::Type(knows), f.city, false)
        .unwrap();
    let a = person(&graph, &mut batch, f.person);
    let b = person(&graph, &mut batch, f.person);
    for node in [a, b] {
        graph
            .set_property(&mut batch, node, f.city, &Value::from("Oslo"))
            .unwrap();
    }
    graph
        .set_property(&mut batch, a, f.email, &Value::from("a@x"))
        .unwrap();
    let edge = graph.create_edge(&mut batch, a, b, knows).unwrap();
    graph
        .set_property(&mut batch, edge, f.city, &Value::from("Oslo"))
        .unwrap();
    batch.commit().unwrap();

    let view = storage.snapshot().unwrap();
    assert_eq!(
        lookup(&graph, &view, edge_index, &Value::from("Oslo")),
        vec![Owner::Edge(edge)]
    );
    for kind in [LABEL_MEMBER, TYPE_MEMBER, UNIQUE_ENTRY, PROPERTY_ENTRY] {
        assert!(count_kind(&view, kind) > 0);
    }
    drop(view);

    let mut batch = GraphBatch::begin(&storage).unwrap();
    graph.delete_edge(&mut batch, edge).unwrap();
    graph.delete_node(&mut batch, a).unwrap();
    graph.delete_node(&mut batch, b).unwrap();
    batch.commit().unwrap();

    let view = storage.snapshot().unwrap();
    for kind in [LABEL_MEMBER, TYPE_MEMBER, UNIQUE_ENTRY, PROPERTY_ENTRY] {
        assert_eq!(count_kind(&view, kind), 0, "kind {kind:#04x}");
    }
    let mut batch = GraphBatch::begin(&storage).unwrap();
    let c = person(&graph, &mut batch, f.person);
    graph
        .set_property(&mut batch, c, f.email, &Value::from("a@x"))
        .unwrap();
}

#[test]
fn ranges_follow_value_order() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let f = fixture(&graph, &mut batch);
    let values = [
        Value::Null,
        Value::Bool(false),
        Value::Int(-10),
        Value::Int(0),
        Value::Int(10),
        Value::Float(-0.5),
        Value::Float(0.0),
        Value::Float(2.5),
        Value::from("a"),
        Value::from("ab"),
        Value::from("b"),
        Value::from(&[0_u8][..]),
    ];
    let mut nodes = Vec::new();
    for value in &values {
        nodes.push(person_with(
            &graph,
            &mut batch,
            f.person,
            f.city,
            value.clone(),
        ));
    }
    batch.commit().unwrap();
    let view = storage.snapshot().unwrap();
    let node_at = |range: std::ops::Range<usize>| -> Vec<Owner> {
        nodes[range].iter().map(|&node| Owner::Node(node)).collect()
    };

    let all = owners(&graph, &view, f.plain, Bound::Unbounded, Bound::Unbounded);
    assert_eq!(all, node_at(0..values.len()));
    let ints = owners(
        &graph,
        &view,
        f.plain,
        Bound::Included(&Value::Int(i64::MIN)),
        Bound::Included(&Value::Int(i64::MAX)),
    );
    assert_eq!(ints, node_at(2..5));
    let exclusive = owners(
        &graph,
        &view,
        f.plain,
        Bound::Excluded(&Value::Int(-10)),
        Bound::Excluded(&Value::Int(10)),
    );
    assert_eq!(exclusive, node_at(3..4));
    let floats = owners(
        &graph,
        &view,
        f.plain,
        Bound::Excluded(&Value::Float(-0.5)),
        Bound::Included(&Value::Float(f64::INFINITY)),
    );
    assert_eq!(floats, node_at(6..8));
    let negative_zero = lookup(&graph, &view, f.plain, &Value::Float(-0.0));
    assert_eq!(negative_zero, node_at(6..7));
    let strings = owners(
        &graph,
        &view,
        f.plain,
        Bound::Included(&Value::from("a")),
        Bound::Excluded(&Value::from("b")),
    );
    assert_eq!(strings, node_at(8..10));
    let tail = owners(
        &graph,
        &view,
        f.plain,
        Bound::Excluded(&Value::from("ab")),
        Bound::Unbounded,
    );
    assert_eq!(tail, node_at(10..12));
    assert!(matches!(
        graph.index_lookup(&view, f.plain, &Value::Float(f64::NAN)),
        Err(GraphError::InvalidValue)
    ));
    assert!(matches!(
        graph.index_lookup(&view, IndexId::from_u128(99).unwrap(), &Value::Null),
        Err(GraphError::InvalidIdentity)
    ));
}

#[derive(Clone, Default)]
struct NodeState {
    labels: BTreeSet<LabelId>,
    props: BTreeMap<PropertyKeyId, Value>,
}

#[derive(Clone)]
struct EdgeState {
    source: NodeId,
    target: NodeId,
    props: BTreeMap<PropertyKeyId, Value>,
}

#[derive(Clone, Default)]
struct Model {
    nodes: BTreeMap<NodeId, NodeState>,
    edges: BTreeMap<EdgeId, EdgeState>,
}

impl Model {
    fn unique_taken(
        &self,
        label: LabelId,
        key: PropertyKeyId,
        node: NodeId,
        value: &Value,
    ) -> bool {
        *value != Value::Null
            && self.nodes.iter().any(|(&other, state)| {
                other != node
                    && state.labels.contains(&label)
                    && state.props.get(&key).map(encoded) == Some(encoded(value))
            })
    }

    fn expected(&self, definition: &IndexDefinition) -> Vec<(Vec<u8>, Owner)> {
        let present = |props: &BTreeMap<PropertyKeyId, Value>| {
            props
                .get(&definition.key)
                .filter(|value| !(definition.unique && **value == Value::Null))
                .map(encoded)
        };
        let mut entries: Vec<(Vec<u8>, Owner)> = match definition.scope {
            Scope::Label(label) => self
                .nodes
                .iter()
                .filter(|(_, state)| state.labels.contains(&label))
                .filter_map(|(&node, state)| present(&state.props).map(|v| (v, Owner::Node(node))))
                .collect(),
            Scope::Type(_) => self
                .edges
                .iter()
                .filter_map(|(&edge, state)| present(&state.props).map(|v| (v, Owner::Edge(edge))))
                .collect(),
        };
        entries.sort();
        entries
    }
}

fn assert_indexes_match(
    graph: &GraphStore,
    read: &impl KeyRead,
    model: &Model,
    indexes: &[IndexId],
    labels: &[LabelId],
    pool: &[Value],
) {
    for &index in indexes {
        let definition = graph.index(read, index).unwrap();
        let expected = model.expected(&definition);
        let got = owners(graph, read, index, Bound::Unbounded, Bound::Unbounded);
        let want: Vec<Owner> = expected.iter().map(|(_, owner)| *owner).collect();
        assert_eq!(got, want, "{definition:?}");
        for value in pool {
            let key = encoded(value);
            let want: Vec<Owner> = expected
                .iter()
                .filter(|(v, _)| *v == key)
                .map(|(_, owner)| *owner)
                .collect();
            assert_eq!(
                lookup(graph, read, index, value),
                want,
                "{definition:?} {value:?}"
            );
        }
    }
    for &label in labels {
        let got: Vec<NodeId> = graph
            .nodes_with_label(read, label)
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let want: Vec<NodeId> = model
            .nodes
            .iter()
            .filter(|(_, state)| state.labels.contains(&label))
            .map(|(&node, _)| node)
            .collect();
        assert_eq!(got, want);
    }
}

#[test]
fn index_scans_agree_with_brute_force_over_random_histories() {
    let vfs = MemoryVfs::new();
    let mut storage = create(&vfs);
    let graph = graph(5);
    let mut rng = Rng::new(0x5EED_0012);
    let pool = [
        Value::Null,
        Value::Int(0),
        Value::Int(1),
        Value::Int(2),
        Value::Float(-0.0),
        Value::Float(1.5),
        Value::from("a"),
        Value::from("b"),
        Value::from(&[1_u8][..]),
    ];

    let mut batch = GraphBatch::begin(&storage).unwrap();
    let a: LabelId = graph.intern(&mut batch, "A").unwrap();
    let b: LabelId = graph.intern(&mut batch, "B").unwrap();
    let t: TypeId = graph.intern(&mut batch, "T").unwrap();
    let k: PropertyKeyId = graph.intern(&mut batch, "k").unwrap();
    let u: PropertyKeyId = graph.intern(&mut batch, "u").unwrap();
    let mut indexes = vec![
        graph
            .create_index(&mut batch, Scope::Label(a), k, false)
            .unwrap(),
        graph
            .create_index(&mut batch, Scope::Label(b), u, true)
            .unwrap(),
        graph
            .create_index(&mut batch, Scope::Type(t), k, false)
            .unwrap(),
    ];
    batch.commit().unwrap();
    let labels = [a, b];
    let keys = [k, u];

    let mut committed = Model::default();
    let mut unique_conflicts = 0_usize;
    for round in 0..50 {
        if round == 25 {
            let mut batch = GraphBatch::begin(&storage).unwrap();
            indexes.push(
                graph
                    .create_index(&mut batch, Scope::Label(b), k, false)
                    .unwrap(),
            );
            indexes.push(
                graph
                    .create_index(&mut batch, Scope::Label(a), u, false)
                    .unwrap(),
            );
            batch.commit().unwrap();
        }
        let mut model = committed.clone();
        let mut batch = GraphBatch::begin(&storage).unwrap();
        for _ in 0..rng.below(15) + 1 {
            let nodes: Vec<NodeId> = model.nodes.keys().copied().collect();
            let edges: Vec<EdgeId> = model.edges.keys().copied().collect();
            let before = dump(&batch);
            let failed = match rng.below(9) {
                0 | 1 => {
                    let node = graph.create_node(&mut batch).unwrap();
                    model.nodes.insert(node, NodeState::default());
                    false
                }
                2 => {
                    let Some(&node) = rng.pick(&nodes) else {
                        continue;
                    };
                    let in_use = model
                        .edges
                        .values()
                        .any(|e| e.source == node || e.target == node);
                    let result = graph.delete_node(&mut batch, node);
                    if in_use {
                        assert!(matches!(result, Err(GraphError::EndpointInUse)));
                    } else {
                        result.unwrap();
                        model.nodes.remove(&node);
                    }
                    in_use
                }
                3 => {
                    let Some(&node) = rng.pick(&nodes) else {
                        continue;
                    };
                    let label = *rng.pick(&labels).unwrap();
                    let state = &model.nodes[&node];
                    let conflict = label == b
                        && !state.labels.contains(&b)
                        && state
                            .props
                            .get(&u)
                            .is_some_and(|value| model.unique_taken(b, u, node, value));
                    let result = graph.add_label(&mut batch, node, label);
                    if conflict {
                        assert!(matches!(result, Err(GraphError::UniqueConflict)));
                        unique_conflicts += 1;
                    } else {
                        result.unwrap();
                        model.nodes.get_mut(&node).unwrap().labels.insert(label);
                    }
                    conflict
                }
                4 => {
                    let Some(&node) = rng.pick(&nodes) else {
                        continue;
                    };
                    let label = *rng.pick(&labels).unwrap();
                    graph.remove_label(&mut batch, node, label).unwrap();
                    model.nodes.get_mut(&node).unwrap().labels.remove(&label);
                    false
                }
                5 => {
                    let Some(&node) = rng.pick(&nodes) else {
                        continue;
                    };
                    let key = *rng.pick(&keys).unwrap();
                    let values = if key == u { &pool[..3] } else { &pool[..] };
                    let value = rng.pick(values).unwrap().clone();
                    let state = &model.nodes[&node];
                    let conflict = key == u
                        && state.labels.contains(&b)
                        && model.unique_taken(b, u, node, &value);
                    let result = graph.set_property(&mut batch, node, key, &value);
                    if conflict {
                        assert!(matches!(result, Err(GraphError::UniqueConflict)));
                        unique_conflicts += 1;
                    } else {
                        result.unwrap();
                        model.nodes.get_mut(&node).unwrap().props.insert(key, value);
                    }
                    conflict
                }
                6 => {
                    let Some(&node) = rng.pick(&nodes) else {
                        continue;
                    };
                    let key = *rng.pick(&keys).unwrap();
                    graph.clear_property(&mut batch, node, key).unwrap();
                    model.nodes.get_mut(&node).unwrap().props.remove(&key);
                    false
                }
                7 => {
                    let (Some(&source), Some(&target)) = (rng.pick(&nodes), rng.pick(&nodes))
                    else {
                        continue;
                    };
                    let edge = graph.create_edge(&mut batch, source, target, t).unwrap();
                    let state = EdgeState {
                        source,
                        target,
                        props: BTreeMap::new(),
                    };
                    model.edges.insert(edge, state);
                    false
                }
                _ => {
                    let Some(&edge) = rng.pick(&edges) else {
                        continue;
                    };
                    if rng.chance(40) {
                        graph.delete_edge(&mut batch, edge).unwrap();
                        model.edges.remove(&edge);
                    } else {
                        let value = rng.pick(&pool).unwrap().clone();
                        graph.set_property(&mut batch, edge, k, &value).unwrap();
                        model.edges.get_mut(&edge).unwrap().props.insert(k, value);
                    }
                    false
                }
            };
            if failed {
                assert_eq!(dump(&batch), before, "failed call staged keys");
            }
        }
        assert_indexes_match(&graph, &batch, &model, &indexes, &labels, &pool);

        if rng.chance(75) {
            batch.commit().unwrap();
            committed = model;
        } else {
            drop(batch);
        }
        if round % 10 == 9 {
            storage = crash_and_reopen(&vfs, storage);
        }
        let view = storage.snapshot().unwrap();
        assert_indexes_match(&graph, &view, &committed, &indexes, &labels, &pool);
    }
    assert!(unique_conflicts > 0, "history never hit a unique conflict");
}
