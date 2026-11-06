//! Integrity checks run before a mutation stages any key.
//!
//! Every check reads through [`KeyRead`]. Inside a
//! [`GraphBatch`](crate::GraphBatch) that includes writes staged earlier in
//! the same batch.

use pizzlydb_core::{ID_LEN, NamespaceId, NodeId};

use crate::access::{KeyRead, PrefixScan};
use crate::directory;
use crate::error::GraphError;
use crate::keys;

/// Both endpoints must be live nodes.
pub(crate) fn ensure_endpoints_live<R: KeyRead>(
    read: &R,
    namespace: NamespaceId,
    source: NodeId,
    target: NodeId,
) -> Result<(), GraphError> {
    for node in [source, target] {
        match directory::load_node(read, namespace, node) {
            Ok(_) => {}
            Err(GraphError::NotFound | GraphError::Deleted) => {
                return Err(GraphError::EndpointNotLive);
            }
            Err(err) => return Err(err),
        }
    }

    Ok(())
}

/// `node` must have no adjacency key in either direction.
pub(crate) fn ensure_no_incident_edges<R: KeyRead>(
    read: &R,
    namespace: NamespaceId,
    node: NodeId,
) -> Result<(), GraphError> {
    let mut scan = PrefixScan::new(read, keys::adjacency_of(namespace, node))?;
    match scan.next().transpose()? {
        Some(_) => Err(GraphError::EndpointInUse),
        None => Ok(()),
    }
}

/// The unique entry at `key` must be free or held by `owner`.
pub(crate) fn ensure_unique<R: KeyRead>(
    read: &R,
    key: &[u8],
    owner: &[u8; ID_LEN],
) -> Result<(), GraphError> {
    match read.get(key)? {
        Some(holder) if holder.as_slice() != owner.as_slice() => Err(GraphError::UniqueConflict),
        _ => Ok(()),
    }
}
