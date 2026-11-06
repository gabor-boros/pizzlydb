//! Identifier-to-record map and identifier allocation.
//!
//! The directory is the key map itself. A node or edge id resolves with one
//! point read of its record key:
//!
//! - no key: the id was never issued, [`GraphError::NotFound`];
//! - a tombstone: the id was deleted, [`GraphError::Deleted`];
//! - a live record: the record.
//!
//! Tombstones are never removed by this crate, so a deleted id stays
//! [`GraphError::Deleted`].
//!
//! Server-generated ids come from one counter key per identifier kind. The
//! counter holds the last issued id as a big-endian `u128`. Allocation stages
//! the new counter value in the same batch as the record that uses it. A
//! counter only moves forward, so an id is never issued twice.

use pizzlydb_core::{EdgeId, ID_LEN, Identifier, NamespaceId, NodeId};
use pizzlydb_storage::Vfs;

use crate::access::KeyRead;
use crate::batch::GraphBatch;
use crate::error::{GraphError, corrupt};
use crate::keys;
use crate::property::Owner;
use crate::record::{EdgeRecord, NodeRecord, Stored, decode_edge, decode_node};

pub(crate) fn load_node<R: KeyRead>(
    read: &R,
    namespace: NamespaceId,
    id: NodeId,
) -> Result<NodeRecord, GraphError> {
    let bytes = read
        .get(&keys::node(namespace, id))?
        .ok_or(GraphError::NotFound)?;

    match decode_node(id, &bytes)? {
        Stored::Live(record) => Ok(record),
        Stored::Deleted => Err(GraphError::Deleted),
    }
}

pub(crate) fn load_edge<R: KeyRead>(
    read: &R,
    namespace: NamespaceId,
    id: EdgeId,
) -> Result<EdgeRecord, GraphError> {
    let bytes = read
        .get(&keys::edge(namespace, id))?
        .ok_or(GraphError::NotFound)?;

    match decode_edge(id, &bytes)? {
        Stored::Live(record) => Ok(record),
        Stored::Deleted => Err(GraphError::Deleted),
    }
}

/// Return `Ok` when `owner` is a live node or edge.
pub(crate) fn ensure_live<R: KeyRead>(
    read: &R,
    namespace: NamespaceId,
    owner: Owner,
) -> Result<(), GraphError> {
    match owner {
        Owner::Node(node) => load_node(read, namespace, node).map(drop),
        Owner::Edge(edge) => load_edge(read, namespace, edge).map(drop),
    }
}

/// Issue the next identifier of `id_kind` and stage the counter update.
pub(crate) fn allocate<I: Identifier, V: Vfs>(
    batch: &mut GraphBatch<'_, V>,
    namespace: NamespaceId,
    id_kind: u8,
) -> Result<I, GraphError> {
    let key = keys::counter(namespace, id_kind);
    let last = match batch.get(&key)? {
        None => 0,
        Some(bytes) => {
            u128::from_be_bytes(<[u8; ID_LEN]>::try_from(bytes.as_slice()).map_err(corrupt)?)
        }
    };

    let next = last.checked_add(1).ok_or(GraphError::IdExhausted)?;
    let id = I::from_bytes(next.to_be_bytes()).map_err(corrupt)?;
    batch.put(key, next.to_be_bytes().to_vec())?;

    Ok(id)
}
