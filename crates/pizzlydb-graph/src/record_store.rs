//! Node and edge create, read, and delete.

use pizzlydb_core::{EdgeId, NamespaceId, NodeId, TypeId};
use pizzlydb_storage::Vfs;

use crate::access::KeyRead;
use crate::batch::GraphBatch;
use crate::error::GraphError;
use crate::keys::{self, id_kind};
use crate::property::Owner;
use crate::record::{EdgeRecord, NodeRecord, encode_edge, encode_node, encode_tombstone};
use crate::{adjacency, catalog, constraint, directory, index, property};

/// Graph operations scoped to one namespace.
///
/// A `GraphStore` holds no state besides its namespace. Read methods take any
/// [`KeyRead`]: a committed [`ReadView`](pizzlydb_storage::ReadView) or an
/// open [`GraphBatch`]. Write methods stage keys on a [`GraphBatch`], and a
/// method that returns an error stages nothing.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use std::path::Path;
///
/// use pizzlydb_core::NamespaceId;
/// use pizzlydb_graph::{GraphBatch, GraphError, GraphStore};
/// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
///
/// let vfs = MemoryVfs::new();
/// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
/// let left = GraphStore::new(NamespaceId::from_u128(1)?);
/// let right = GraphStore::new(NamespaceId::from_u128(2)?);
///
/// let mut batch = GraphBatch::begin(&storage)?;
/// let node = left.create_node(&mut batch)?;
/// batch.commit()?;
///
/// let view = storage.snapshot()?;
/// assert!(left.get_node(&view, node).is_ok());
/// assert!(matches!(right.get_node(&view, node), Err(GraphError::NotFound)));
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GraphStore {
    namespace: NamespaceId,
}

impl GraphStore {
    /// Operations on the keys of `namespace`.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use pizzlydb_core::NamespaceId;
    /// use pizzlydb_graph::GraphStore;
    ///
    /// let namespace = NamespaceId::from_u128(7)?;
    /// assert_eq!(GraphStore::new(namespace).namespace(), namespace);
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub const fn new(namespace: NamespaceId) -> Self {
        Self { namespace }
    }

    /// Namespace of every key this store reads and writes.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use pizzlydb_core::NamespaceId;
    /// use pizzlydb_graph::GraphStore;
    ///
    /// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
    /// assert_eq!(graph.namespace().to_u128(), 1);
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub const fn namespace(&self) -> NamespaceId {
        self.namespace
    }

    /// Create a node with a new identifier.
    ///
    /// Identifiers increase from 1 and are never reused, including after a
    /// delete.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::IdExhausted`] when the node counter is at
    /// `u128::MAX`, and [`GraphError::Storage`] or [`GraphError::Corrupt`] on a
    /// failed read.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_core::NamespaceId;
    /// use pizzlydb_graph::{GraphBatch, GraphStore};
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
    /// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
    ///
    /// let mut batch = GraphBatch::begin(&storage)?;
    /// let first = graph.create_node(&mut batch)?;
    /// let second = graph.create_node(&mut batch)?;
    /// assert_eq!(first.to_u128(), 1);
    /// assert_eq!(second.to_u128(), 2);
    /// # Ok(())
    /// # }
    /// ```
    pub fn create_node<V: Vfs>(&self, batch: &mut GraphBatch<'_, V>) -> Result<NodeId, GraphError> {
        batch.atomic(|batch| {
            let id = directory::allocate(batch, self.namespace, id_kind::NODE)?;
            batch.put(keys::node(self.namespace, id), encode_node())?;
            Ok(id)
        })
    }

    /// Read a live node.
    ///
    /// This is one point read.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::NotFound`] for an identifier that was never
    /// created in this namespace, [`GraphError::Deleted`] for a deleted node,
    /// and [`GraphError::Storage`] or [`GraphError::Corrupt`] on a failed read.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_core::{NamespaceId, NodeId};
    /// use pizzlydb_graph::{GraphBatch, GraphError, GraphStore};
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
    /// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
    ///
    /// let mut batch = GraphBatch::begin(&storage)?;
    /// let node = graph.create_node(&mut batch)?;
    /// assert_eq!(graph.get_node(&batch, node)?.id, node);
    /// let unknown = NodeId::from_u128(99)?;
    /// assert!(matches!(graph.get_node(&batch, unknown), Err(GraphError::NotFound)));
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_node<R: KeyRead>(&self, read: &R, id: NodeId) -> Result<NodeRecord, GraphError> {
        directory::load_node(read, self.namespace, id)
    }

    /// Delete a node with no incident edges.
    ///
    /// The delete removes the node's labels, properties, label membership,
    /// and index entries, and leaves a tombstone. Later reads of the
    /// identifier return [`GraphError::Deleted`].
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::EndpointInUse`] when an edge still starts or ends
    /// at the node, [`GraphError::NotFound`] or [`GraphError::Deleted`] when
    /// the node is not live, and [`GraphError::Storage`] or
    /// [`GraphError::Corrupt`] on a failed read.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_core::{NamespaceId, TypeId};
    /// use pizzlydb_graph::{GraphBatch, GraphError, GraphStore};
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
    /// assert!(matches!(graph.delete_node(&mut batch, a), Err(GraphError::EndpointInUse)));
    /// graph.delete_edge(&mut batch, edge)?;
    /// graph.delete_node(&mut batch, a)?;
    /// assert!(matches!(graph.get_node(&batch, a), Err(GraphError::Deleted)));
    /// # Ok(())
    /// # }
    /// ```
    pub fn delete_node<V: Vfs>(
        &self,
        batch: &mut GraphBatch<'_, V>,
        id: NodeId,
    ) -> Result<(), GraphError> {
        batch.atomic(|batch| {
            directory::load_node(batch, self.namespace, id)?;
            constraint::ensure_no_incident_edges(batch, self.namespace, id)?;
            property::clear_all(batch, self.namespace, Owner::Node(id))?;
            catalog::clear_labels(batch, self.namespace, id)?;
            batch.put(keys::node(self.namespace, id), encode_tombstone())
        })
    }

