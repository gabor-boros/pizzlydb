//! Node and edge records.
//!
//! A record value starts with a version byte and a state byte. The version is
//! `1`. State `1` is live and state `2` is a tombstone. Byte `0` is invalid in
//! both fields.
//!
//! - Live node: `[1, 1]`.
//! - Live edge: `[1, 1] || source || target || type`.
//! - Tombstone of either kind: `[1, 2]`.
//!
//! Labels, properties, and adjacency are separate keys. A record holds no
//! storage location.

use pizzlydb_core::{EdgeId, ID_LEN, NodeId, TypeId};

use crate::access::id_at;
use crate::error::GraphError;

const VERSION: u8 = 1;
const LIVE: u8 = 1;
const DELETED: u8 = 2;
const HEADER: usize = 2;
const EDGE_LEN: usize = HEADER + 3 * ID_LEN;

/// A live node.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use std::path::Path;
///
/// use pizzlydb_core::NamespaceId;
/// use pizzlydb_graph::{GraphBatch, GraphStore, NodeRecord};
/// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
///
/// let vfs = MemoryVfs::new();
/// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
/// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
///
/// let mut batch = GraphBatch::begin(&storage)?;
/// let id = graph.create_node(&mut batch)?;
/// assert_eq!(graph.get_node(&batch, id)?, NodeRecord { id });
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodeRecord {
    /// Node identifier.
    pub id: NodeId,
}

/// A live directed edge.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use std::path::Path;
///
/// use pizzlydb_core::{NamespaceId, TypeId};
/// use pizzlydb_graph::{EdgeRecord, GraphBatch, GraphStore};
/// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
///
/// let vfs = MemoryVfs::new();
/// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
/// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
///
/// let mut batch = GraphBatch::begin(&storage)?;
/// let knows: TypeId = graph.intern(&mut batch, "KNOWS")?;
/// let a = graph.create_node(&mut batch)?;
/// let b = graph.create_node(&mut batch)?;
/// let id = graph.create_edge(&mut batch, a, b, knows)?;
/// assert_eq!(
///     graph.get_edge(&batch, id)?,
///     EdgeRecord { id, source: a, target: b, edge_type: knows }
/// );
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EdgeRecord {
    /// Edge identifier.
    pub id: EdgeId,
    /// Source node.
    pub source: NodeId,
    /// Target node.
    pub target: NodeId,
    /// Relationship type.
    pub edge_type: TypeId,
}

/// Decoded record value.
pub(crate) enum Stored<T> {
    Live(T),
    Deleted,
}

pub(crate) fn encode_node() -> Vec<u8> {
    vec![VERSION, LIVE]
}

pub(crate) fn encode_edge(record: &EdgeRecord) -> Vec<u8> {
    let mut out = Vec::with_capacity(EDGE_LEN);
    out.extend_from_slice(&[VERSION, LIVE]);
    out.extend_from_slice(record.source.as_bytes());
    out.extend_from_slice(record.target.as_bytes());
    out.extend_from_slice(record.edge_type.as_bytes());
    out
}

pub(crate) fn encode_tombstone() -> Vec<u8> {
    vec![VERSION, DELETED]
}

pub(crate) fn decode_node(id: NodeId, bytes: &[u8]) -> Result<Stored<NodeRecord>, GraphError> {
    match header(bytes)? {
        DELETED if bytes.len() == HEADER => Ok(Stored::Deleted),
        LIVE if bytes.len() == HEADER => Ok(Stored::Live(NodeRecord { id })),
        _ => Err(GraphError::Corrupt),
    }
}

pub(crate) fn decode_edge(id: EdgeId, bytes: &[u8]) -> Result<Stored<EdgeRecord>, GraphError> {
    match header(bytes)? {
        DELETED if bytes.len() == HEADER => Ok(Stored::Deleted),
        LIVE if bytes.len() == EDGE_LEN => Ok(Stored::Live(EdgeRecord {
            id,
            source: id_at(bytes, HEADER)?,
            target: id_at(bytes, HEADER + ID_LEN)?,
            edge_type: id_at(bytes, HEADER + 2 * ID_LEN)?,
        })),
        _ => Err(GraphError::Corrupt),
    }
}

fn header(bytes: &[u8]) -> Result<u8, GraphError> {
    match bytes {
        [VERSION, state, ..] => Ok(*state),
        _ => Err(GraphError::Corrupt),
    }
}

#[cfg(test)]
mod tests {
    use pizzlydb_core::{EdgeId, NodeId, TypeId};

    use super::{
        EdgeRecord, Stored, decode_edge, decode_node, encode_edge, encode_node, encode_tombstone,
    };
    use crate::error::GraphError;

    fn edge() -> EdgeRecord {
        EdgeRecord {
            id: EdgeId::from_u128(4).unwrap(),
            source: NodeId::from_u128(1).unwrap(),
            target: NodeId::from_u128(2).unwrap(),
            edge_type: TypeId::from_u128(3).unwrap(),
        }
    }

    #[test]
    fn records_round_trip() {
        let node = NodeId::from_u128(1).unwrap();
        assert!(matches!(
            decode_node(node, &encode_node()),
            Ok(Stored::Live(record)) if record.id == node
        ));
        assert!(matches!(
            decode_node(node, &encode_tombstone()),
            Ok(Stored::Deleted)
        ));

        let record = edge();
        assert!(matches!(
            decode_edge(record.id, &encode_edge(&record)),
            Ok(Stored::Live(decoded)) if decoded == record
        ));
        assert!(matches!(
            decode_edge(record.id, &encode_tombstone()),
            Ok(Stored::Deleted)
        ));
    }

    #[test]
    fn malformed_records_are_corrupt() {
        let node = NodeId::from_u128(1).unwrap();
        let record = edge();
        let mut zero_endpoint = encode_edge(&record);
        zero_endpoint[2..18].fill(0);
        let cases: [&[u8]; 6] = [&[], &[1], &[0, 1], &[2, 1], &[1, 0], &[1, 1, 0]];
        for bytes in cases {
            assert!(matches!(decode_node(node, bytes), Err(GraphError::Corrupt)));
        }
        assert!(matches!(
            decode_edge(record.id, &encode_node()),
            Err(GraphError::Corrupt)
        ));
        assert!(matches!(
            decode_edge(record.id, &zero_endpoint),
            Err(GraphError::Corrupt)
        ));
        assert!(matches!(
            decode_node(node, &encode_edge(&record)),
            Err(GraphError::Corrupt)
        ));
    }
}
