//! Durable registry of graph namespaces.
//!
//! A namespace id is allocated from the system id space and is never reused.
//! `GraphStore::open` reads the registry entry and rejects an unknown or
//! dropping namespace. That read is part of the transaction read set when the
//! caller passes a [`GraphTxn`](crate::GraphTxn).
//!
//! Dropping a namespace marks it `Dropping` immediately. [`NamespaceRegistry::reclaim`]
//! deletes its graph keys in bounded chunks and then removes the registry
//! entry. A crash resumes from the stored cursor. Other namespaces are not
//! visited.

use pizzlydb_core::{GraphMutation, NamespaceId};
use pizzlydb_storage::Vfs;
use pizzlydb_txn::{
    GRAPH_LOGICAL_VERSION, namespace_name_key, namespace_record_end, namespace_record_key,
    namespace_record_prefix, stat_prefix,
};

use crate::access::KeyRead;
use crate::batch::GraphTxn;
use crate::catalog::valid_name;
use crate::error::{GraphError, corrupt};
use crate::record_store::GraphStore;

const META_VERSION: u8 = 1;
const STATE_ACTIVE: u8 = 1;
const STATE_DROPPING: u8 = 2;
const CREATED_AT: usize = 4;
const HEADER: usize = 28;
const NAMESPACE_KIND: u8 = 7;

/// Placeholder limits. Quotas are not enforced yet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NamespaceLimits {
    /// Maximum live nodes, or [`None`] when unset.
    pub max_nodes: Option<u64>,
    /// Maximum live edges, or [`None`] when unset.
    pub max_edges: Option<u64>,
}

/// Options for [`NamespaceRegistry::create`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NamespaceOptions {
    /// Stored and not enforced.
    pub limits: NamespaceLimits,
}

/// Lifecycle of a registered namespace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NamespaceState {
    /// Accepts graph operations.
    Active,
    /// Rejects graph operations while its keys are deleted.
    Dropping,
}

/// Registry record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamespaceMeta {
    /// Namespace identifier.
    pub id: NamespaceId,
    /// Unique name.
    pub name: String,
    /// Active or dropping.
    pub state: NamespaceState,
    /// Commit sequence that created the namespace. Zero only before that commit lands.
    pub created_commit: u64,
    /// Graph logical format version copied from the transaction manager.
    pub logical_version: u16,
    /// Placeholder limits.
    pub limits: NamespaceLimits,
}

/// Registry operations. The type has no state.
#[derive(Clone, Copy, Debug, Default)]
pub struct NamespaceRegistry;

impl NamespaceRegistry {
    /// Register `name` and allocate a fresh id.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::InvalidName`], [`GraphError::NameConflict`],
    /// [`GraphError::IdExhausted`], or a storage error.
    pub fn create<V: Vfs>(
        batch: &mut GraphTxn<V>,
        name: impl AsRef<[u8]>,
        options: NamespaceOptions,
    ) -> Result<NamespaceMeta, GraphError> {
        let name = valid_name(name.as_ref())?;
        if lookup_name(batch, name)?.is_some() {
            return Err(GraphError::NameConflict);
        }
        batch.atomic(|batch| {
            let raw = batch.allocate_raw(&[0; 16], NAMESPACE_KIND)?;
            let id = NamespaceId::from_u128(raw).map_err(|_| GraphError::Corrupt)?;
            let meta = NamespaceMeta {
                id,
                name: String::from_utf8(name.to_vec()).map_err(corrupt)?,
                state: NamespaceState::Active,
                created_commit: 0,
                logical_version: GRAPH_LOGICAL_VERSION,
                limits: options.limits,
            };
            let encoded = encode_meta(&meta)?;
            let key = namespace_record_key(id.as_bytes());
            batch.put(key.clone(), encoded)?;
            batch.stamp_commit(&key, CREATED_AT)?;
            batch.put(namespace_name_key(name), id.as_bytes().to_vec())?;
            batch.record(GraphMutation::NamespaceCreate {
                namespace: id,
                name: name.to_vec(),
            });
            Ok(meta)
        })
    }

