//! Secondary indexes and label and relationship-type membership.
//!
//! An index covers one property key within one [`Scope`]. Its entries are
//! keys in the same ordered map as the properties:
//!
//! - property index: `index || value || owner` to an empty value. Every
//!   present value is indexed, including [`Value::Null`]. An absent property
//!   has no entry.
//! - unique index: `index || value` to the owner. [`Value::Null`] has no
//!   entry, so any number of owners may hold null.
//!
//! `value` is the key format of [`encode_value_key`], so entries sort by
//! value type tag (null, bool, int, float, string, bytes) and then by value.
//!
//! Membership keys list the nodes with a label and the edges of a type.
//!
//! Entries change in the same batch as the property, label, or edge change
//! that causes them, so an index read at any snapshot agrees with the
//! properties at that snapshot. A unique check reads through the batch and
//! sees values staged earlier in it. [`GraphStore::create_index`] builds every
//! entry in the batch that defines the index; the index is visible when that
//! batch commits.

use std::ops::Bound;

use pizzlydb_core::{
    EdgeId, ID_LEN, IndexId, LabelId, NamespaceId, NodeId, PropertyKeyId, TypeId, Value,
};
use pizzlydb_storage::Vfs;

use crate::access::{IdScan, KeyRead, PrefixScan, id_at};
use crate::batch::GraphBatch;
use crate::catalog;
use crate::constraint;
use crate::directory;
use crate::error::GraphError;
use crate::keys::{self, id_kind};
use crate::property::{self, Owner, Scope};
use crate::record::EdgeRecord;
use crate::record_store::GraphStore;

const DEFINITION_VERSION: u8 = 1;
const DEFINITION_LEN: usize = 3 + 2 * ID_LEN;

/// Scope, property key, and uniqueness of an index.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use std::path::Path;
///
/// use pizzlydb_core::{LabelId, NamespaceId, PropertyKeyId};
/// use pizzlydb_graph::{GraphBatch, GraphStore, IndexDefinition, Scope};
/// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
///
/// let vfs = MemoryVfs::new();
/// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
/// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
///
/// let mut batch = GraphBatch::begin(&storage)?;
/// let person: LabelId = graph.intern(&mut batch, "Person")?;
/// let email: PropertyKeyId = graph.intern(&mut batch, "email")?;
/// let index = graph.create_index(&mut batch, Scope::Label(person), email, true)?;
/// assert_eq!(
///     graph.index(&batch, index)?,
///     IndexDefinition { scope: Scope::Label(person), key: email, unique: true }
/// );
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct IndexDefinition {
    /// Nodes with a label or edges of a type.
    pub scope: Scope,
    /// Indexed property key.
    pub key: PropertyKeyId,
    /// `true` when two owners in the scope may not hold the same non-null
    /// value.
    pub unique: bool,
}

impl IndexDefinition {
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(DEFINITION_LEN);
        out.extend_from_slice(&[DEFINITION_VERSION, self.scope.kind()]);
        out.extend_from_slice(self.scope.id_bytes());
        out.extend_from_slice(self.key.as_bytes());
        out.push(u8::from(self.unique));
        out
    }

    fn decode(bytes: &[u8]) -> Result<Self, GraphError> {
        match bytes {
            [DEFINITION_VERSION, kind, .., unique @ (0 | 1)] if bytes.len() == DEFINITION_LEN => {
                Ok(Self {
                    scope: Scope::decode(*kind, bytes, 2)?,
                    key: id_at(bytes, 2 + ID_LEN)?,
                    unique: *unique == 1,
                })
            }
            _ => Err(GraphError::Corrupt),
        }
    }
}

