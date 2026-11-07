//! Names of labels, relationship types, and property keys, and node labels.
//!
//! A name is 1 to [`MAX_NAME_LEN`] bytes of UTF-8 with no NUL byte. Names
//! compare as exact bytes, with no normalization and no case folding. Labels,
//! relationship types, and property keys have separate name spaces, so one
//! name can identify a label and a relationship type with unrelated
//! identifiers.
//!
//! Each name has a name-to-id key and an id-to-name key, so both lookups are
//! one point read. A node label is a node-to-label key plus a label-to-node
//! membership key, staged together with the matching index entries.

use std::collections::BTreeSet;

use pizzlydb_core::{ID_LEN, Identifier, LabelId, NamespaceId, NodeId, PropertyKeyId, TypeId};
use pizzlydb_storage::Vfs;

use crate::access::{IdScan, KeyRead, id_at};
use crate::batch::GraphTxn;
use crate::directory;
use crate::error::GraphError;
use crate::index;
use crate::keys::{self, id_kind};
use crate::record_store::GraphStore;

/// Maximum name length in bytes.
pub const MAX_NAME_LEN: usize = 256;

mod sealed {
    pub trait Sealed {
        const ID_KIND: u8;
    }
}

/// Identifier with a name in the catalog: [`LabelId`], [`TypeId`], or
/// [`PropertyKeyId`].
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use std::path::Path;
///
/// use pizzlydb_core::{LabelId, NamespaceId, TypeId};
/// use pizzlydb_graph::{GraphTxn, CatalogId, GraphBatch, GraphStore, KeyRead};
/// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
///
/// fn name_of<I: CatalogId, R: KeyRead>(
///     graph: &GraphStore,
///     read: &R,
///     id: I,
/// ) -> Result<String, pizzlydb_graph::GraphError> {
///     graph.name(read, id)
/// }
///
/// let vfs = MemoryVfs::new();
/// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
/// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
///
/// let mut batch = GraphBatch::begin(&storage)?;
/// let label: LabelId = graph.intern(&mut batch, "Thing")?;
/// let edge_type: TypeId = graph.intern(&mut batch, "Thing")?;
/// assert_eq!(name_of(&graph, &batch, label)?, "Thing");
/// assert_eq!(name_of(&graph, &batch, edge_type)?, "Thing");
/// # Ok(())
/// # }
/// ```
pub trait CatalogId: Identifier + sealed::Sealed {}

impl sealed::Sealed for LabelId {
    const ID_KIND: u8 = id_kind::LABEL;
}
impl sealed::Sealed for TypeId {
    const ID_KIND: u8 = id_kind::TYPE;
}
impl sealed::Sealed for PropertyKeyId {
    const ID_KIND: u8 = id_kind::PROPERTY_KEY;
}
impl CatalogId for LabelId {}
impl CatalogId for TypeId {}
impl CatalogId for PropertyKeyId {}

impl GraphStore {
    /// Return the identifier of `name`, issuing one when the name is new.
    ///
    /// Interning an existing name stages nothing.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::InvalidName`] for an empty name, a name longer
    /// than [`MAX_NAME_LEN`] bytes, a name with a NUL byte, or invalid UTF-8;
    /// [`GraphError::IdExhausted`] when the counter is at `u128::MAX`; and
    /// [`GraphError::Storage`] or [`GraphError::Corrupt`] on a failed read.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_core::{LabelId, NamespaceId};
    /// use pizzlydb_graph::{GraphBatch, GraphError, GraphStore};
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
    /// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
    ///
    /// let mut batch = GraphBatch::begin(&storage)?;
    /// let upper: LabelId = graph.intern(&mut batch, "A")?;
    /// let lower: LabelId = graph.intern(&mut batch, "a")?;
    /// assert_ne!(upper, lower);
    /// assert_eq!(graph.intern::<LabelId, _>(&mut batch, "A")?, upper);
    /// assert!(matches!(
    ///     graph.intern::<LabelId, _>(&mut batch, b"bad\0name"),
    ///     Err(GraphError::InvalidName)
    /// ));
    /// # Ok(())
    /// # }
    /// ```
    pub fn intern<I: CatalogId, V: Vfs>(
        &self,
        batch: &mut GraphTxn<V>,
        name: impl AsRef<[u8]>,
    ) -> Result<I, GraphError> {
        let name = valid_name(name.as_ref())?;
        if let Some(id) = self.lookup(batch, name)? {
            return Ok(id);
        }

        batch.atomic(|batch| {
            let id: I = directory::allocate(batch, self.namespace(), I::ID_KIND)?;
            self.put_name(batch, id, name)?;
            batch.record(pizzlydb_core::GraphMutation::CatalogCreate {
                namespace: self.namespace(),
                kind: I::ID_KIND,
                id: *id.as_bytes(),
                name: name.to_vec(),
            });
            Ok(id)
        })
    }