    /// Create a directed edge from `source` to `target`.
    ///
    /// The edge record, both adjacency keys, and the relationship-type
    /// membership key are staged together. Self-loops and parallel edges are
    /// allowed.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::InvalidIdentity`] when `edge_type` was not issued
    /// in this namespace, [`GraphError::EndpointNotLive`] when either endpoint
    /// is missing or deleted, [`GraphError::IdExhausted`] when the edge counter
    /// is at `u128::MAX`, and [`GraphError::Storage`] or
    /// [`GraphError::Corrupt`] on a failed read.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_core::{NamespaceId, NodeId, TypeId};
    /// use pizzlydb_graph::{GraphBatch, GraphError, GraphStore};
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
    /// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
    ///
    /// let mut batch = GraphBatch::begin(&storage)?;
    /// let knows: TypeId = graph.intern(&mut batch, "KNOWS")?;
    /// let a = graph.create_node(&mut batch)?;
    /// let missing = NodeId::from_u128(99)?;
    ///
    /// let staged = batch.len();
    /// assert!(matches!(
    ///     graph.create_edge(&mut batch, a, missing, knows),
    ///     Err(GraphError::EndpointNotLive)
    /// ));
    /// assert_eq!(batch.len(), staged);
    ///
    /// let edge = graph.create_edge(&mut batch, a, a, knows)?;
    /// assert_eq!(graph.get_edge(&batch, edge)?.target, a);
    /// # Ok(())
    /// # }
    /// ```
    pub fn create_edge<V: Vfs>(
        &self,
        batch: &mut GraphBatch<'_, V>,
        source: NodeId,
        target: NodeId,
        edge_type: TypeId,
    ) -> Result<EdgeId, GraphError> {
        batch.atomic(|batch| {
            catalog::ensure_issued(batch, self.namespace, edge_type)?;
            constraint::ensure_endpoints_live(batch, self.namespace, source, target)?;
            let id = directory::allocate(batch, self.namespace, id_kind::EDGE)?;
            let record = EdgeRecord {
                id,
                source,
                target,
                edge_type,
            };
            batch.put(keys::edge(self.namespace, id), encode_edge(&record))?;
            adjacency::link(batch, self.namespace, &record)?;
            index::note_edge(batch, self.namespace, &record)?;
            Ok(id)
        })
    }

    /// Read a live edge.
    ///
    /// This is one point read.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::NotFound`] for an identifier that was never
    /// created in this namespace, [`GraphError::Deleted`] for a deleted edge,
    /// and [`GraphError::Storage`] or [`GraphError::Corrupt`] on a failed read.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_core::{NamespaceId, TypeId};
    /// use pizzlydb_graph::{GraphBatch, GraphStore};
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
    /// batch.commit()?;
    ///
    /// let record = graph.get_edge(&storage.snapshot()?, edge)?;
    /// assert_eq!((record.source, record.target, record.edge_type), (a, b, knows));
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_edge<R: KeyRead>(&self, read: &R, id: EdgeId) -> Result<EdgeRecord, GraphError> {
        directory::load_edge(read, self.namespace, id)
    }

    /// Delete an edge.
    ///
    /// The delete removes both adjacency keys, the edge's properties, its
    /// relationship-type membership, and its index entries, and leaves a
    /// tombstone. Later reads of the identifier return
    /// [`GraphError::Deleted`].
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::NotFound`] or [`GraphError::Deleted`] when the
    /// edge is not live, and [`GraphError::Storage`] or
    /// [`GraphError::Corrupt`] on a failed read.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_core::{NamespaceId, TypeId};
    /// use pizzlydb_graph::{Direction, GraphBatch, GraphError, GraphStore};
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
    /// graph.delete_edge(&mut batch, edge)?;
    ///
    /// assert!(matches!(graph.get_edge(&batch, edge), Err(GraphError::Deleted)));
    /// assert_eq!(graph.neighbors(&batch, a, Direction::Out)?.count(), 0);
    /// # Ok(())
    /// # }
    /// ```
    pub fn delete_edge<V: Vfs>(
        &self,
        batch: &mut GraphBatch<'_, V>,
        id: EdgeId,
    ) -> Result<(), GraphError> {
        batch.atomic(|batch| {
            let record = directory::load_edge(batch, self.namespace, id)?;
            adjacency::unlink(batch, self.namespace, &record)?;
            property::clear_all(batch, self.namespace, Owner::Edge(id))?;
            index::forget_edge(batch, self.namespace, &record)?;
            batch.put(keys::edge(self.namespace, id), encode_tombstone())
        })
    }
}