/// Owners of index entries in value order, then owner order.
///
/// Returned by [`GraphStore::index_lookup`] and [`GraphStore::index_range`].
/// It borrows the read view and reads only index entry keys. A read error or
/// a malformed entry ends the scan after it is yielded.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use std::path::Path;
///
/// use pizzlydb_core::{LabelId, NamespaceId, PropertyKeyId, Value};
/// use pizzlydb_graph::{GraphBatch, GraphStore, Owner, Scope};
/// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
///
/// let vfs = MemoryVfs::new();
/// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
/// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
///
/// let mut batch = GraphBatch::begin(&storage)?;
/// let person: LabelId = graph.intern(&mut batch, "Person")?;
/// let city: PropertyKeyId = graph.intern(&mut batch, "city")?;
/// let index = graph.create_index(&mut batch, Scope::Label(person), city, false)?;
/// let node = graph.create_node(&mut batch)?;
/// graph.add_label(&mut batch, node, person)?;
/// graph.set_property(&mut batch, node, city, &Value::from("Oslo"))?;
/// batch.commit()?;
///
/// let view = storage.snapshot()?;
/// let hits = graph
///     .index_lookup(&view, index, &Value::from("Oslo"))?
///     .collect::<Result<Vec<_>, _>>()?;
///
/// assert_eq!(hits, vec![Owner::Node(node)]);
/// # Ok(())
/// # }
/// ```
#[must_use = "a scan must be driven to read keys"]
pub struct IndexScan<'r, R: KeyRead + 'r> {
    scan: PrefixScan<'r, R>,
    unique: bool,
    scope: Scope,
    lower: Bound<Vec<u8>>,
    upper: Bound<Vec<u8>>,
}

impl<'r, R: KeyRead + 'r> IndexScan<'r, R> {
    fn split<'k>(&self, key: &'k [u8], value: &[u8]) -> Result<(&'k [u8], Owner), GraphError> {
        let start = self.scan.prefix_len();
        let (encoded, owner) = if self.unique {
            if value.len() != ID_LEN {
                return Err(GraphError::Corrupt);
            }
            (key.get(start..), value)
        } else {
            let end = key.len().saturating_sub(ID_LEN);
            if !value.is_empty() || end <= start {
                return Err(GraphError::Corrupt);
            }
            (key.get(start..end), &key[end..])
        };

        let encoded = encoded
            .filter(|encoded| !encoded.is_empty())
            .ok_or(GraphError::Corrupt)?;

        let owner = match self.scope {
            Scope::Label(_) => Owner::Node(id_at(owner, 0)?),
            Scope::Type(_) => Owner::Edge(id_at(owner, 0)?),
        };

        Ok((encoded, owner))
    }
}

impl<'r, R: KeyRead + 'r> Iterator for IndexScan<'r, R> {
    type Item = Result<Owner, GraphError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let (key, value) = match self.scan.next()? {
                Ok(entry) => entry,
                Err(err) => return Some(Err(err)),
            };

            let (encoded, owner) = match self.split(&key, &value) {
                Ok(split) => split,
                Err(err) => {
                    self.scan.finish();
                    return Some(Err(err));
                }
            };

            if matches!(&self.lower, Bound::Excluded(lower) if encoded == lower.as_slice()) {
                continue;
            }

            let past_upper = match &self.upper {
                Bound::Included(upper) => encoded > upper.as_slice(),
                Bound::Excluded(upper) => encoded >= upper.as_slice(),
                Bound::Unbounded => false,
            };

            if past_upper {
                self.scan.finish();
                return None;
            }

            return Some(Ok(owner));
        }
    }
}