    /// Registered namespaces in id order.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Storage`] or [`GraphError::Corrupt`].
    pub fn list<R: KeyRead>(read: &R) -> Result<Vec<NamespaceMeta>, GraphError> {
        let start = namespace_record_prefix();
        let end = namespace_record_end();
        let mut out = Vec::new();
        for (key, value) in read.scan_range(&start, &end)? {
            if key.len() != start.len() + 16 {
                return Err(GraphError::Corrupt);
            }
            let mut id = [0_u8; 16];
            id.copy_from_slice(&key[start.len()..]);
            out.push(decode_meta(
                NamespaceId::from_bytes(id).map_err(corrupt)?,
                &value,
            )?);
        }
        Ok(out)
    }

    /// One registry record.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::NamespaceNotFound`] when `id` is not registered.
    pub fn get<R: KeyRead>(read: &R, id: NamespaceId) -> Result<NamespaceMeta, GraphError> {
        load(read, id)
    }

    /// Identifier of `name`, if it is registered.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::InvalidName`] or a storage error.
    pub fn lookup<R: KeyRead>(
        read: &R,
        name: impl AsRef<[u8]>,
    ) -> Result<Option<NamespaceId>, GraphError> {
        let name = valid_name(name.as_ref())?;
        lookup_name(read, name)
    }

    /// Rename an active namespace.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::NamespaceNotFound`], [`GraphError::NamespaceDropping`],
    /// [`GraphError::InvalidName`], or [`GraphError::NameConflict`].
    pub fn rename<V: Vfs>(
        batch: &mut GraphTxn<V>,
        id: NamespaceId,
        name: impl AsRef<[u8]>,
    ) -> Result<(), GraphError> {
        let name = valid_name(name.as_ref())?;
        let mut meta = load(batch, id)?;
        if meta.state != NamespaceState::Active {
            return Err(GraphError::NamespaceDropping);
        }
        if meta.name.as_bytes() == name {
            return Ok(());
        }
        if lookup_name(batch, name)?.is_some() {
            return Err(GraphError::NameConflict);
        }
        let previous = meta.name.clone();
        meta.name = String::from_utf8(name.to_vec()).map_err(corrupt)?;
        batch.atomic(|batch| {
            batch.delete(namespace_name_key(previous.as_bytes()))?;
            batch.put(namespace_name_key(name), id.as_bytes().to_vec())?;
            batch.put(namespace_record_key(id.as_bytes()), encode_meta(&meta)?)?;
            batch.record(GraphMutation::NamespaceRename {
                namespace: id,
                previous: previous.into_bytes(),
                name: name.to_vec(),
            });
            Ok(())
        })
    }

    /// Mark `id` dropping. Later [`Self::reclaim`] calls delete its keys.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::NamespaceNotFound`]. Repeating a drop is success.
    pub fn drop_namespace<V: Vfs>(
        batch: &mut GraphTxn<V>,
        id: NamespaceId,
    ) -> Result<(), GraphError> {
        let mut meta = load(batch, id)?;
        if meta.state == NamespaceState::Dropping {
            return Ok(());
        }
        meta.state = NamespaceState::Dropping;
        let name = meta.name.clone();
        batch.atomic(|batch| {
            batch.put(namespace_record_key(id.as_bytes()), encode_meta(&meta)?)?;
            batch.record(GraphMutation::NamespaceDrop {
                namespace: id,
                name: name.into_bytes(),
            });
            Ok(())
        })
    }

    /// Delete up to `max_keys` graph keys of a dropping namespace.
    ///
    /// Returns `true` when the registry entry is gone. A caller repeats the
    /// call, each in its own transaction, until then.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::NamespaceNotFound`] when the namespace is already
    /// gone, and [`GraphError::SchemaConflict`] when it is still active.
    pub fn reclaim<V: Vfs>(
        batch: &mut GraphTxn<V>,
        id: NamespaceId,
        max_keys: u32,
    ) -> Result<bool, GraphError> {
        let meta = match load(batch, id) {
            Ok(meta) => meta,
            Err(GraphError::NamespaceNotFound) => return Ok(true),
            Err(err) => return Err(err),
        };
        if meta.state != NamespaceState::Dropping {
            return Err(GraphError::SchemaConflict);
        }
        let cursor = cursor_of(batch, id)?;
        let prefix = id.as_bytes().to_vec();
        let end = prefix_end(&prefix).ok_or(GraphError::Corrupt)?;
        let start = if cursor.is_empty() {
            prefix.clone()
        } else {
            cursor
        };
        let cap = usize::try_from(max_keys.max(1)).unwrap_or(usize::MAX);
        let (rows, truncated) = batch.scan_capped(&start, &end, cap)?;
        batch.atomic(|batch| {
            let mut last = Vec::new();
            for (key, _) in &rows {
                batch.delete(key.clone())?;
                last.clone_from(key);
            }
            if truncated {
                let next = successor(&last);
                write_cursor(batch, &meta, &next)?;
                return Ok(false);
            }
            delete_system_keys(batch, id)?;
            let (left, still) = batch.scan_capped(&prefix, &end, 1)?;
            if still || !left.is_empty() {
                let resume = left.first().map(|(key, _)| key.clone()).unwrap_or(prefix);
                write_cursor(batch, &meta, &resume)?;
                return Ok(false);
            }
            batch.delete(namespace_name_key(meta.name.as_bytes()))?;
            batch.delete(namespace_record_key(id.as_bytes()))?;
            Ok(true)
        })
    }
}

