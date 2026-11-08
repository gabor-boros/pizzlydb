//! Per-namespace schema version and a snapshot cache of catalog metadata.
//!
//! A schema version starts at zero. The first schema change in a transaction
//! increments it once; later schema changes in that transaction record the
//! same version. The version key is part of the read set, so a concurrent
//! schema change conflicts.
//!
//! Names are never retired. A rename changes the bytes of an issued id, and
//! the id is not reused. That is the catalog retirement policy.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use pizzlydb_core::{
    GraphMutation, IndexId, LabelId, NamespaceId, PropertyKeyId, TypeId, schema_change,
};
use pizzlydb_storage::Vfs;

use crate::access::{KeyRead, PrefixScan, id_at};
use crate::batch::GraphTxn;
use crate::error::{GraphError, corrupt};
use crate::index::{IndexDefinition, IndexPhase};
use crate::keys::{self, id_kind};
use crate::property::{PropertyDeclaration, Scope};
use crate::record_store::GraphStore;

pub(crate) fn read_version<R: KeyRead>(
    read: &R,
    namespace: NamespaceId,
) -> Result<u64, GraphError> {
    match read.get(&keys::schema_version(namespace))? {
        None => Ok(0),
        Some(bytes) if bytes.len() == 8 => {
            let mut buf = [0_u8; 8];
            buf.copy_from_slice(&bytes);
            Ok(u64::from_be_bytes(buf))
        }
        Some(_) => Err(GraphError::Corrupt),
    }
}

/// Increment the schema version at most once in this transaction and record
/// the change.
pub(crate) fn bump<V: Vfs>(
    batch: &mut GraphTxn<V>,
    namespace: NamespaceId,
    kind: u8,
) -> Result<u64, GraphError> {
    let version = if batch.schema_was_bumped(namespace) {
        read_version(batch, namespace)?
    } else {
        let next = read_version(batch, namespace)?
            .checked_add(1)
            .ok_or(GraphError::Corrupt)?;
        batch.put(keys::schema_version(namespace), next.to_be_bytes().to_vec())?;
        batch.mark_schema_bumped(namespace);
        next
    };
    batch.record(GraphMutation::SchemaChanged {
        namespace,
        version,
        kind,
    });
    Ok(version)
}

impl GraphStore {
    /// Schema version of this namespace. Absence is version zero.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Storage`] or [`GraphError::Corrupt`] on a failed read.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_core::NamespaceId;
    /// use pizzlydb_graph::{GraphStore, GraphTxn};
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
    /// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
    /// let txn = GraphTxn::begin(&storage)?;
    /// assert_eq!(graph.schema_version(&txn)?, 0);
    /// # Ok(())
    /// # }
    /// ```
    pub fn schema_version<R: KeyRead>(&self, read: &R) -> Result<u64, GraphError> {
        self.ensure(read)?;
        read_version(read, self.namespace())
    }
}

/// Immutable catalog facts for one `(namespace, schema version)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogSnapshot {
    /// Namespace the facts were read from.
    pub namespace: NamespaceId,
    /// Schema version of the read.
    pub version: u64,
    /// Label id to name.
    pub labels: BTreeMap<LabelId, String>,
    /// Relationship type id to name.
    pub types: BTreeMap<TypeId, String>,
    /// Property key id to name.
    pub keys: BTreeMap<PropertyKeyId, String>,
    /// Declarations.
    pub declarations: BTreeMap<(Scope, PropertyKeyId), PropertyDeclaration>,
    /// Indexes in the `Ready` phase.
    pub indexes: BTreeMap<IndexId, IndexDefinition>,
}

/// Process-local cache of [`CatalogSnapshot`] values.
///
/// Entries are immutable. A reader of an older snapshot keeps the entry for
/// that version. The cache does not lock the database.
#[derive(Debug, Default)]
pub struct CatalogCache {
    entries: RwLock<BTreeMap<(NamespaceId, u64), Arc<CatalogSnapshot>>>,
}