impl GraphStore {
    /// Create an index on property `key` within `scope` and build its entries
    /// from the current members of the scope.
    ///
    /// Creating an index that already exists with the same scope, key, and
    /// uniqueness returns its identifier and stages nothing. A unique index
    /// and a non-unique index on the same key are separate indexes.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::UniqueConflict`] when `unique` is set and two
    /// members hold the same non-null value, [`GraphError::InvalidIdentity`]
    /// when the scope or `key` was not issued in this namespace,
    /// [`GraphError::IdExhausted`] when the index counter is at `u128::MAX`,
    /// and [`GraphError::Storage`] or [`GraphError::Corrupt`] on a failed read.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_core::{LabelId, NamespaceId, PropertyKeyId, Value};
    /// use pizzlydb_graph::{GraphBatch, GraphError, GraphStore, Scope};
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
    /// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
    ///
    /// let mut batch = GraphBatch::begin(&storage)?;
    /// let person: LabelId = graph.intern(&mut batch, "Person")?;
    /// let email: PropertyKeyId = graph.intern(&mut batch, "email")?;
    /// for _ in 0..2 {
    ///     let node = graph.create_node(&mut batch)?;
    ///     graph.add_label(&mut batch, node, person)?;
    ///     graph.set_property(&mut batch, node, email, &Value::from("a@example.com"))?;
    /// }
    ///
    /// let scope = Scope::Label(person);
    /// assert!(matches!(
    ///     graph.create_index(&mut batch, scope, email, true),
    ///     Err(GraphError::UniqueConflict)
    /// ));
    /// assert_eq!(graph.find_index(&batch, scope, email, true)?, None);
    /// graph.create_index(&mut batch, scope, email, false)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn create_index<V: Vfs>(
        &self,
        batch: &mut GraphBatch<'_, V>,
        scope: Scope,
        key: PropertyKeyId,
        unique: bool,
    ) -> Result<IndexId, GraphError> {
        let namespace = self.namespace();
        scope.ensure_issued(batch, namespace)?;
        catalog::ensure_issued(batch, namespace, key)?;
        if let Some(index) = self.find_index(batch, scope, key, unique)? {
            return Ok(index);
        }

        batch.atomic(|batch| {
            let index = directory::allocate(batch, namespace, id_kind::INDEX)?;
            let definition = IndexDefinition { scope, key, unique };

            batch.put(
                keys::index_definition(namespace, index),
                definition.encode(),
            )?;

            batch.put(
                keys::index_target(namespace, scope, key, unique),
                index.as_bytes().to_vec(),
            )?;

            let target = Target { index, key, unique };
            for owner in members(batch, namespace, scope)? {
                if let Some(value) = property::read_cell(batch, namespace, owner, key)? {
                    insert_entry(batch, namespace, &target, owner, &value)?;
                }
            }

            Ok(index)
        })
    }

    /// Definition of an index.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::InvalidIdentity`] when `index` was not issued in
    /// this namespace, and [`GraphError::Storage`] or [`GraphError::Corrupt`]
    /// on a failed read.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_core::{IndexId, NamespaceId};
    /// use pizzlydb_graph::{GraphBatch, GraphError, GraphStore};
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
    /// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
    ///
    /// let view = storage.snapshot()?;
    /// let unknown = IndexId::from_u128(1)?;
    /// assert!(matches!(graph.index(&view, unknown), Err(GraphError::InvalidIdentity)));
    /// # Ok(())
    /// # }
    /// ```
    pub fn index<R: KeyRead>(
        &self,
        read: &R,
        index: IndexId,
    ) -> Result<IndexDefinition, GraphError> {
        let bytes = read
            .get(&keys::index_definition(self.namespace(), index))?
            .ok_or(GraphError::InvalidIdentity)?;

        IndexDefinition::decode(&bytes)
    }

    /// Identifier of the index on `key` within `scope` with the given
    /// uniqueness, or [`None`].
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Storage`] or [`GraphError::Corrupt`] on a failed
    /// read.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_core::{NamespaceId, PropertyKeyId, TypeId};
    /// use pizzlydb_graph::{GraphBatch, GraphStore, Scope};
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
    /// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
    ///
    /// let mut batch = GraphBatch::begin(&storage)?;
    /// let knows: TypeId = graph.intern(&mut batch, "KNOWS")?;
    /// let since: PropertyKeyId = graph.intern(&mut batch, "since")?;
    /// let scope = Scope::Type(knows);
    /// let index = graph.create_index(&mut batch, scope, since, false)?;
    /// assert_eq!(graph.find_index(&batch, scope, since, false)?, Some(index));
    /// assert_eq!(graph.find_index(&batch, scope, since, true)?, None);
    /// # Ok(())
    /// # }
    /// ```
    pub fn find_index<R: KeyRead>(
        &self,
        read: &R,
        scope: Scope,
        key: PropertyKeyId,
        unique: bool,
    ) -> Result<Option<IndexId>, GraphError> {
        match read.get(&keys::index_target(self.namespace(), scope, key, unique))? {
            None => Ok(None),
            Some(bytes) if bytes.len() == ID_LEN => id_at(&bytes, 0).map(Some),
            Some(_) => Err(GraphError::Corrupt),
        }
    }

    /// Owners whose indexed value equals `value`.
    ///
    /// A unique index yields at most one owner and none for
    /// [`Value::Null`].
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::InvalidValue`] for NaN and the errors of
    /// [`Self::index`]. The iterator yields read errors and
    /// [`GraphError::Corrupt`] for malformed entries.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_core::{LabelId, NamespaceId, PropertyKeyId, Value};
    /// use pizzlydb_graph::{GraphBatch, GraphStore, Owner, Scope};
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
    /// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
    ///
    /// let mut batch = GraphBatch::begin(&storage)?;
    /// let person: LabelId = graph.intern(&mut batch, "Person")?;
    /// let nickname: PropertyKeyId = graph.intern(&mut batch, "nickname")?;
    /// let index = graph.create_index(&mut batch, Scope::Label(person), nickname, false)?;
    /// let node = graph.create_node(&mut batch)?;
    /// graph.add_label(&mut batch, node, person)?;
    /// graph.set_property(&mut batch, node, nickname, &Value::Null)?;
    ///
    /// let nulls = graph.index_lookup(&batch, index, &Value::Null)?.collect::<Result<Vec<_>, _>>()?;
    /// assert_eq!(nulls, vec![Owner::Node(node)]);
    /// # Ok(())
    /// # }
    /// ```
    pub fn index_lookup<'r, R: KeyRead>(
        &self,
        read: &'r R,
        index: IndexId,
        value: &Value,
    ) -> Result<IndexScan<'r, R>, GraphError> {
        self.index_range(read, index, Bound::Included(value), Bound::Included(value))
    }

    /// Owners whose indexed value is within `lower` and `upper`.
    ///
    /// Bounds compare in index order: by value type tag (null, bool, int,
    /// float, string, bytes) and then by value, so a range between two
    /// integers yields only integers.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::InvalidValue`] for a NaN bound and the errors of
    /// [`Self::index`]. The iterator yields read errors and
    /// [`GraphError::Corrupt`] for malformed entries.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::ops::Bound;
    /// use std::path::Path;
    ///
    /// use pizzlydb_core::{LabelId, NamespaceId, PropertyKeyId, Value};
    /// use pizzlydb_graph::{GraphBatch, GraphStore, Owner, Scope};
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
    /// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
    ///
    /// let mut batch = GraphBatch::begin(&storage)?;
    /// let person: LabelId = graph.intern(&mut batch, "Person")?;
    /// let age: PropertyKeyId = graph.intern(&mut batch, "age")?;
    /// let index = graph.create_index(&mut batch, Scope::Label(person), age, false)?;
    /// let mut nodes = Vec::new();
    ///
    /// for years in [-5_i64, 17, 30, 64] {
    ///     let node = graph.create_node(&mut batch)?;
    ///     graph.add_label(&mut batch, node, person)?;
    ///     graph.set_property(&mut batch, node, age, &Value::Int(years))?;
    ///     nodes.push(node);
    /// }
    ///
    /// let adults = graph
    ///     .index_range(&batch, index, Bound::Included(&Value::Int(18)), Bound::Excluded(&Value::Int(65)))?
    ///     .collect::<Result<Vec<_>, _>>()?;
    ///
    /// assert_eq!(adults, vec![Owner::Node(nodes[2]), Owner::Node(nodes[3])]);
    /// # Ok(())
    /// # }
    /// ```
    pub fn index_range<'r, R: KeyRead>(
        &self,
        read: &'r R,
        index: IndexId,
        lower: Bound<&Value>,
        upper: Bound<&Value>,
    ) -> Result<IndexScan<'r, R>, GraphError> {
        let definition = self.index(read, index)?;
        let lower = encode_bound(lower)?;
        let upper = encode_bound(upper)?;
        let prefix = keys::index_entries(self.namespace(), index, definition.unique);
        let mut start = prefix.clone();
        if let Bound::Included(bytes) | Bound::Excluded(bytes) = &lower {
            start.extend_from_slice(bytes);
        }

        Ok(IndexScan {
            scan: PrefixScan::starting_at(read, prefix, &start)?,
            unique: definition.unique,
            scope: definition.scope,
            lower,
            upper,
        })
    }

    /// Nodes with `label`, in identifier order.
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
    /// let person: LabelId = graph.intern(&mut batch, "Person")?;
    /// let a = graph.create_node(&mut batch)?;
    /// let b = graph.create_node(&mut batch)?;
    /// graph.add_label(&mut batch, b, person)?;
    /// graph.add_label(&mut batch, a, person)?;
    /// let members = graph.nodes_with_label(&batch, person)?.collect::<Result<Vec<_>, _>>()?;
    /// assert_eq!(members, vec![a, b]);
    /// # Ok(())
    /// # }
    /// ```
    pub fn nodes_with_label<'r, R: KeyRead>(
        &self,
        read: &'r R,
        label: LabelId,
    ) -> Result<IdScan<'r, R, NodeId>, GraphError> {
        IdScan::new(read, keys::label_members(self.namespace(), label))
    }

    /// Edges of `edge_type`, in identifier order.
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
    /// let edge = graph.create_edge(&mut batch, a, a, knows)?;
    /// let edges = graph.edges_with_type(&batch, knows)?.collect::<Result<Vec<_>, _>>()?;
    /// assert_eq!(edges, vec![edge]);
    /// # Ok(())
    /// # }
    /// ```
    pub fn edges_with_type<'r, R: KeyRead>(
        &self,
        read: &'r R,
        edge_type: TypeId,
    ) -> Result<IdScan<'r, R, EdgeId>, GraphError> {
        IdScan::new(read, keys::type_members(self.namespace(), edge_type))
    }
}