impl GraphStore {
    /// Open a registered active namespace.
    ///
    /// The registry read is tracked when `read` is a transaction.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::NamespaceNotFound`] or [`GraphError::NamespaceDropping`].
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_graph::{GraphStore, GraphTxn, NamespaceOptions, NamespaceRegistry};
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
    /// let mut txn = GraphTxn::begin(&storage)?;
    /// let meta = NamespaceRegistry::create(&mut txn, "main", NamespaceOptions::default())?;
    /// txn.commit()?;
    /// let txn = GraphTxn::begin(&storage)?;
    /// let graph = GraphStore::open(&txn, meta.id)?;
    /// assert_eq!(graph.namespace(), meta.id);
    /// # Ok(())
    /// # }
    /// ```
    pub fn open<R: KeyRead>(read: &R, id: NamespaceId) -> Result<Self, GraphError> {
        match load(read, id)?.state {
            NamespaceState::Active => Ok(Self::checked(id)),
            NamespaceState::Dropping => Err(GraphError::NamespaceDropping),
        }
    }
}

pub(crate) fn load<R: KeyRead>(read: &R, id: NamespaceId) -> Result<NamespaceMeta, GraphError> {
    match read.get(&namespace_record_key(id.as_bytes()))? {
        None => Err(GraphError::NamespaceNotFound),
        Some(bytes) => decode_meta(id, &bytes),
    }
}

fn lookup_name<R: KeyRead>(read: &R, name: &[u8]) -> Result<Option<NamespaceId>, GraphError> {
    match read.get(&namespace_name_key(name))? {
        None => Ok(None),
        Some(bytes) if bytes.len() == 16 => {
            let mut id = [0_u8; 16];
            id.copy_from_slice(&bytes);
            Ok(Some(NamespaceId::from_bytes(id).map_err(corrupt)?))
        }
        Some(_) => Err(GraphError::Corrupt),
    }
}

fn encode_meta(meta: &NamespaceMeta) -> Result<Vec<u8>, GraphError> {
    let name = meta.name.as_bytes();
    let name_len = u32::try_from(name.len()).map_err(|_| GraphError::InvalidName)?;
    let mut out = Vec::with_capacity(HEADER + 4 + name.len());
    out.push(META_VERSION);
    out.push(match meta.state {
        NamespaceState::Active => STATE_ACTIVE,
        NamespaceState::Dropping => STATE_DROPPING,
    });
    out.extend_from_slice(&meta.logical_version.to_be_bytes());
    out.extend_from_slice(&meta.created_commit.to_be_bytes());
    out.extend_from_slice(&meta.limits.max_nodes.unwrap_or(0).to_be_bytes());
    out.extend_from_slice(&meta.limits.max_edges.unwrap_or(0).to_be_bytes());
    out.extend_from_slice(&name_len.to_be_bytes());
    out.extend_from_slice(name);
    Ok(out)
}