    /// Identifier of `name`, or [`None`] when the name was never interned.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::InvalidName`] for a name that
    /// [`Self::intern`] rejects, and [`GraphError::Storage`] or
    /// [`GraphError::Corrupt`] on a failed read.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_core::{LabelId, NamespaceId, TypeId};
    /// use pizzlydb_graph::{GraphBatch, GraphStore};
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
    /// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
    ///
    /// let mut batch = GraphBatch::begin(&storage)?;
    /// let person: LabelId = graph.intern(&mut batch, "Person")?;
    /// assert_eq!(graph.lookup(&batch, "Person")?, Some(person));
    /// assert_eq!(graph.lookup::<TypeId, _>(&batch, "Person")?, None);
    /// # Ok(())
    /// # }
    /// ```
    pub fn lookup<I: CatalogId, R: KeyRead>(
        &self,
        read: &R,
        name: impl AsRef<[u8]>,
    ) -> Result<Option<I>, GraphError> {
        let name = valid_name(name.as_ref())?;
        match read.get(&keys::name(self.namespace(), I::ID_KIND, name))? {
            None => Ok(None),
            Some(bytes) if bytes.len() == ID_LEN => id_at(&bytes, 0).map(Some),
            Some(_) => Err(GraphError::Corrupt),
        }
    }

    /// Name of an issued identifier.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::InvalidIdentity`] when `id` was not issued in this
    /// namespace, and [`GraphError::Storage`] or [`GraphError::Corrupt`] on a
    /// failed read.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_core::{NamespaceId, PropertyKeyId};
    /// use pizzlydb_graph::{GraphBatch, GraphError, GraphStore};
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
    /// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
    ///
    /// let mut batch = GraphBatch::begin(&storage)?;
    /// let age: PropertyKeyId = graph.intern(&mut batch, "age")?;
    /// assert_eq!(graph.name(&batch, age)?, "age");
    /// let unknown = PropertyKeyId::from_u128(42)?;
    /// assert!(matches!(graph.name(&batch, unknown), Err(GraphError::InvalidIdentity)));
    /// # Ok(())
    /// # }
    /// ```
    pub fn name<I: CatalogId, R: KeyRead>(&self, read: &R, id: I) -> Result<String, GraphError> {
        let bytes = read
            .get(&keys::name_by_id(
                self.namespace(),
                I::ID_KIND,
                id.as_bytes(),
            ))?
            .ok_or(GraphError::InvalidIdentity)?;

        String::from_utf8(bytes).map_err(|_| GraphError::Corrupt)
    }