/// Index that an owner's property feeds.
struct Target {
    index: IndexId,
    key: PropertyKeyId,
    unique: bool,
}

/// Move the entries of `owner` for property `key` from `previous` to
/// `current` in every index on the key within the owner's scopes.
pub(crate) fn apply_property_index<V: Vfs>(
    batch: &mut GraphBatch<'_, V>,
    namespace: NamespaceId,
    owner: Owner,
    key: PropertyKeyId,
    previous: Option<&Value>,
    current: Option<&Value>,
) -> Result<(), GraphError> {
    if previous == current {
        return Ok(());
    }

    for scope in scopes(batch, namespace, owner)? {
        for target in targets(batch, namespace, scope, Some(key))? {
            if let Some(value) = previous {
                remove_entry(batch, namespace, &target, owner, value)?;
            }
            if let Some(value) = current {
                insert_entry(batch, namespace, &target, owner, value)?;
            }
        }
    }

    Ok(())
}

/// Update the entries of `node` in every index scoped to a removed or added
/// label.
pub(crate) fn sync_labels<V: Vfs>(
    batch: &mut GraphBatch<'_, V>,
    namespace: NamespaceId,
    node: NodeId,
    removed: &[LabelId],
    added: &[LabelId],
) -> Result<(), GraphError> {
    let owner = Owner::Node(node);

    for (labels, insert) in [(removed, false), (added, true)] {
        for &label in labels {
            for target in targets(batch, namespace, Scope::Label(label), None)? {
                let Some(value) = property::read_cell(batch, namespace, owner, target.key)? else {
                    continue;
                };
                if insert {
                    insert_entry(batch, namespace, &target, owner, &value)?;
                } else {
                    remove_entry(batch, namespace, &target, owner, &value)?;
                }
            }
        }
    }

    Ok(())
}