fn decode_meta(id: NamespaceId, bytes: &[u8]) -> Result<NamespaceMeta, GraphError> {
    if bytes.len() < HEADER + 4 || bytes[0] != META_VERSION {
        return Err(GraphError::Corrupt);
    }
    let state = match bytes[1] {
        STATE_ACTIVE => NamespaceState::Active,
        STATE_DROPPING => NamespaceState::Dropping,
        _ => return Err(GraphError::Corrupt),
    };
    let logical_version =
        u16::from_be_bytes(bytes[2..4].try_into().map_err(|_| GraphError::Corrupt)?);
    let created_commit =
        u64::from_be_bytes(bytes[4..12].try_into().map_err(|_| GraphError::Corrupt)?);
    let max_nodes = u64::from_be_bytes(bytes[12..20].try_into().map_err(|_| GraphError::Corrupt)?);
    let max_edges = u64::from_be_bytes(bytes[20..28].try_into().map_err(|_| GraphError::Corrupt)?);
    let name_len =
        u32::from_be_bytes(bytes[28..32].try_into().map_err(|_| GraphError::Corrupt)?) as usize;
    let name_bytes = bytes.get(32..32 + name_len).ok_or(GraphError::Corrupt)?;
    Ok(NamespaceMeta {
        id,
        name: String::from_utf8(name_bytes.to_vec()).map_err(corrupt)?,
        state,
        created_commit,
        logical_version,
        limits: NamespaceLimits {
            max_nodes: (max_nodes != 0).then_some(max_nodes),
            max_edges: (max_edges != 0).then_some(max_edges),
        },
    })
}

fn cursor_of<R: KeyRead>(read: &R, id: NamespaceId) -> Result<Vec<u8>, GraphError> {
    let Some(bytes) = read.get(&namespace_record_key(id.as_bytes()))? else {
        return Ok(Vec::new());
    };
    if bytes.len() < HEADER + 4 {
        return Err(GraphError::Corrupt);
    }
    let name_len = u32::from_be_bytes(
        bytes
            .get(28..32)
            .ok_or(GraphError::Corrupt)?
            .try_into()
            .map_err(|_| GraphError::Corrupt)?,
    ) as usize;
    let cursor_at = 32 + name_len;
    if cursor_at > bytes.len() {
        return Err(GraphError::Corrupt);
    }
    if cursor_at == bytes.len() {
        return Ok(Vec::new());
    }
    let rest = &bytes[cursor_at..];
    if rest.len() < 4 {
        return Err(GraphError::Corrupt);
    }
    let len = u32::from_be_bytes(rest[..4].try_into().map_err(|_| GraphError::Corrupt)?) as usize;
    Ok(rest.get(4..4 + len).ok_or(GraphError::Corrupt)?.to_vec())
}

fn write_cursor<V: Vfs>(
    batch: &mut GraphTxn<V>,
    meta: &NamespaceMeta,
    cursor: &[u8],
) -> Result<(), GraphError> {
    let mut bytes = encode_meta(meta)?;
    let len = u32::try_from(cursor.len()).map_err(|_| GraphError::Corrupt)?;
    bytes.extend_from_slice(&len.to_be_bytes());
    bytes.extend_from_slice(cursor);
    batch.put(namespace_record_key(meta.id.as_bytes()), bytes)
}

fn delete_system_keys<V: Vfs>(batch: &mut GraphTxn<V>, id: NamespaceId) -> Result<(), GraphError> {
    let prefix = stat_prefix(id.as_bytes());
    let end = prefix_end(&prefix).ok_or(GraphError::Corrupt)?;
    loop {
        let (rows, truncated) = batch.scan_capped(&prefix, &end, 64)?;
        if rows.is_empty() {
            break;
        }
        for (key, _) in rows {
            batch.delete(key)?;
        }
        if !truncated {
            break;
        }
    }
    for kind in 1..=6_u8 {
        batch.delete(id_watermark(id.as_bytes(), kind))?;
    }
    Ok(())
}

fn id_watermark(namespace: &[u8; 16], kind: u8) -> Vec<u8> {
    let mut key = vec![0; 16];
    key.push(5);
    key.extend_from_slice(namespace);
    key.push(kind);
    key
}

fn successor(key: &[u8]) -> Vec<u8> {
    let mut next = key.to_vec();
    next.push(0);
    next
}

fn prefix_end(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut end = prefix.to_vec();
    while let Some(byte) = end.pop() {
        if byte < u8::MAX {
            end.push(byte + 1);
            return Some(end);
        }
    }
    None
}

/// Check every registered active namespace.
///
/// # Errors
///
/// Returns the errors of [`crate::verify_full`].
pub fn verify_registered<R: KeyRead>(read: &R) -> Result<(), GraphError> {
    for meta in NamespaceRegistry::list(read)? {
        if meta.state == NamespaceState::Active {
            crate::verify::verify_full(&GraphStore::new(meta.id), read)?;
        }
    }
    Ok(())
}