    /// Give an issued identifier a new name.
    ///
    /// Renaming to the current name stages nothing. The old name becomes free.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::InvalidName`] for a name that [`Self::intern`]
    /// rejects, [`GraphError::InvalidIdentity`] when `id` was not issued in
    /// this namespace, [`GraphError::NameConflict`] when another identifier of
    /// the same kind has the name, and [`GraphError::Storage`] or
    /// [`GraphError::Corrupt`] on a failed read.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_core::{LabelId, NamespaceId};
    /// use pizzlydb_graph::{GraphBatch, GraphError, GraphStore};
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
    /// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
    ///
    /// let mut batch = GraphBatch::begin(&storage)?;
    /// let person: LabelId = graph.intern(&mut batch, "Person")?;
    /// let robot: LabelId = graph.intern(&mut batch, "Robot")?;
    ///
    /// graph.rename(&mut batch, person, "Human")?;
    /// assert_eq!(graph.lookup(&batch, "Human")?, Some(person));
    /// assert_eq!(graph.lookup::<LabelId, _>(&batch, "Person")?, None);
    /// assert!(matches!(
    ///     graph.rename(&mut batch, robot, "Human"),
    ///     Err(GraphError::NameConflict)
    /// ));
    /// # Ok(())
    /// # }
    /// ```
    pub fn rename<I: CatalogId, V: Vfs>(
        &self,
        batch: &mut GraphTxn<V>,
        id: I,
        name: impl AsRef<[u8]>,
    ) -> Result<(), GraphError> {
        let name = valid_name(name.as_ref())?;
        let current = self.name(batch, id)?;
        if current.as_bytes() == name {
            return Ok(());
        }

        if self.lookup::<I, _>(batch, name)?.is_some() {
            return Err(GraphError::NameConflict);
        }

        batch.atomic(|batch| {
            batch.delete(keys::name(self.namespace(), I::ID_KIND, current.as_bytes()))?;
            self.put_name(batch, id, name)?;
            batch.record(pizzlydb_core::GraphMutation::CatalogRename {
                namespace: self.namespace(),
                kind: I::ID_KIND,
                id: *id.as_bytes(),
                previous: current.into_bytes(),
                name: name.to_vec(),
            });
            Ok(())
        })
    }

    /// Give a node a label. Adding a label the node already has stages
    /// nothing.
    ///
    /// The node-to-label key, the label membership key, and entries for every
    /// index scoped to the label are staged together.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::NotFound`] or [`GraphError::Deleted`] when the
    /// node is not live, [`GraphError::InvalidIdentity`] when `label` was not
    /// issued in this namespace, [`GraphError::UniqueConflict`] when a unique
    /// index on the label already holds one of the node's values, and
    /// [`GraphError::Storage`] or [`GraphError::Corrupt`] on a failed read.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_core::{LabelId, NamespaceId};
    /// use pizzlydb_graph::{GraphBatch, GraphStore};
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
    /// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
    ///
    /// let mut batch = GraphBatch::begin(&storage)?;
    /// let person: LabelId = graph.intern(&mut batch, "Person")?;
    /// let node = graph.create_node(&mut batch)?;
    /// graph.add_label(&mut batch, node, person)?;
    /// assert!(graph.has_label(&batch, node, person)?);
    /// # Ok(())
    /// # }
    /// ```
    pub fn add_label<V: Vfs>(
        &self,
        batch: &mut GraphTxn<V>,
        node: NodeId,
        label: LabelId,
    ) -> Result<(), GraphError> {
        directory::load_node(batch, self.namespace(), node)?;
        ensure_issued(batch, self.namespace(), label)?;
        if self.has_label(batch, node, label)? {
            return Ok(());
        }

        batch.atomic(|batch| change_labels(batch, self.namespace(), node, &[], &[label]))
    }