/// Stage the relationship-type membership key of a new edge.
pub(crate) fn note_edge<V: Vfs>(
    batch: &mut GraphBatch<'_, V>,
    namespace: NamespaceId,
    edge: &EdgeRecord,
) -> Result<(), GraphError> {
    batch.put(
        keys::type_member(namespace, edge.edge_type, edge.id),
        Vec::new(),
    )
}

/// Stage removal of the relationship-type membership key of an edge.
pub(crate) fn forget_edge<V: Vfs>(
    batch: &mut GraphBatch<'_, V>,
    namespace: NamespaceId,
    edge: &EdgeRecord,
) -> Result<(), GraphError> {
    batch.delete(keys::type_member(namespace, edge.edge_type, edge.id))
}

fn insert_entry<V: Vfs>(
    batch: &mut GraphBatch<'_, V>,
    namespace: NamespaceId,
    target: &Target,
    owner: Owner,
    value: &Value,
) -> Result<(), GraphError> {
    if target.unique && *value == Value::Null {
        return Ok(());
    }

    let key = entry_key(namespace, target, owner, value)?;
    if target.unique {
        constraint::ensure_unique(batch, &key, owner.id_bytes())?;
        batch.put(key, owner.id_bytes().to_vec())
    } else {
        batch.put(key, Vec::new())
    }
}

fn remove_entry<V: Vfs>(
    batch: &mut GraphBatch<'_, V>,
    namespace: NamespaceId,
    target: &Target,
    owner: Owner,
    value: &Value,
) -> Result<(), GraphError> {
    if target.unique && *value == Value::Null {
        return Ok(());
    }

    batch.delete(entry_key(namespace, target, owner, value)?)
}

pub(crate) fn index_entry_key(
    namespace: NamespaceId,
    index: IndexId,
    unique: bool,
    owner: Owner,
    value: &Value,
) -> Result<Vec<u8>, GraphError> {
    let mut key = keys::index_entries(namespace, index, unique);
    keys::push_value(&mut key, value)?;
    if !unique {
        key.extend_from_slice(owner.id_bytes());
    }

    Ok(key)
}

