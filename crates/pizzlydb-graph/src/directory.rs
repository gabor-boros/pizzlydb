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
//! Server-generated ids come from the transaction allocator. Each namespace
//! and id kind has its own sequence. The persisted watermark lives in the
//! system key space, not in a graph counter key. A deleted id keeps a
//! tombstone, so a later read returns [`GraphError::Deleted`].

use pizzlydb_core::{EdgeId, Identifier, NamespaceId, NodeId};
use pizzlydb_storage::Vfs;

use crate::access::KeyRead;
use crate::batch::GraphTxn;
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

/// Issue the next identifier of `id_kind`.
///
/// The id comes from the transaction manager's block allocator, not from a
/// counter key in the read set.
pub(crate) fn allocate<I: Identifier, V: Vfs>(
    batch: &mut GraphTxn<V>,
    namespace: NamespaceId,
    id_kind: u8,
) -> Result<I, GraphError> {
    let next = batch.allocate_id(namespace, id_kind)?;
    I::from_bytes(next.to_be_bytes()).map_err(corrupt)
}