impl CatalogCache {
    /// Empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Cached snapshot, or a newly loaded one.
    ///
    /// # Errors
    ///
    /// Returns the errors of reading the catalog.
    pub fn get_or_load<R: KeyRead>(
        &self,
        read: &R,
        namespace: NamespaceId,
    ) -> Result<Arc<CatalogSnapshot>, GraphError> {
        let version = read_version(read, namespace)?;
        if let Some(hit) = self
            .entries
            .read()
            .unwrap_or_else(|err| err.into_inner())
            .get(&(namespace, version))
            .cloned()
        {
            return Ok(hit);
        }
        let loaded = Arc::new(load_snapshot(read, namespace, version)?);
        self.entries
            .write()
            .unwrap_or_else(|err| err.into_inner())
            .insert((namespace, version), Arc::clone(&loaded));
        Ok(loaded)
    }
}

fn load_snapshot<R: KeyRead>(
    read: &R,
    namespace: NamespaceId,
    version: u64,
) -> Result<CatalogSnapshot, GraphError> {
    Ok(CatalogSnapshot {
        namespace,
        version,
        labels: load_names(read, namespace, id_kind::LABEL)?,
        types: load_names(read, namespace, id_kind::TYPE)?,
        keys: load_names(read, namespace, id_kind::PROPERTY_KEY)?,
        declarations: load_declarations(read, namespace)?,
        indexes: load_ready_indexes(read, namespace)?,
    })
}

fn load_names<R: KeyRead, I: pizzlydb_core::Identifier>(
    read: &R,
    namespace: NamespaceId,
    kind: u8,
) -> Result<BTreeMap<I, String>, GraphError> {
    let prefix = keys::name_id_keys(namespace, kind);
    let mut names = BTreeMap::new();
    for entry in PrefixScan::new(read, prefix.clone())? {
        let (key, value) = entry?;
        if key.len() != prefix.len() + 16 {
            return Err(GraphError::Corrupt);
        }
        let id = id_at(&key, prefix.len())?;
        let name = String::from_utf8(value).map_err(corrupt)?;
        names.insert(id, name);
    }
    Ok(names)
}

fn load_declarations<R: KeyRead>(
    read: &R,
    namespace: NamespaceId,
) -> Result<BTreeMap<(Scope, PropertyKeyId), PropertyDeclaration>, GraphError> {
    let prefix = keys::declaration_keys(namespace);
    let mut declarations = BTreeMap::new();
    for entry in PrefixScan::new(read, prefix.clone())? {
        let (key, value) = entry?;
        if key.len() != prefix.len() + 1 + 32 {
            return Err(GraphError::Corrupt);
        }
        let kind = key[prefix.len()];
        let scope = Scope::decode(kind, &key, prefix.len() + 1)?;
        let property = id_at(&key, prefix.len() + 1 + 16)?;
        declarations.insert((scope, property), PropertyDeclaration::decode(&value)?);
    }
    Ok(declarations)
}

fn load_ready_indexes<R: KeyRead>(
    read: &R,
    namespace: NamespaceId,
) -> Result<BTreeMap<IndexId, IndexDefinition>, GraphError> {
    let graph = GraphStore::new(namespace);
    let prefix = keys::index_keys(namespace);
    let mut indexes = BTreeMap::new();
    for entry in PrefixScan::new(read, prefix)? {
        let (key, _) = entry?;
        if key.len() < 16 {
            return Err(GraphError::Corrupt);
        }
        let id = id_at::<IndexId>(&key, key.len() - 16)?;
        let state = graph.index_state(read, id)?;
        if state.phase == IndexPhase::Ready {
            indexes.insert(id, state.definition);
        }
    }
    Ok(indexes)
}

pub(crate) fn touch_catalog<V: Vfs>(
    batch: &mut GraphTxn<V>,
    namespace: NamespaceId,
) -> Result<(), GraphError> {
    bump(batch, namespace, schema_change::CATALOG).map(|_| ())
}

pub(crate) fn touch_rename<V: Vfs>(
    batch: &mut GraphTxn<V>,
    namespace: NamespaceId,
) -> Result<(), GraphError> {
    bump(batch, namespace, schema_change::RENAME).map(|_| ())
}