fn entry_key(
    namespace: NamespaceId,
    target: &Target,
    owner: Owner,
    value: &Value,
) -> Result<Vec<u8>, GraphError> {
    index_entry_key(namespace, target.index, target.unique, owner, value)
}

/// Scopes that contain `owner`: a node's labels, or an edge's type.
fn scopes<R: KeyRead>(
    read: &R,
    namespace: NamespaceId,
    owner: Owner,
) -> Result<Vec<Scope>, GraphError> {
    match owner {
        Owner::Node(node) => IdScan::new(read, keys::node_labels(namespace, node))?
            .map(|label| label.map(Scope::Label))
            .collect(),
        Owner::Edge(edge) => {
            let record = directory::load_edge(read, namespace, edge)?;
            Ok(vec![Scope::Type(record.edge_type)])
        }
    }
}

/// Indexes within `scope`, optionally limited to one property key.
fn targets<R: KeyRead>(
    read: &R,
    namespace: NamespaceId,
    scope: Scope,
    key: Option<PropertyKeyId>,
) -> Result<Vec<Target>, GraphError> {
    let mut prefix = keys::index_targets(namespace, scope);
    let key_at = prefix.len();
    if let Some(key) = key {
        prefix.extend_from_slice(key.as_bytes());
    }

    let mut found = Vec::new();
    for entry in PrefixScan::new(read, prefix)? {
        let (target_key, index) = entry?;
        let unique = match target_key.get(key_at + ID_LEN..) {
            Some([0]) => false,
            Some([1]) => true,
            _ => return Err(GraphError::Corrupt),
        };

        if index.len() != ID_LEN {
            return Err(GraphError::Corrupt);
        }

        found.push(Target {
            index: id_at(&index, 0)?,
            key: id_at(&target_key, key_at)?,
            unique,
        });
    }

    Ok(found)
}

fn members<R: KeyRead>(
    read: &R,
    namespace: NamespaceId,
    scope: Scope,
) -> Result<Vec<Owner>, GraphError> {
    let graph = GraphStore::new(namespace);
    match scope {
        Scope::Label(label) => graph
            .nodes_with_label(read, label)?
            .map(|node| node.map(Owner::Node))
            .collect(),
        Scope::Type(edge_type) => graph
            .edges_with_type(read, edge_type)?
            .map(|edge| edge.map(Owner::Edge))
            .collect(),
    }
}

fn encode_bound(bound: Bound<&Value>) -> Result<Bound<Vec<u8>>, GraphError> {
    let encode = |value: &Value| {
        let mut out = Vec::new();
        keys::push_value(&mut out, value).map(|()| out)
    };

    Ok(match bound {
        Bound::Included(value) => Bound::Included(encode(value)?),
        Bound::Excluded(value) => Bound::Excluded(encode(value)?),
        Bound::Unbounded => Bound::Unbounded,
    })
}

#[cfg(test)]
mod tests {
    use pizzlydb_core::{LabelId, PropertyKeyId, TypeId};

    use super::IndexDefinition;
    use crate::error::GraphError;
    use crate::property::Scope;

    #[test]
    fn definitions_round_trip() {
        let key = PropertyKeyId::from_u128(9).unwrap();
        for definition in [
            IndexDefinition {
                scope: Scope::Label(LabelId::from_u128(1).unwrap()),
                key,
                unique: true,
            },
            IndexDefinition {
                scope: Scope::Type(TypeId::from_u128(u128::MAX).unwrap()),
                key,
                unique: false,
            },
        ] {
            assert_eq!(
                IndexDefinition::decode(&definition.encode()).unwrap(),
                definition
            );
        }
    }

    #[test]
    fn malformed_definitions_are_corrupt() {
        let valid = IndexDefinition {
            scope: Scope::Label(LabelId::from_u128(1).unwrap()),
            key: PropertyKeyId::from_u128(2).unwrap(),
            unique: false,
        }
        .encode();
        let mut cases = vec![Vec::new(), valid[..valid.len() - 1].to_vec()];
        for (at, byte) in [(0, 2), (1, 3), (valid.len() - 1, 2)] {
            let mut bad = valid.clone();
            bad[at] = byte;
            cases.push(bad);
        }
        let mut zero_key = valid.clone();
        zero_key[18..34].fill(0);
        cases.push(zero_key);
        for bytes in cases {
            assert!(matches!(
                IndexDefinition::decode(&bytes),
                Err(GraphError::Corrupt)
            ));
        }
    }
}
