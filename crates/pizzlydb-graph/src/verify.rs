//! Check that the keys of one namespace agree with its records.
//!
//! [`verify_full`] reads one snapshot. It does not stage writes.

use crate::access::{KeyRead, PrefixScan};
use crate::adjacency::Direction;
use crate::error::GraphError;
use crate::index::IndexDefinition;
use crate::keys::{self, ADJACENCY_EDGE, ADJACENCY_LEN, ADJACENCY_NEIGHBOR, ADJACENCY_TYPE};
use crate::property::{Owner, Scope};
use crate::record::EdgeRecord;
use crate::record_store::GraphStore;
use pizzlydb_core::{
    EdgeId, ID_LEN, IndexId, KeyBuf, LabelId, NodeId, TypeId, Value, decode_value_key,
};

/// Check records, adjacency, membership, and index entries in `graph`'s namespace.
///
/// A live edge has two live endpoints, one outgoing adjacency key, one incoming
/// adjacency key, and one type-membership key. Label keys and label-membership
/// keys name the same live nodes. Each property-index hit is an owner in that
/// index's scope whose stored value is the indexed value, and each stored value
/// in the scope has that hit. A unique entry names the owner of that non-null
/// value. A deleted record has no adjacency key and no type-membership key.
///
/// # Errors
///
/// Returns [`GraphError::VerificationFailed`] when a key is missing, extra, or
/// does not match the record it names, including when stored bytes do not
/// decode. Returns [`GraphError::Storage`] when a read fails.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use std::path::Path;
///
/// use pizzlydb_core::NamespaceId;
/// use pizzlydb_graph::{GraphBatch, GraphStore, verify_full};
/// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
///
/// let vfs = MemoryVfs::new();
/// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
/// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
/// let mut batch = GraphBatch::begin(&storage)?;
/// graph.create_node(&mut batch)?;
/// batch.commit()?;
///
/// let view = storage.snapshot()?;
/// verify_full(&graph, &view)?;
/// # Ok(())
/// # }
/// ```
pub fn verify_full<R: KeyRead>(graph: &GraphStore, view: &R) -> Result<(), GraphError> {
    let namespace = graph.namespace();
    for_each_prefix(view, &keys::node_keys(namespace), |key, _| {
        if key.len() != keys::node_keys(namespace).len() + ID_LEN {
            return Err(GraphError::VerificationFailed);
        }
        let id = id_at::<NodeId>(key, keys::node_keys(namespace).len())?;
        match graph.get_node(view, id) {
            Ok(_) => Ok(()),
            Err(GraphError::Deleted) => {
                if any_key(view, keys::adjacency_of(namespace, id))?
                    || any_key(view, keys::node_labels(namespace, id))?
                {
                    Err(GraphError::VerificationFailed)
                } else {
                    Ok(())
                }
            }
            Err(err) => Err(failed(err)),
        }
    })?;

    for_each_prefix(view, &keys::edge_keys(namespace), |key, _| {
        if key.len() != keys::edge_keys(namespace).len() + ID_LEN {
            return Err(GraphError::VerificationFailed);
        }
        let id = id_at::<EdgeId>(key, keys::edge_keys(namespace).len())?;
        match graph.get_edge(view, id) {
            Ok(record) => check_live_edge(graph, view, &record),
            Err(GraphError::Deleted) => Ok(()),
            Err(err) => Err(failed(err)),
        }
    })?;

    check_adjacency(graph, view)?;
    check_labels(graph, view)?;
    check_type_members(graph, view)?;

    for_each_prefix(view, &keys::index_keys(namespace), |key, _| {
        if key.len() < ID_LEN {
            return Err(GraphError::VerificationFailed);
        }
        let id = id_at::<IndexId>(key, key.len() - ID_LEN)?;
        let state = graph.index_state(view, id).map_err(failed)?;
        check_index(graph, view, id, state)
    })?;

    Ok(())
}

fn for_each_prefix<R: KeyRead>(
    view: &R,
    prefix: &[u8],
    mut visit: impl FnMut(&[u8], &[u8]) -> Result<(), GraphError>,
) -> Result<(), GraphError> {
    for item in view.scan(prefix).map_err(GraphError::Storage)? {
        let (key, value) = item.map_err(GraphError::Storage)?;
        if !key.starts_with(prefix) {
            break;
        }
        visit(&key, &value)?;
    }
    Ok(())
}