    /// Remove a label from a node. Removing a label the node does not have
    /// stages nothing.
    ///
    /// The node-to-label key, the label membership key, and entries for every
    /// index scoped to the label are removed together.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::NotFound`] or [`GraphError::Deleted`] when the
    /// node is not live, and [`GraphError::Storage`] or
    /// [`GraphError::Corrupt`] on a failed read.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_core::{LabelId, NamespaceId};
    /// use pizzlydb_graph::{GraphBatch, GraphStore};
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
    /// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
    ///
    /// let mut batch = GraphBatch::begin(&storage)?;
    /// let person: LabelId = graph.intern(&mut batch, "Person")?;
    /// let node = graph.create_node(&mut batch)?;
    /// graph.add_label(&mut batch, node, person)?;
    /// graph.remove_label(&mut batch, node, person)?;
    /// assert!(!graph.has_label(&batch, node, person)?);
    /// # Ok(())
    /// # }
    /// ```
    pub fn remove_label<V: Vfs>(
        &self,
        batch: &mut GraphTxn<V>,
        node: NodeId,
        label: LabelId,
    ) -> Result<(), GraphError> {
        directory::load_node(batch, self.namespace(), node)?;
        if !self.has_label(batch, node, label)? {
            return Ok(());
        }

        batch.atomic(|batch| change_labels(batch, self.namespace(), node, &[label], &[]))
    }

    /// Replace the labels of a node with `labels`. Duplicates are ignored.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::add_label`].
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_core::{LabelId, NamespaceId};
    /// use pizzlydb_graph::{GraphBatch, GraphStore};
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
    /// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
    ///
    /// let mut batch = GraphBatch::begin(&storage)?;
    /// let a: LabelId = graph.intern(&mut batch, "A")?;
    /// let b: LabelId = graph.intern(&mut batch, "B")?;
    /// let c: LabelId = graph.intern(&mut batch, "C")?;
    /// let node = graph.create_node(&mut batch)?;
    ///
    /// graph.set_labels(&mut batch, node, &[a, b])?;
    /// graph.set_labels(&mut batch, node, &[c, b, c])?;
    /// let labels = graph.labels(&batch, node)?.collect::<Result<Vec<_>, _>>()?;
    /// assert_eq!(labels, vec![b, c]);
    /// # Ok(())
    /// # }
    /// ```
    pub fn set_labels<V: Vfs>(
        &self,
        batch: &mut GraphTxn<V>,
        node: NodeId,
        labels: &[LabelId],
    ) -> Result<(), GraphError> {
        directory::load_node(batch, self.namespace(), node)?;
        let wanted: BTreeSet<LabelId> = labels.iter().copied().collect();
        for &label in &wanted {
            ensure_issued(batch, self.namespace(), label)?;
        }

        let current = current_labels(batch, self.namespace(), node)?;
        let removed: Vec<LabelId> = current.difference(&wanted).copied().collect();
        let added: Vec<LabelId> = wanted.difference(&current).copied().collect();
        if removed.is_empty() && added.is_empty() {
            return Ok(());
        }

        batch.atomic(|batch| change_labels(batch, self.namespace(), node, &removed, &added))
    }

    /// `true` when `node` has `label`.
    ///
    /// This is one point read. A missing or deleted node has no labels.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Storage`] on a failed read.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_core::{LabelId, NamespaceId};
    /// use pizzlydb_graph::{GraphBatch, GraphStore};
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
    /// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
    ///
    /// let mut batch = GraphBatch::begin(&storage)?;
    /// let person: LabelId = graph.intern(&mut batch, "Person")?;
    /// let node = graph.create_node(&mut batch)?;
    /// assert!(!graph.has_label(&batch, node, person)?);
    /// # Ok(())
    /// # }
    /// ```
    pub fn has_label<R: KeyRead>(
        &self,
        read: &R,
        node: NodeId,
        label: LabelId,
    ) -> Result<bool, GraphError> {
        Ok(read
            .get(&keys::node_label(self.namespace(), node, label))?
            .is_some())
    }

