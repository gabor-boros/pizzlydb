//! The graph engine against the reference model.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;
use std::path::{Path, PathBuf};

use pizzlydb_core::{
    EdgeId, IndexId, LabelId, NamespaceId, NodeId, PropertyKeyId, TypeId, encode_value,
};
use pizzlydb_graph::{
    Direction, GraphBatch, GraphError, GraphStore, GraphTxn, Owner, PropertyDeclaration, Scope,
};
use pizzlydb_storage::{Durability, FileRole, MemoryVfs, Storage, Vfs, wal_path};
use pizzlydb_testkit::{
    Counters, DIR_IN, DIR_OUT, DeclFact, EdgeFact, GraphSnapshot, IndexFact, IndexHit, ModelScope,
    NAME_KEY, NAME_LABEL, NAME_TYPE, NeighborFact, OWNER_EDGE, OWNER_NODE, Operation,
    PROPERTY_STEPS, SCOPE_LABEL, SCOPE_TYPE, StepError, Subject, WorkloadBias, generate_history,
    history_strategy, run_history,
};
use proptest::prelude::*;
use proptest::test_runner::{Config, FileFailurePersistence};

struct Engine {
    vfs: MemoryVfs,
    generation: u32,
    path: String,
    storage: Option<Storage<MemoryVfs>>,
}

impl Engine {
    fn new() -> Self {
        let mut engine = Self {
            vfs: MemoryVfs::new(),
            generation: 0,
            path: String::new(),
            storage: None,
        };
        engine.open_fresh();
        engine
    }

    fn open_fresh(&mut self) {
        self.storage.take();
        if !self.path.is_empty() {
            let main = PathBuf::from(&self.path);
            let wal = wal_path(&main);
            let _ = self.vfs.remove(&main, FileRole::Main);
            let _ = self.vfs.remove(&wal, FileRole::Wal);
        }
        self.generation = self.generation.saturating_add(1);
        self.path = format!("db-{}", self.generation);
        let path = self.path.clone();
        let created = Storage::create(&self.vfs, Path::new(&path), 16, Durability::None, |id| {
            id.fill(1);
        })
        .expect("create database");
        self.storage = Some(created);
    }

    fn db(&self) -> &Storage<MemoryVfs> {
        self.storage.as_ref().expect("database is open")
    }
}

impl Subject for Engine {
    fn reset(&mut self) {
        self.open_fresh();
    }

