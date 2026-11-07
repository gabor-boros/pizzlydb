//! Outgoing and incoming adjacency keys.
//!
//! Every live edge has one outgoing key on its source and one incoming key on
//! its target, staged in the same batch as the edge record. A key is
//! `node || direction || type || neighbor || edge`, so a prefix scan returns
//! one direction of one node in type, neighbor, and edge order. A self-loop
//! has both keys on the same node. A node with no edges has no keys.

use pizzlydb_core::{EdgeId, NamespaceId, NodeId, TypeId};
use pizzlydb_storage::Vfs;

use crate::access::{KeyRead, PrefixScan, id_at};
use crate::batch::GraphTxn;
use crate::error::GraphError;
use crate::keys::{self, ADJACENCY_EDGE, ADJACENCY_LEN, ADJACENCY_NEIGHBOR, ADJACENCY_TYPE};
use crate::record::EdgeRecord;
use crate::record_store::GraphStore;

/// Edge direction relative to the scanned node.
///
/// # Examples
///
/// ```
/// use pizzlydb_graph::Direction;
///
/// assert_ne!(Direction::Out, Direction::In);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Edges whose source is the scanned node.
    Out,
    /// Edges whose target is the scanned node.
    In,
}

impl Direction {
    pub(crate) const fn byte(self) -> u8 {
        match self {
            Self::Out => 1,
            Self::In => 2,
        }
    }
}

/// One adjacency entry.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use std::path::Path;
///
/// use pizzlydb_core::{NamespaceId, TypeId};
/// use pizzlydb_graph::{GraphTxn, Direction, GraphBatch, GraphStore, Neighbor};
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
/// let edge = graph.create_edge(&mut batch, a, b, knows)?;
///
/// let incoming = graph.neighbors(&batch, b, Direction::In)?.collect::<Result<Vec<_>, _>>()?;
/// assert_eq!(incoming, vec![Neighbor { edge, edge_type: knows, neighbor: a }]);
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Neighbor {
    /// The connecting edge.
    pub edge: EdgeId,
    /// Relationship type of the edge.
    pub edge_type: TypeId,
    /// The other endpoint: the target for [`Direction::Out`], the source for
    /// [`Direction::In`].
    pub neighbor: NodeId,
}

/// Adjacency entries of one node in one direction.
///
/// Returned by [`GraphStore::neighbors`] and
/// [`GraphStore::neighbors_of_type`]. It borrows the read view, reads only
/// adjacency keys, and yields entries in type, neighbor, and edge order. A
/// read error ends the scan after it is yielded.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use std::path::Path;
///
/// use pizzlydb_core::{NamespaceId, TypeId};
/// use pizzlydb_graph::{Direction, GraphBatch, GraphStore};
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
/// graph.create_edge(&mut batch, a, b, knows)?;
/// graph.create_edge(&mut batch, a, b, knows)?;
/// batch.commit()?;
///
/// let view = storage.snapshot()?;
/// let targets = graph
///     .neighbors(&view, a, Direction::Out)?
///     .map(|entry| entry.map(|n| n.neighbor))
///     .collect::<Result<Vec<_>, _>>()?;
/// assert_eq!(targets, vec![b, b]);
/// # Ok(())
/// # }
/// ```
#[must_use = "a scan must be driven to read keys"]
pub struct Neighbors<'r, R: KeyRead + 'r> {
    scan: PrefixScan<'r, R>,
}

impl<'r, R: KeyRead + 'r> Iterator for Neighbors<'r, R> {
    type Item = Result<Neighbor, GraphError>;

    fn next(&mut self) -> Option<Self::Item> {
        let item = self.scan.next()?;
        Some(item.and_then(|(key, _)| {
            if key.len() != ADJACENCY_LEN {
                return Err(GraphError::Corrupt);
            }
            Ok(Neighbor {
                edge: id_at(&key, ADJACENCY_EDGE)?,
                edge_type: id_at(&key, ADJACENCY_TYPE)?,
                neighbor: id_at(&key, ADJACENCY_NEIGHBOR)?,
            })
        }))
    }
}