    /// Labels of `node` in identifier order.
    ///
    /// A missing or deleted node has no labels.
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
    /// use pizzlydb_core::{LabelId, NamespaceId};
    /// use pizzlydb_graph::{GraphBatch, GraphStore};
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
    /// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
    ///
    /// let mut batch = GraphBatch::begin(&storage)?;
    /// let a: LabelId = graph.intern(&mut batch, "A")?;
    /// let b: LabelId = graph.intern(&mut batch, "B")?;
    /// let node = graph.create_node(&mut batch)?;
    ///
    /// graph.add_label(&mut batch, node, b)?;
    /// graph.add_label(&mut batch, node, a)?;
    ///
    /// let labels = graph.labels(&batch, node)?.collect::<Result<Vec<_>, _>>()?;
    /// assert_eq!(labels, vec![a, b]);
    /// # Ok(())
    /// # }
    /// ```
    pub fn labels<'r, R: KeyRead>(
        &self,
        read: &'r R,
        node: NodeId,
    ) -> Result<IdScan<'r, R, LabelId>, GraphError> {
        IdScan::new(read, keys::node_labels(self.namespace(), node))
    }

    fn put_name<I: CatalogId, V: Vfs>(
        &self,
        batch: &mut GraphTxn<V>,
        id: I,
        name: &[u8],
    ) -> Result<(), GraphError> {
        let namespace = self.namespace();
        batch.put(
            keys::name(namespace, I::ID_KIND, name),
            id.as_bytes().to_vec(),
        )?;
        batch.put(
            keys::name_by_id(namespace, I::ID_KIND, id.as_bytes()),
            name.to_vec(),
        )
    }
}

/// `id` must have been issued in `namespace`.
pub(crate) fn ensure_issued<I: CatalogId, R: KeyRead>(
    read: &R,
    namespace: NamespaceId,
    id: I,
) -> Result<(), GraphError> {
    match read.get(&keys::name_by_id(namespace, I::ID_KIND, id.as_bytes()))? {
        Some(_) => Ok(()),
        None => Err(GraphError::InvalidIdentity),
    }
}

/// Remove every label of `node`.
pub(crate) fn clear_labels<V: Vfs>(
    batch: &mut GraphTxn<V>,
    namespace: NamespaceId,
    node: NodeId,
) -> Result<(), GraphError> {
    let current: Vec<LabelId> = current_labels(batch, namespace, node)?
        .into_iter()
        .collect();

    change_labels(batch, namespace, node, &current, &[])
}

fn current_labels<R: KeyRead>(
    read: &R,
    namespace: NamespaceId,
    node: NodeId,
) -> Result<BTreeSet<LabelId>, GraphError> {
    IdScan::new(read, keys::node_labels(namespace, node))?.collect()
}

fn change_labels<V: Vfs>(
    batch: &mut GraphTxn<V>,
    namespace: NamespaceId,
    node: NodeId,
    removed: &[LabelId],
    added: &[LabelId],
) -> Result<(), GraphError> {
    for &label in removed {
        batch.delete(keys::node_label(namespace, node, label))?;
        batch.delete(keys::label_member(namespace, label, node))?;
        batch.record(pizzlydb_core::GraphMutation::LabelRemove {
            namespace,
            node,
            label,
        });
    }

    for &label in added {
        batch.put(keys::node_label(namespace, node, label), Vec::new())?;
        batch.put(keys::label_member(namespace, label, node), Vec::new())?;
        batch.record(pizzlydb_core::GraphMutation::LabelAdd {
            namespace,
            node,
            label,
        });
    }

    index::sync_labels(batch, namespace, node, removed, added)
}

pub(crate) fn valid_name(name: &[u8]) -> Result<&[u8], GraphError> {
    let valid = !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && !name.contains(&0)
        && std::str::from_utf8(name).is_ok();

    if valid {
        Ok(name)
    } else {
        Err(GraphError::InvalidName)
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_NAME_LEN, valid_name};
    use crate::error::GraphError;

    #[test]
    fn name_validation() {
        let longest = vec![b'x'; MAX_NAME_LEN];
        let overlong = vec![b'x'; MAX_NAME_LEN + 1];
        assert!(valid_name(b"Person").is_ok());
        assert!(valid_name("Étoile".as_bytes()).is_ok());
        assert!(valid_name(&longest).is_ok());
        for bad in [&b""[..], b"a\0b", &[0xFF, 0xFE], &overlong] {
            assert!(matches!(valid_name(bad), Err(GraphError::InvalidName)));
        }
    }
}