    fn apply(&mut self, op: &Operation) -> Result<(), StepError> {
        let mut batch = GraphBatch::begin(self.db()).map_err(map_graph)?;
        match apply_op(&mut batch, op) {
            Ok(()) => {
                batch.commit().map_err(map_graph)?;
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    fn snapshot(&self) -> Result<GraphSnapshot, StepError> {
        observe(self.db())
    }
}

fn apply_op<V: Vfs>(batch: &mut GraphTxn<V>, op: &Operation) -> Result<(), StepError> {
    match op {
        Operation::CreateNode { namespace } => store(*namespace)?
            .create_node(batch)
            .map(|_| ())
            .map_err(map_graph),
        Operation::DeleteNode { namespace, node } => store(*namespace)?
            .delete_node(batch, ident::<NodeId>(*node)?)
            .map_err(map_graph),
        Operation::CreateEdge {
            namespace,
            source,
            target,
            edge_type,
        } => store(*namespace)?
            .create_edge(
                batch,
                ident::<NodeId>(*source)?,
                ident::<NodeId>(*target)?,
                ident::<TypeId>(*edge_type)?,
            )
            .map(|_| ())
            .map_err(map_graph),
        Operation::DeleteEdge { namespace, edge } => store(*namespace)?
            .delete_edge(batch, ident::<EdgeId>(*edge)?)
            .map_err(map_graph),
        Operation::InternLabel { namespace, name } => store(*namespace)?
            .intern::<LabelId, _>(batch, name)
            .map(|_| ())
            .map_err(map_graph),
        Operation::InternType { namespace, name } => store(*namespace)?
            .intern::<TypeId, _>(batch, name)
            .map(|_| ())
            .map_err(map_graph),
        Operation::InternKey { namespace, name } => store(*namespace)?
            .intern::<PropertyKeyId, _>(batch, name)
            .map(|_| ())
            .map_err(map_graph),
        Operation::RenameLabel {
            namespace,
            id,
            name,
        } => store(*namespace)?
            .rename(batch, ident::<LabelId>(*id)?, name)
            .map_err(map_graph),
        Operation::RenameType {
            namespace,
            id,
            name,
        } => store(*namespace)?
            .rename(batch, ident::<TypeId>(*id)?, name)
            .map_err(map_graph),
        Operation::RenameKey {
            namespace,
            id,
            name,
        } => store(*namespace)?
            .rename(batch, ident::<PropertyKeyId>(*id)?, name)
            .map_err(map_graph),
        Operation::AddLabel {
            namespace,
            node,
            label,
        } => store(*namespace)?
            .add_label(batch, ident::<NodeId>(*node)?, ident::<LabelId>(*label)?)
            .map_err(map_graph),
        Operation::RemoveLabel {
            namespace,
            node,
            label,
        } => store(*namespace)?
            .remove_label(batch, ident::<NodeId>(*node)?, ident::<LabelId>(*label)?)
            .map_err(map_graph),
        Operation::SetNodeProperty {
            namespace,
            node,
            key,
            value,
        } => store(*namespace)?
            .set_property(
                batch,
                ident::<NodeId>(*node)?,
                ident::<PropertyKeyId>(*key)?,
                value,
            )
            .map_err(map_graph),
        Operation::SetEdgeProperty {
            namespace,
            edge,
            key,
            value,
        } => store(*namespace)?
            .set_property(
                batch,
                ident::<EdgeId>(*edge)?,
                ident::<PropertyKeyId>(*key)?,
                value,
            )
            .map_err(map_graph),
        Operation::ClearNodeProperty {
            namespace,
            node,
            key,
        } => store(*namespace)?
            .clear_property(
                batch,
                ident::<NodeId>(*node)?,
                ident::<PropertyKeyId>(*key)?,
            )
            .map_err(map_graph),
        Operation::ClearEdgeProperty {
            namespace,
            edge,
            key,
        } => store(*namespace)?
            .clear_property(
                batch,
                ident::<EdgeId>(*edge)?,
                ident::<PropertyKeyId>(*key)?,
            )
            .map_err(map_graph),
        Operation::Declare {
            namespace,
            scope,
            key,
            value_type,
            nullable,
            required,
        } => store(*namespace)?
            .declare(
                batch,
                model_scope(*scope)?,
                ident::<PropertyKeyId>(*key)?,
                PropertyDeclaration {
                    value_type: *value_type,
                    nullable: *nullable,
                    required: *required,
                },
            )
            .map_err(map_graph),
        Operation::CreateIndex {
            namespace,
            scope,
            key,
            unique,
        } => store(*namespace)?
            .create_index(
                batch,
                model_scope(*scope)?,
                ident::<PropertyKeyId>(*key)?,
                *unique,
            )
            .map(|_| ())
            .map_err(map_graph),
    }
}

fn observe(storage: &Storage<MemoryVfs>) -> Result<GraphSnapshot, StepError> {
    let view = storage.snapshot().map_err(storage_err)?;
    let mut snapshot = empty_snapshot();
    for namespace in namespaces(&view)? {
        let graph = GraphStore::new(namespace);
        let ns = namespace.to_u128();
        let next_node = first_unissued(|id| node_issued(&graph, &view, id))?;
        let next_edge = first_unissued(|id| edge_issued(&graph, &view, id))?;
        let next_label = first_unissued(|id| named::<LabelId>(&graph, &view, id))?;
        let next_type = first_unissued(|id| named::<TypeId>(&graph, &view, id))?;
        let next_key = first_unissued(|id| named::<PropertyKeyId>(&graph, &view, id))?;
        let next_index = first_unissued(|id| index_issued(&graph, &view, id))?;
        snapshot.counters.insert(
            ns,
            Counters {
                next_node,
                next_edge,
                next_label,
                next_type,
                next_key,
                next_index,
            },
        );
        copy_names::<LabelId>(&mut snapshot, &graph, &view, ns, NAME_LABEL, next_label)?;
        copy_names::<TypeId>(&mut snapshot, &graph, &view, ns, NAME_TYPE, next_type)?;
        copy_names::<PropertyKeyId>(&mut snapshot, &graph, &view, ns, NAME_KEY, next_key)?;
        for id in 1..next_node {
            match graph.get_node(&view, ident::<NodeId>(id)?) {
                Ok(_) => {
                    snapshot.nodes.insert((ns, id));
                }
                Err(GraphError::Deleted) => {
                    snapshot.node_tombstones.insert((ns, id));
                }
                Err(err) => return Err(map_graph(err)),
            }
        }
        for id in 1..next_edge {
            match graph.get_edge(&view, ident::<EdgeId>(id)?) {
                Ok(record) => {
                    snapshot.edges.insert(
                        (ns, id),
                        EdgeFact {
                            source: record.source.to_u128(),
                            target: record.target.to_u128(),
                            edge_type: record.edge_type.to_u128(),
                        },
                    );
                }
                Err(GraphError::Deleted) => {
                    snapshot.edge_tombstones.insert((ns, id));
                }
                Err(err) => return Err(map_graph(err)),
            }
        }
        let nodes: Vec<u128> = snapshot
            .nodes
            .iter()
            .filter(|(item_ns, _)| *item_ns == ns)
            .map(|(_, id)| *id)
            .collect();
        for node in nodes {
            let labels = graph
                .labels(&view, ident::<NodeId>(node)?)
                .map_err(map_graph)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(map_graph)?;
            for label in labels {
                snapshot.labels.insert((ns, node, label.to_u128()));
            }
            copy_owner_props(&mut snapshot, &graph, &view, ns, OWNER_NODE, node, next_key)?;
            for direction in [Direction::Out, Direction::In] {
                let tag = if direction == Direction::Out {
                    DIR_OUT
                } else {
                    DIR_IN
                };
                let neighbors = graph
                    .neighbors(&view, ident::<NodeId>(node)?, direction)
                    .map_err(map_graph)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(map_graph)?;
                for neighbor in neighbors {
                    snapshot.neighbors.insert(NeighborFact {
                        namespace: ns,
                        node,
                        direction: tag,
                        edge_type: neighbor.edge_type.to_u128(),
                        neighbor: neighbor.neighbor.to_u128(),
                        edge: neighbor.edge.to_u128(),
                    });
                }
            }
        }
        let edges: Vec<u128> = snapshot
            .edges
            .keys()
            .filter(|(item_ns, _)| *item_ns == ns)
            .map(|(_, id)| *id)
            .collect();
        for edge in edges {
            copy_owner_props(&mut snapshot, &graph, &view, ns, OWNER_EDGE, edge, next_key)?;
        }
        for label in 1..next_label {
            copy_declarations(
                &mut snapshot,
                &graph,
                &view,
                ns,
                Scope::Label(ident::<LabelId>(label)?),
                SCOPE_LABEL,
                label,
                next_key,
            )?;
        }
        for edge_type in 1..next_type {
            copy_declarations(
                &mut snapshot,
                &graph,
                &view,
                ns,
                Scope::Type(ident::<TypeId>(edge_type)?),
                SCOPE_TYPE,
                edge_type,
                next_key,
            )?;
        }
        for index in 1..next_index {
            let definition = graph
                .index(&view, ident::<IndexId>(index)?)
                .map_err(map_graph)?;
            let (scope_kind, scope) = match definition.scope {
                Scope::Label(id) => (SCOPE_LABEL, id.to_u128()),
                Scope::Type(id) => (SCOPE_TYPE, id.to_u128()),
            };
            snapshot.indexes.insert(
                (ns, index),
                IndexFact {
                    scope_kind,
                    scope,
                    key: definition.key.to_u128(),
                    unique: definition.unique,
                },
            );
            let hits = graph
                .index_range(
                    &view,
                    ident::<IndexId>(index)?,
                    Bound::Unbounded,
                    Bound::Unbounded,
                )
                .map_err(map_graph)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(map_graph)?;
            for owner in hits {
                let (owner_kind, owner_id) = match owner {
                    Owner::Node(id) => (OWNER_NODE, id.to_u128()),
                    Owner::Edge(id) => (OWNER_EDGE, id.to_u128()),
                };
                let value = graph
                    .get_property(&view, owner, definition.key)
                    .map_err(map_graph)?
                    .map(|value| encode_value(&value).map_err(|_| StepError::InvalidValue))
                    .transpose()?
                    .unwrap_or_default();
                snapshot.index_hits.insert(IndexHit {
                    namespace: ns,
                    index,
                    value,
                    owner_kind,
                    owner: owner_id,
                });
            }
        }
    }
    Ok(snapshot)
}

fn empty_snapshot() -> GraphSnapshot {
    GraphSnapshot {
        counters: BTreeMap::new(),
        names: BTreeMap::new(),
        nodes: BTreeSet::new(),
        node_tombstones: BTreeSet::new(),
        edges: BTreeMap::new(),
        edge_tombstones: BTreeSet::new(),
        labels: BTreeSet::new(),
        properties: BTreeMap::new(),
        declarations: BTreeMap::new(),
        indexes: BTreeMap::new(),
        neighbors: BTreeSet::new(),
        index_hits: BTreeSet::new(),
    }
}

fn namespaces(view: &impl pizzlydb_graph::KeyRead) -> Result<Vec<NamespaceId>, StepError> {
    let mut ids = BTreeSet::new();
    for item in view.scan(&[]).map_err(storage_err)? {
        let (key, _) = item.map_err(storage_err)?;
        if key.len() < 16 {
            continue;
        }
        let mut raw = [0u8; 16];
        raw.copy_from_slice(&key[..16]);
        if let Ok(id) = NamespaceId::from_bytes(raw) {
            ids.insert(id);
        }
    }
    Ok(ids.into_iter().collect())
}

fn copy_names<I>(
    snapshot: &mut GraphSnapshot,
    graph: &GraphStore,
    view: &impl pizzlydb_graph::KeyRead,
    namespace: u128,
    kind: u8,
    next: u128,
) -> Result<(), StepError>
where
    I: pizzlydb_graph::CatalogId + FromU128,
{
    for id in 1..next {
        let name = graph.name(view, ident::<I>(id)?).map_err(map_graph)?;
        snapshot.names.insert((namespace, kind, id), name);
    }
    Ok(())
}

fn copy_owner_props(
    snapshot: &mut GraphSnapshot,
    graph: &GraphStore,
    view: &impl pizzlydb_graph::KeyRead,
    namespace: u128,
    owner_kind: u8,
    owner: u128,
    next_key: u128,
) -> Result<(), StepError> {
    let owner_id = match owner_kind {
        OWNER_NODE => Owner::Node(ident::<NodeId>(owner)?),
        _ => Owner::Edge(ident::<EdgeId>(owner)?),
    };
    for key in 1..next_key {
        if let Some(value) = graph
            .get_property(view, owner_id, ident::<PropertyKeyId>(key)?)
            .map_err(map_graph)?
        {
            let bytes = encode_value(&value).map_err(|_| StepError::InvalidValue)?;
            snapshot
                .properties
                .insert((namespace, owner_kind, owner, key), bytes);
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn copy_declarations(
    snapshot: &mut GraphSnapshot,
    graph: &GraphStore,
    view: &impl pizzlydb_graph::KeyRead,
    namespace: u128,
    scope: Scope,
    scope_kind: u8,
    scope_id: u128,
    next_key: u128,
) -> Result<(), StepError> {
    for key in 1..next_key {
        if let Some(declaration) = graph
            .declaration(view, scope, ident::<PropertyKeyId>(key)?)
            .map_err(map_graph)?
        {
            snapshot.declarations.insert(
                (namespace, scope_kind, scope_id, key),
                DeclFact {
                    type_tag: declaration.value_type.tag(),
                    nullable: declaration.nullable,
                    required: declaration.required,
                },
            );
        }
    }
    Ok(())
}

fn first_unissued(
    mut issued: impl FnMut(u128) -> Result<bool, StepError>,
) -> Result<u128, StepError> {
    if !issued(1)? {
        return Ok(1);
    }
    let mut high = 1u128;
    for _ in 0..40 {
        let Some(next) = high.checked_mul(2) else {
            return Err(StepError::Other("counter search overflowed".to_owned()));
        };
        if !issued(next)? {
            let mut low = high;
            high = next;
            while low + 1 < high {
                let mid = low + (high - low) / 2;
                if issued(mid)? {
                    low = mid;
                } else {
                    high = mid;
                }
            }
            return Ok(high);
        }
        high = next;
    }
    Err(StepError::Other("counter search gave up".to_owned()))
}

fn node_issued(
    graph: &GraphStore,
    view: &impl pizzlydb_graph::KeyRead,
    id: u128,
) -> Result<bool, StepError> {
    match graph.get_node(view, ident::<NodeId>(id)?) {
        Ok(_) | Err(GraphError::Deleted) => Ok(true),
        Err(GraphError::NotFound) => Ok(false),
        Err(err) => Err(map_graph(err)),
    }
}

fn edge_issued(
    graph: &GraphStore,
    view: &impl pizzlydb_graph::KeyRead,
    id: u128,
) -> Result<bool, StepError> {
    match graph.get_edge(view, ident::<EdgeId>(id)?) {
        Ok(_) | Err(GraphError::Deleted) => Ok(true),
        Err(GraphError::NotFound) => Ok(false),
        Err(err) => Err(map_graph(err)),
    }
}

fn named<I: pizzlydb_graph::CatalogId + FromU128>(
    graph: &GraphStore,
    view: &impl pizzlydb_graph::KeyRead,
    id: u128,
) -> Result<bool, StepError> {
    match graph.name(view, ident::<I>(id)?) {
        Ok(_) => Ok(true),
        Err(GraphError::InvalidIdentity) => Ok(false),
        Err(err) => Err(map_graph(err)),
    }
}

fn index_issued(
    graph: &GraphStore,
    view: &impl pizzlydb_graph::KeyRead,
    id: u128,
) -> Result<bool, StepError> {
    match graph.index(view, ident::<IndexId>(id)?) {
        Ok(_) => Ok(true),
        Err(GraphError::InvalidIdentity) => Ok(false),
        Err(err) => Err(map_graph(err)),
    }
}

fn store(namespace: u128) -> Result<GraphStore, StepError> {
    Ok(GraphStore::new(ident::<NamespaceId>(namespace)?))
}

fn model_scope(scope: ModelScope) -> Result<Scope, StepError> {
    match scope {
        ModelScope::Label(id) => Ok(Scope::Label(ident::<LabelId>(id)?)),
        ModelScope::Type(id) => Ok(Scope::Type(ident::<TypeId>(id)?)),
    }
}

fn ident<T>(id: u128) -> Result<T, StepError>
where
    T: FromU128,
{
    T::from_u128(id).map_err(|_| StepError::InvalidIdentity)
}

trait FromU128: Sized {
    fn from_u128(value: u128) -> Result<Self, pizzlydb_core::CoreError>;
}

macro_rules! from_u128 {
    ($($ty:ty),* $(,)?) => {
        $(impl FromU128 for $ty {
            fn from_u128(value: u128) -> Result<Self, pizzlydb_core::CoreError> {
                <$ty>::from_u128(value)
            }
        })*
    };
}

from_u128!(
    NamespaceId,
    NodeId,
    EdgeId,
    LabelId,
    TypeId,
    PropertyKeyId,
    IndexId
);

fn map_graph(err: GraphError) -> StepError {
    match err {
        GraphError::NotFound => StepError::NotFound,
        GraphError::Deleted => StepError::Deleted,
        GraphError::InvalidIdentity => StepError::InvalidIdentity,
        GraphError::EndpointNotLive => StepError::EndpointNotLive,
        GraphError::EndpointInUse => StepError::EndpointInUse,
        GraphError::InvalidName => StepError::InvalidName,
        GraphError::NameConflict => StepError::NameConflict,
        GraphError::SchemaConflict | GraphError::ConstraintViolation { .. } => {
            StepError::SchemaConflict
        }
        GraphError::UniqueConflict => StepError::UniqueConflict,
        GraphError::InvalidValue => StepError::InvalidValue,
        other => StepError::Other(other.to_string()),
    }
}

fn storage_err(err: pizzlydb_storage::StorageError) -> StepError {
    StepError::Other(err.to_string())
}

fn cases() -> u32 {
    std::env::var("PIZZLYDB_PROPTEST_CASES")
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(1000)
}

fn config(cases: u32) -> Config {
    let mut config = Config::with_cases(cases);
    config.failure_persistence = Some(Box::new(FileFailurePersistence::Off));
    config
}

proptest! {
    #![proptest_config(config(cases()))]
    #[test]
    fn graph_matches_the_reference_model(seed in any::<u64>()) {
        let history = generate_history(seed, PROPERTY_STEPS, &WorkloadBias::default());
        let mut engine = Engine::new();
        run_history(&mut engine, &history).unwrap_or_else(|err| panic!("{err}"));
    }
}

proptest! {
    #![proptest_config(config(64))]
    #[test]
    fn shrinkable_histories_match_the_reference_model(history in history_strategy()) {
        let mut engine = Engine::new();
        run_history(&mut engine, &history).unwrap_or_else(|err| panic!("{err}"));
    }
}

#[test]
fn replaying_a_seed_matches_the_first_run() {
    let history = generate_history(99, PROPERTY_STEPS, &WorkloadBias::default());
    let mut first = Engine::new();
    let mut second = Engine::new();
    run_history(&mut first, &history).unwrap();
    run_history(&mut second, &history).unwrap();
    assert_eq!(first.snapshot().unwrap(), second.snapshot().unwrap());
}

#[test]
fn replay_seed_from_the_environment() {
    let Ok(raw) = std::env::var("PIZZLYDB_SEED") else {
        return;
    };
    let seed = raw.parse::<u64>().expect("PIZZLYDB_SEED is a u64");
    let history = generate_history(seed, PROPERTY_STEPS, &WorkloadBias::default());
    let mut engine = Engine::new();
    run_history(&mut engine, &history).unwrap_or_else(|err| panic!("{err}"));
}