fn check_live_edge<R: KeyRead>(
    graph: &GraphStore,
    view: &R,
    record: &EdgeRecord,
) -> Result<(), GraphError> {
    graph.get_node(view, record.source).map_err(failed)?;
    graph.get_node(view, record.target).map_err(failed)?;

    let namespace = graph.namespace();
    let outgoing = keys::adjacency(
        namespace,
        record.source,
        Direction::Out.byte(),
        record.edge_type,
        record.target,
        record.id,
    );
    let incoming = keys::adjacency(
        namespace,
        record.target,
        Direction::In.byte(),
        record.edge_type,
        record.source,
        record.id,
    );
    let member = keys::type_member(namespace, record.edge_type, record.id);
    if !has_key(view, &outgoing)? || !has_key(view, &incoming)? || !has_key(view, &member)? {
        return Err(GraphError::VerificationFailed);
    }

    Ok(())
}

fn check_adjacency<R: KeyRead>(graph: &GraphStore, view: &R) -> Result<(), GraphError> {
    let prefix = keys::adjacency_keys(graph.namespace());
    for entry in PrefixScan::new(view, prefix).map_err(failed)? {
        let (key, _) = entry.map_err(failed)?;
        if key.len() != ADJACENCY_LEN {
            return Err(GraphError::VerificationFailed);
        }

        let node_at = KeyBuf::PREFIX_LEN;
        let direction = *key
            .get(node_at + ID_LEN)
            .ok_or(GraphError::VerificationFailed)?;
        let node = id_at::<NodeId>(&key, node_at)?;
        let edge_type = id_at::<TypeId>(&key, ADJACENCY_TYPE)?;
        let neighbor = id_at::<NodeId>(&key, ADJACENCY_NEIGHBOR)?;
        let edge = id_at::<EdgeId>(&key, ADJACENCY_EDGE)?;
        let record = graph.get_edge(view, edge).map_err(failed)?;
        let (expect_node, expect_neighbor) = match direction {
            1 => (record.source, record.target),
            2 => (record.target, record.source),
            _ => return Err(GraphError::VerificationFailed),
        };
        if node != expect_node || neighbor != expect_neighbor || edge_type != record.edge_type {
            return Err(GraphError::VerificationFailed);
        }
    }

    Ok(())
}

fn check_labels<R: KeyRead>(graph: &GraphStore, view: &R) -> Result<(), GraphError> {
    let namespace = graph.namespace();
    let listed = keys::node_label_keys(namespace);
    for entry in PrefixScan::new(view, listed.clone()).map_err(failed)? {
        let (key, _) = entry.map_err(failed)?;
        if key.len() != listed.len() + 2 * ID_LEN {
            return Err(GraphError::VerificationFailed);
        }
        let node = id_at::<NodeId>(&key, listed.len())?;
        let label = id_at::<LabelId>(&key, listed.len() + ID_LEN)?;
        graph.get_node(view, node).map_err(failed)?;
        graph.name::<LabelId, _>(view, label).map_err(failed)?;
        if !has_key(view, &keys::label_member(namespace, label, node))? {
            return Err(GraphError::VerificationFailed);
        }
    }

    let prefix = keys::label_member_keys(namespace);
    for entry in PrefixScan::new(view, prefix.clone()).map_err(failed)? {
        let (key, _) = entry.map_err(failed)?;
        if key.len() != prefix.len() + 2 * ID_LEN {
            return Err(GraphError::VerificationFailed);
        }
        let label = id_at::<LabelId>(&key, prefix.len())?;
        let node = id_at::<NodeId>(&key, prefix.len() + ID_LEN)?;
        graph.get_node(view, node).map_err(failed)?;
        if !graph.has_label(view, node, label).map_err(failed)? {
            return Err(GraphError::VerificationFailed);
        }
    }

    Ok(())
}

fn check_type_members<R: KeyRead>(graph: &GraphStore, view: &R) -> Result<(), GraphError> {
    let prefix = keys::type_member_keys(graph.namespace());
    for entry in PrefixScan::new(view, prefix.clone()).map_err(failed)? {
        let (key, _) = entry.map_err(failed)?;
        if key.len() != prefix.len() + 2 * ID_LEN {
            return Err(GraphError::VerificationFailed);
        }
        let edge_type = id_at::<TypeId>(&key, prefix.len())?;
        let edge = id_at::<EdgeId>(&key, prefix.len() + ID_LEN)?;
        let record = graph.get_edge(view, edge).map_err(failed)?;
        if record.edge_type != edge_type {
            return Err(GraphError::VerificationFailed);
        }
    }

    Ok(())
}

fn check_index<R: KeyRead>(
    graph: &GraphStore,
    view: &R,
    index: IndexId,
    state: crate::index::IndexState,
) -> Result<(), GraphError> {
    use crate::index::IndexPhase;

    if state.phase == IndexPhase::Dropped {
        let prefix = keys::index_entries(graph.namespace(), index, state.definition.unique);
        if any_key(view, prefix)? {
            return Err(GraphError::VerificationFailed);
        }
        return Ok(());
    }
    if state.phase != IndexPhase::Ready {
        return entries_match_stored(graph, view, index, state.definition);
    }

    entries_match_stored(graph, view, index, state.definition)?;
    members_are_indexed(graph, view, index, state.definition)
}