impl GraphStore {
    /// Adjacency entries of `node` in `direction`.
    ///
    /// The scan reads no node, edge, or property keys. A missing or deleted
    /// node has no entries.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Storage`] when the scan cannot start. The
    /// iterator yields read errors and [`GraphError::Corrupt`] for malformed
    /// keys.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_core::{NamespaceId, TypeId};
    /// use pizzlydb_graph::{Direction, GraphBatch, GraphStore};
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
    /// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
    ///
    /// let mut batch = GraphBatch::begin(&storage)?;
    /// let knows: TypeId = graph.intern(&mut batch, "KNOWS")?;
    /// let a = graph.create_node(&mut batch)?;
    /// let edge = graph.create_edge(&mut batch, a, a, knows)?;
    ///
    /// let out = graph.neighbors(&batch, a, Direction::Out)?.collect::<Result<Vec<_>, _>>()?;
    /// let incoming = graph.neighbors(&batch, a, Direction::In)?.collect::<Result<Vec<_>, _>>()?;
    /// assert_eq!(out, incoming);
    /// assert_eq!(out[0].edge, edge);
    /// # Ok(())
    /// # }
    /// ```
    pub fn neighbors<'r, R: KeyRead>(
        &self,
        read: &'r R,
        node: NodeId,
        direction: Direction,
    ) -> Result<Neighbors<'r, R>, GraphError> {
        let prefix = keys::adjacency_toward(self.namespace(), node, direction.byte(), None);
        Ok(Neighbors {
            scan: PrefixScan::new(read, prefix)?,
        })
    }

    /// Adjacency entries of `node` in `direction` with relationship type
    /// `edge_type`.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::neighbors`].
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_core::{NamespaceId, TypeId};
    /// use pizzlydb_graph::{Direction, GraphBatch, GraphStore};
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
    /// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
    ///
    /// let mut batch = GraphBatch::begin(&storage)?;
    /// let knows: TypeId = graph.intern(&mut batch, "KNOWS")?;
    /// let likes: TypeId = graph.intern(&mut batch, "LIKES")?;
    /// let a = graph.create_node(&mut batch)?;
    /// let b = graph.create_node(&mut batch)?;
    /// graph.create_edge(&mut batch, a, b, knows)?;
    /// let liked = graph.create_edge(&mut batch, a, b, likes)?;
    ///
    /// let edges = graph
    ///     .neighbors_of_type(&batch, a, Direction::Out, likes)?
    ///     .map(|entry| entry.map(|n| n.edge))
    ///     .collect::<Result<Vec<_>, _>>()?;
    /// assert_eq!(edges, vec![liked]);
    /// # Ok(())
    /// # }
    /// ```
    pub fn neighbors_of_type<'r, R: KeyRead>(
        &self,
        read: &'r R,
        node: NodeId,
        direction: Direction,
        edge_type: TypeId,
    ) -> Result<Neighbors<'r, R>, GraphError> {
        let prefix =
            keys::adjacency_toward(self.namespace(), node, direction.byte(), Some(edge_type));
        Ok(Neighbors {
            scan: PrefixScan::new(read, prefix)?,
        })
    }
}

/// Stage the outgoing and incoming keys of `edge`.
pub(crate) fn link<V: Vfs>(
    batch: &mut GraphTxn<V>,
    namespace: NamespaceId,
    edge: &EdgeRecord,
) -> Result<(), GraphError> {
    let [out, incoming] = entries(namespace, edge);
    batch.put(out, Vec::new())?;
    batch.put(incoming, Vec::new())
}

/// Stage removal of the outgoing and incoming keys of `edge`.
pub(crate) fn unlink<V: Vfs>(
    batch: &mut GraphTxn<V>,
    namespace: NamespaceId,
    edge: &EdgeRecord,
) -> Result<(), GraphError> {
    let [out, incoming] = entries(namespace, edge);
    batch.delete(out)?;
    batch.delete(incoming)
}

fn entries(namespace: NamespaceId, edge: &EdgeRecord) -> [Vec<u8>; 2] {
    [
        keys::adjacency(
            namespace,
            edge.source,
            Direction::Out.byte(),
            edge.edge_type,
            edge.target,
            edge.id,
        ),
        keys::adjacency(
            namespace,
            edge.target,
            Direction::In.byte(),
            edge.edge_type,
            edge.source,
            edge.id,
        ),
    ]
}