fn entries_match_stored<R: KeyRead>(
    graph: &GraphStore,
    view: &R,
    index: IndexId,
    definition: IndexDefinition,
) -> Result<(), GraphError> {
    let prefix = keys::index_entries(graph.namespace(), index, definition.unique);
    for_each_prefix(view, &prefix, |key, stored| {
        let suffix = key
            .get(prefix.len()..)
            .ok_or(GraphError::VerificationFailed)?;
        let (encoded, owner) = split_entry(definition, suffix, stored)?;
        let value = graph
            .get_property(view, owner, definition.key)
            .map_err(failed)?
            .ok_or(GraphError::VerificationFailed)?;
        let mut actual = Vec::new();
        pizzlydb_core::encode_value_key(&value, &mut actual)
            .map_err(|_| GraphError::VerificationFailed)?;
        if actual.as_slice() != encoded {
            return Err(GraphError::VerificationFailed);
        }
        Ok(())
    })
}

fn members_are_indexed<R: KeyRead>(
    graph: &GraphStore,
    view: &R,
    index: IndexId,
    definition: IndexDefinition,
) -> Result<(), GraphError> {
    let namespace = graph.namespace();
    let prefix = match definition.scope {
        Scope::Label(label) => keys::label_members(namespace, label),
        Scope::Type(edge_type) => keys::type_members(namespace, edge_type),
    };
    for_each_prefix(view, &prefix, |key, _| {
        if key.len() < ID_LEN {
            return Err(GraphError::VerificationFailed);
        }
        let owner = match definition.scope {
            Scope::Label(_) => Owner::Node(id_at(key, key.len() - ID_LEN)?),
            Scope::Type(_) => Owner::Edge(id_at(key, key.len() - ID_LEN)?),
        };
        let Some(value) = graph
            .get_property(view, owner, definition.key)
            .map_err(failed)?
        else {
            return Ok(());
        };
        if definition.unique && value == Value::Null {
            return Ok(());
        }
        let entry =
            crate::index::index_entry_key(namespace, index, definition.unique, owner, &value)
                .map_err(|_| GraphError::VerificationFailed)?;
        if !has_key(view, &entry)? {
            return Err(GraphError::VerificationFailed);
        }
        Ok(())
    })
}

fn split_entry(
    definition: IndexDefinition,
    suffix: &[u8],
    stored: &[u8],
) -> Result<(Vec<u8>, Owner), GraphError> {
    let (value_bytes, owner_bytes) = if definition.unique {
        if stored.len() != ID_LEN {
            return Err(GraphError::VerificationFailed);
        }
        (suffix, stored)
    } else {
        if !stored.is_empty() || suffix.len() < ID_LEN {
            return Err(GraphError::VerificationFailed);
        }
        let split = suffix.len() - ID_LEN;
        (&suffix[..split], &suffix[split..])
    };

    let (value, used) =
        decode_value_key(value_bytes).map_err(|_| GraphError::VerificationFailed)?;
    if used != value_bytes.len() || (definition.unique && value == Value::Null) {
        return Err(GraphError::VerificationFailed);
    }

    let owner = match definition.scope {
        Scope::Label(_) => Owner::Node(id_at(owner_bytes, 0)?),
        Scope::Type(_) => Owner::Edge(id_at(owner_bytes, 0)?),
    };
    if definition.unique && owner_bytes != owner.id_bytes() {
        return Err(GraphError::VerificationFailed);
    }

    Ok((value_bytes.to_vec(), owner))
}

fn any_key<R: KeyRead>(read: &R, prefix: Vec<u8>) -> Result<bool, GraphError> {
    let mut scan = PrefixScan::new(read, prefix).map_err(failed)?;
    Ok(scan.next().transpose().map_err(failed)?.is_some())
}

fn has_key<R: KeyRead>(read: &R, key: &[u8]) -> Result<bool, GraphError> {
    Ok(read.get(key)?.is_some())
}

fn id_at<I: pizzlydb_core::Identifier>(bytes: &[u8], offset: usize) -> Result<I, GraphError> {
    crate::access::id_at(bytes, offset).map_err(|_| GraphError::VerificationFailed)
}

fn failed(err: GraphError) -> GraphError {
    match err {
        GraphError::Storage(err) => GraphError::Storage(err),
        GraphError::VerificationFailed => GraphError::VerificationFailed,
        _ => GraphError::VerificationFailed,
    }
}
