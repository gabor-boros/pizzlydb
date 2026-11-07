//! Copy one namespace into a new database through the graph API.
//!
//! [`salvage`] does not write, resize, sync, or rename the source file or its
//! log. A failed call leaves an existing destination in place. `destination.partial`
//! and `destination.source` may remain.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use pizzlydb_core::{
    EdgeId, ID_LEN, Identifier, IndexId, LabelId, NamespaceId, NodeId, PropertyKeyId, TypeId,
};
use pizzlydb_storage::{Durability, FileRole, OpenOptions, Storage, StorageError, Vfs, VfsError};

use crate::access::{IdScan, KeyRead, PrefixScan, id_at};
use crate::batch::GraphBatch;
use crate::catalog::CatalogId;
use crate::error::GraphError;
use crate::keys::{self, id_kind};
use crate::property::{Owner, Scope};
use crate::record_store::GraphStore;
use crate::verify::verify_full;

const FRAME_COUNT: usize = 32;

/// Record that was not copied.
///
/// `source_id` is the source identifier, or zero when the omission is the log
/// or the identifier itself did not decode. [`OmissionKind::Wal`] is recorded
/// once when the source log cannot be opened and the checkpoint image can.
///
/// # Examples
///
/// ```
/// use pizzlydb_graph::{Omission, OmissionKind, OmissionReason};
///
/// let omission = Omission {
///     kind: OmissionKind::Node,
///     source_id: 4,
///     reason: OmissionReason::Corrupt,
/// };
/// assert_eq!(omission.kind, OmissionKind::Node);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Omission {
    /// Which record was skipped.
    pub kind: OmissionKind,
    /// Source identifier, or zero for a log omission.
    pub source_id: u128,
    /// Why the record was skipped.
    pub reason: OmissionReason,
}

/// Kind of record named by an [`Omission`].
///
/// # Examples
///
/// ```
/// use pizzlydb_graph::OmissionKind;
///
/// assert_ne!(OmissionKind::Node, OmissionKind::Edge);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OmissionKind {
    /// The source log was not copied.
    Wal,
    /// A node record.
    Node,
    /// An edge record.
    Edge,
    /// A label name or a node's label.
    Label,
    /// A relationship-type name.
    Type,
    /// A property-key name.
    PropertyKey,
    /// A property value or a declaration.
    Property,
    /// An index definition.
    Index,
}

/// Why an [`Omission`] was skipped.
///
/// # Examples
///
/// ```
/// use pizzlydb_graph::OmissionReason;
///
/// assert_ne!(OmissionReason::Missing, OmissionReason::Corrupt);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OmissionReason {
    /// The stored bytes do not decode.
    Corrupt,
    /// The record names something that was not copied.
    Missing,
    /// The log was replaced by the checkpoint image.
    Wal,
}

/// Records copied by [`salvage`], and the records that were skipped.
///
/// `nodes` and `edges` count records created in the destination. A missing or
/// deleted source record is a hole: it is not an omission, and it is not
/// counted. A hole or an omission shifts every later identifier of that kind,
/// because destination identifiers come from the destination counters. When
/// nothing of that kind is skipped, those identifiers match the source.
///
/// # Examples
///
/// ```
/// use pizzlydb_graph::SalvageReport;
///
/// let report = SalvageReport {
///     nodes: 1,
///     edges: 0,
///     omissions: Vec::new(),
///     omitted_namespaces: Vec::new(),
/// };
/// assert_eq!(report.nodes, 1);
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SalvageReport {
    /// Nodes created in the destination.
    pub nodes: u64,
    /// Edges created in the destination.
    pub edges: u64,
    /// Records that were not copied.
    pub omissions: Vec<Omission>,
    /// Namespaces present in the source and not copied.
    pub omitted_namespaces: Vec<NamespaceId>,
}

/// Copy the committed records of `namespace` from `source` into `destination`.
///
/// Only `namespace` is copied. Other namespaces are listed in
/// [`SalvageReport::omitted_namespaces`] and are not per-record omissions.
/// The destination file identity is drawn from a CSPRNG. When
/// [`Storage::open`](pizzlydb_storage::Storage::open)
/// fails and the checkpoint image can be opened, that image is copied and the
/// report contains one [`OmissionKind::Wal`] omission. Uncheckpointed commits
/// are not invented. After the copy, [`verify_full`](crate::verify_full) must
/// succeed before `destination` is replaced.
///
/// # Errors
///
/// Returns [`GraphError::Storage`] when the source cannot be read, neither
/// superblock decodes, or the destination copy cannot be published. A unique
/// index that cannot be rebuilt returns [`GraphError::UniqueConflict`] and
/// does not replace `destination`.
///
/// # Limitations
///
/// - **Offline only.** The source is opened through the storage engine, and
///   nothing excludes a live writer.
///   [Read-only open](https://github.com/gabor-boros/pizzlydb/blob/main/docs/roadmap/015-exclusive-ownership-and-read-only-open.md)
///   is later work.
/// - **Single namespace.** Every other namespace is reported and skipped.
///   [Multi-namespace salvage](https://github.com/gabor-boros/pizzlydb/blob/main/docs/roadmap/109-repair-procedures-and-salvage-hardening.md)
///   is later work.
/// - **Single transaction.** The copied records are committed as one
///   [`GraphBatch`](crate::GraphBatch).
/// - **In-memory copy.** The batch holds the namespace that is copied.
///   [Bounded bulk load](https://github.com/gabor-boros/pizzlydb/blob/main/docs/roadmap/110-bounded-memory-bulk-loader.md)
///   is the same limit for scripts.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use std::path::Path;
///
/// use pizzlydb_core::NamespaceId;
/// use pizzlydb_graph::{GraphBatch, GraphStore, salvage};
/// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
///
/// let vfs = MemoryVfs::new();
/// let storage = Storage::create(&vfs, Path::new("src"), 8, Durability::Full, |id| id.fill(1))?;
/// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
/// let mut batch = GraphBatch::begin(&storage)?;
/// let node = graph.create_node(&mut batch)?;
/// batch.commit()?;
/// drop(storage);
///
/// let report = salvage(&vfs, Path::new("src"), Path::new("dst"), graph.namespace())?;
/// assert_eq!(report.nodes, 1);
/// assert!(report.omissions.is_empty());
/// let opened = Storage::open(&vfs, Path::new("dst"), 8, Durability::Full)?;
/// assert_eq!(graph.get_node(&opened.snapshot()?, node)?.id, node);
/// # Ok(())
/// # }
/// ```
pub fn salvage<V: Vfs>(
    vfs: &V,
    source: &Path,
    destination: &Path,
    namespace: NamespaceId,
) -> Result<SalvageReport, GraphError> {
    if conflicts(source, destination) {
        return Err(invalid_path());
    }

    let partial = suffix_path(destination, ".partial");
    let (source_db, wal_omitted) = open_readable(vfs, source, destination)?;
    let view = source_db.snapshot()?;
    let omitted_namespaces = other_namespaces(&view, namespace)?;
    let file_id = fresh_identity()?;
    let dest_db = Storage::create(vfs, &partial, FRAME_COUNT, Durability::Full, |id| {
        *id = file_id;
    })?;
    let graph = GraphStore::new(namespace);
    let mut report = SalvageReport {
        nodes: 0,
        edges: 0,
        omissions: Vec::new(),
        omitted_namespaces,
    };
    if wal_omitted {
        omit(&mut report, OmissionKind::Wal, 0, OmissionReason::Wal);
    }

    {
        let mut batch = GraphBatch::begin(&dest_db)?;
        copy_namespace(&graph, &view, &mut batch, &mut report)?;
        batch.commit()?;
    }
    drop(view);
    drop(source_db);
    drop(dest_db);
    verify_and_publish(vfs, &partial, destination, namespace)?;
    Ok(report)
}

pub(crate) fn fresh_identity() -> Result<[u8; ID_LEN], GraphError> {
    pizzlydb_storage::random_file_id().map_err(GraphError::from)
}

fn other_namespaces<R: KeyRead>(
    read: &R,
    kept: NamespaceId,
) -> Result<Vec<NamespaceId>, GraphError> {
    let mut found = BTreeSet::new();
    let scan = read.scan(&[])?;
    for item in scan {
        let (key, _) = item.map_err(GraphError::Storage)?;
        let Some(bytes) = key.get(..ID_LEN) else {
            continue;
        };

        let mut id_bytes = [0u8; ID_LEN];
        id_bytes.copy_from_slice(bytes);

        let Ok(id) = NamespaceId::from_bytes(id_bytes) else {
            continue;
        };

        if id != kept {
            found.insert(id);
        }
    }
    Ok(found.into_iter().collect())
}

pub(crate) fn verify_and_publish<V: Vfs>(
    vfs: &V,
    partial: &Path,
    destination: &Path,
    namespace: NamespaceId,
) -> Result<(), GraphError> {
    let graph = GraphStore::new(namespace);
    {
        let opened = Storage::open_read_only(vfs, partial, FRAME_COUNT, Durability::Full)?;
        let view = opened.snapshot()?;
        verify_full(&graph, &view)?;
    }
    pizzlydb_storage::publish(vfs, partial, destination)?;
    Ok(())
}

pub(crate) fn suffix_path(path: &Path, suffix: &str) -> PathBuf {
    let mut out = path.to_path_buf();
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    out.set_file_name(name);
    out
}

pub(crate) fn wal_path(path: &Path) -> PathBuf {
    suffix_path(path, "-wal")
}

struct Copied {
    labels: BTreeMap<LabelId, LabelId>,
    types: BTreeMap<TypeId, TypeId>,
    keys: BTreeMap<PropertyKeyId, PropertyKeyId>,
    nodes: BTreeMap<NodeId, NodeId>,
    edges: BTreeMap<EdgeId, EdgeId>,
}

fn copy_namespace<R: KeyRead, V: Vfs>(
    graph: &GraphStore,
    source: &R,
    batch: &mut GraphBatch<'_, V>,
    report: &mut SalvageReport,
) -> Result<(), GraphError> {
    let mut copied = Copied {
        labels: BTreeMap::new(),
        types: BTreeMap::new(),
        keys: BTreeMap::new(),
        nodes: BTreeMap::new(),
        edges: BTreeMap::new(),
    };
    copy_names::<LabelId, _, _>(
        graph,
        source,
        batch,
        id_kind::LABEL,
        OmissionKind::Label,
        &mut copied.labels,
        report,
    )?;
    copy_names::<TypeId, _, _>(
        graph,
        source,
        batch,
        id_kind::TYPE,
        OmissionKind::Type,
        &mut copied.types,
        report,
    )?;
    copy_names::<PropertyKeyId, _, _>(
        graph,
        source,
        batch,
        id_kind::PROPERTY_KEY,
        OmissionKind::PropertyKey,
        &mut copied.keys,
        report,
    )?;
    copy_nodes(graph, source, batch, &mut copied, report)?;
    copy_label_sets(graph, source, batch, &copied, report)?;
    copy_declarations(graph, source, batch, &copied, report)?;
    copy_owner_properties(graph, source, batch, &copied, report, false)?;
    copy_edges(graph, source, batch, &mut copied, report)?;
    copy_owner_properties(graph, source, batch, &copied, report, true)?;
    copy_indexes(graph, source, batch, &copied, report)?;
    Ok(())
}

fn copy_names<I, R, V>(
    graph: &GraphStore,
    source: &R,
    batch: &mut GraphBatch<'_, V>,
    kind_byte: u8,
    kind: OmissionKind,
    copied: &mut BTreeMap<I, I>,
    report: &mut SalvageReport,
) -> Result<(), GraphError>
where
    I: CatalogId + Ord,
    R: KeyRead,
    V: Vfs,
{
    let ids = IdScan::<_, I>::new(source, keys::name_id_keys(graph.namespace(), kind_byte))?;
    for id in ids {
        let id = match id {
            Ok(id) => id,
            Err(GraphError::Storage(err)) => return Err(GraphError::Storage(err)),
            Err(_) => {
                omit(report, kind, 0, OmissionReason::Corrupt);
                continue;
            }
        };
        let name = match graph.name(source, id) {
            Ok(name) => name,
            Err(GraphError::Storage(err)) => return Err(GraphError::Storage(err)),
            Err(_) => {
                omit(report, kind, id_number(id), OmissionReason::Corrupt);
                continue;
            }
        };
        let created = match graph.intern(batch, name.as_bytes()) {
            Ok(created) => created,
            Err(GraphError::Storage(err)) => return Err(GraphError::Storage(err)),
            Err(_) => {
                omit(report, kind, id_number(id), OmissionReason::Corrupt);
                continue;
            }
        };
        if copied.values().any(|existing| *existing == created) {
            omit(report, kind, id_number(id), OmissionReason::Corrupt);
        } else {
            copied.insert(id, created);
        }
    }
    Ok(())
}

fn copy_nodes<R: KeyRead, V: Vfs>(
    graph: &GraphStore,
    source: &R,
    batch: &mut GraphBatch<'_, V>,
    copied: &mut Copied,
    report: &mut SalvageReport,
) -> Result<(), GraphError> {
    let ids = IdScan::<_, NodeId>::new(source, keys::node_keys(graph.namespace()))?;
    for id in ids {
        let id = match id {
            Ok(id) => id,
            Err(GraphError::Storage(err)) => return Err(GraphError::Storage(err)),
            Err(_) => {
                omit(report, OmissionKind::Node, 0, OmissionReason::Corrupt);
                continue;
            }
        };
        match graph.get_node(source, id) {
            Ok(_) => {
                let created = graph.create_node(batch)?;
                copied.nodes.insert(id, created);
                report.nodes += 1;
            }
            Err(GraphError::NotFound | GraphError::Deleted) => {}
            Err(GraphError::Storage(err)) => return Err(GraphError::Storage(err)),
            Err(_) => omit(
                report,
                OmissionKind::Node,
                id.to_u128(),
                OmissionReason::Corrupt,
            ),
        }
    }
    Ok(())
}

fn copy_label_sets<R: KeyRead, V: Vfs>(
    graph: &GraphStore,
    source: &R,
    batch: &mut GraphBatch<'_, V>,
    copied: &Copied,
    report: &mut SalvageReport,
) -> Result<(), GraphError> {
    for (&src, &dst) in &copied.nodes {
        let labels = match collect_labels(graph, source, src) {
            Ok(labels) => labels,
            Err(GraphError::Storage(err)) => return Err(GraphError::Storage(err)),
            Err(_) => {
                omit(
                    report,
                    OmissionKind::Label,
                    src.to_u128(),
                    OmissionReason::Corrupt,
                );
                continue;
            }
        };
        let mut kept = Vec::new();
        for label in labels {
            match copied.labels.get(&label) {
                Some(mapped) => kept.push(*mapped),
                None => omit(
                    report,
                    OmissionKind::Label,
                    label.to_u128(),
                    OmissionReason::Missing,
                ),
            }
        }
        if !kept.is_empty() {
            graph.set_labels(batch, dst, &kept)?;
        }
    }
    Ok(())
}

fn collect_labels<R: KeyRead>(
    graph: &GraphStore,
    source: &R,
    node: NodeId,
) -> Result<Vec<LabelId>, GraphError> {
    graph.labels(source, node)?.collect()
}

fn copy_declarations<R: KeyRead, V: Vfs>(
    graph: &GraphStore,
    source: &R,
    batch: &mut GraphBatch<'_, V>,
    copied: &Copied,
    report: &mut SalvageReport,
) -> Result<(), GraphError> {
    let prefix = keys::declaration_keys(graph.namespace());
    let expected = prefix.len() + 1 + 2 * ID_LEN;
    for entry in PrefixScan::new(source, prefix.clone())? {
        let (key, _) = match entry {
            Ok(entry) => entry,
            Err(GraphError::Storage(err)) => return Err(GraphError::Storage(err)),
            Err(_) => {
                omit(report, OmissionKind::Property, 0, OmissionReason::Corrupt);
                continue;
            }
        };
        if key.len() != expected {
            omit(report, OmissionKind::Property, 0, OmissionReason::Corrupt);
            continue;
        }
        let scope_at = prefix.len() + 1;
        let property = id_at::<PropertyKeyId>(&key, scope_at + ID_LEN).ok();
        let scope = key
            .get(prefix.len())
            .copied()
            .and_then(|kind| Scope::decode(kind, &key, scope_at).ok());
        let (Some(scope), Some(property)) = (scope, property) else {
            omit(report, OmissionKind::Property, 0, OmissionReason::Corrupt);
            continue;
        };
        let Some(mapped_scope) = map_scope(scope, copied) else {
            omit(
                report,
                OmissionKind::Property,
                property.to_u128(),
                OmissionReason::Missing,
            );
            continue;
        };
        let Some(&mapped_key) = copied.keys.get(&property) else {
            omit(
                report,
                OmissionKind::Property,
                property.to_u128(),
                OmissionReason::Missing,
            );
            continue;
        };
        let declaration = match graph.declaration(source, scope, property) {
            Ok(Some(declaration)) => declaration,
            Ok(None) => {
                omit(
                    report,
                    OmissionKind::Property,
                    property.to_u128(),
                    OmissionReason::Corrupt,
                );
                continue;
            }
            Err(GraphError::Storage(err)) => return Err(GraphError::Storage(err)),
            Err(_) => {
                omit(
                    report,
                    OmissionKind::Property,
                    property.to_u128(),
                    OmissionReason::Corrupt,
                );
                continue;
            }
        };
        graph.declare(batch, mapped_scope, mapped_key, declaration)?;
    }
    Ok(())
}

fn copy_owner_properties<R: KeyRead, V: Vfs>(
    graph: &GraphStore,
    source: &R,
    batch: &mut GraphBatch<'_, V>,
    copied: &Copied,
    report: &mut SalvageReport,
    edges: bool,
) -> Result<(), GraphError> {
    if edges {
        for (&src, &dst) in &copied.edges {
            copy_properties(
                graph,
                source,
                batch,
                copied,
                report,
                (Owner::Edge(src), Owner::Edge(dst), src.to_u128()),
            )?;
        }
    } else {
        for (&src, &dst) in &copied.nodes {
            copy_properties(
                graph,
                source,
                batch,
                copied,
                report,
                (Owner::Node(src), Owner::Node(dst), src.to_u128()),
            )?;
        }
    }
    Ok(())
}

fn copy_properties<R: KeyRead, V: Vfs>(
    graph: &GraphStore,
    source: &R,
    batch: &mut GraphBatch<'_, V>,
    copied: &Copied,
    report: &mut SalvageReport,
    ends: (Owner, Owner, u128),
) -> Result<(), GraphError> {
    let (src, dst, entity) = ends;
    let mut found = Vec::new();
    for declared in [true, false] {
        let scan =
            IdScan::<_, PropertyKeyId>::new(source, keys::cells(graph.namespace(), declared, src))?;
        for key in scan {
            match key {
                Ok(key) => found.push(key),
                Err(GraphError::Storage(err)) => return Err(GraphError::Storage(err)),
                Err(_) => {
                    omit(
                        report,
                        OmissionKind::Property,
                        entity,
                        OmissionReason::Corrupt,
                    );
                    return Ok(());
                }
            }
        }
    }
    found.sort_unstable();
    found.dedup();
    for key in found {
        let value = match graph.get_property(source, src, key) {
            Ok(Some(value)) => value,
            Ok(None) => continue,
            Err(GraphError::Storage(err)) => return Err(GraphError::Storage(err)),
            Err(_) => {
                omit(
                    report,
                    OmissionKind::Property,
                    entity,
                    OmissionReason::Corrupt,
                );
                return Ok(());
            }
        };
        let Some(&mapped) = copied.keys.get(&key) else {
            omit(
                report,
                OmissionKind::Property,
                key.to_u128(),
                OmissionReason::Missing,
            );
            continue;
        };
        graph.set_property(batch, dst, mapped, &value)?;
    }
    Ok(())
}

fn copy_edges<R: KeyRead, V: Vfs>(
    graph: &GraphStore,
    source: &R,
    batch: &mut GraphBatch<'_, V>,
    copied: &mut Copied,
    report: &mut SalvageReport,
) -> Result<(), GraphError> {
    let ids = IdScan::<_, EdgeId>::new(source, keys::edge_keys(graph.namespace()))?;
    for id in ids {
        let id = match id {
            Ok(id) => id,
            Err(GraphError::Storage(err)) => return Err(GraphError::Storage(err)),
            Err(_) => {
                omit(report, OmissionKind::Edge, 0, OmissionReason::Corrupt);
                continue;
            }
        };
        let record = match graph.get_edge(source, id) {
            Ok(record) => record,
            Err(GraphError::NotFound | GraphError::Deleted) => continue,
            Err(GraphError::Storage(err)) => return Err(GraphError::Storage(err)),
            Err(_) => {
                omit(
                    report,
                    OmissionKind::Edge,
                    id.to_u128(),
                    OmissionReason::Corrupt,
                );
                continue;
            }
        };
        let Some(&source_node) = copied.nodes.get(&record.source) else {
            omit(
                report,
                OmissionKind::Edge,
                id.to_u128(),
                OmissionReason::Missing,
            );
            continue;
        };
        let Some(&target) = copied.nodes.get(&record.target) else {
            omit(
                report,
                OmissionKind::Edge,
                id.to_u128(),
                OmissionReason::Missing,
            );
            continue;
        };
        let Some(&edge_type) = copied.types.get(&record.edge_type) else {
            omit(
                report,
                OmissionKind::Edge,
                id.to_u128(),
                OmissionReason::Missing,
            );
            continue;
        };
        let created = graph.create_edge(batch, source_node, target, edge_type)?;
        copied.edges.insert(id, created);
        report.edges += 1;
    }
    Ok(())
}

fn copy_indexes<R: KeyRead, V: Vfs>(
    graph: &GraphStore,
    source: &R,
    batch: &mut GraphBatch<'_, V>,
    copied: &Copied,
    report: &mut SalvageReport,
) -> Result<(), GraphError> {
    let ids = IdScan::<_, IndexId>::new(source, keys::index_keys(graph.namespace()))?;
    for id in ids {
        let id = match id {
            Ok(id) => id,
            Err(GraphError::Storage(err)) => return Err(GraphError::Storage(err)),
            Err(_) => {
                omit(report, OmissionKind::Index, 0, OmissionReason::Corrupt);
                continue;
            }
        };
        let definition = match graph.index(source, id) {
            Ok(definition) => definition,
            Err(GraphError::Storage(err)) => return Err(GraphError::Storage(err)),
            Err(_) => {
                omit(
                    report,
                    OmissionKind::Index,
                    id.to_u128(),
                    OmissionReason::Corrupt,
                );
                continue;
            }
        };
        let Some(scope) = map_scope(definition.scope, copied) else {
            omit(
                report,
                OmissionKind::Index,
                id.to_u128(),
                OmissionReason::Missing,
            );
            continue;
        };
        let Some(&key) = copied.keys.get(&definition.key) else {
            omit(
                report,
                OmissionKind::Index,
                id.to_u128(),
                OmissionReason::Missing,
            );
            continue;
        };
        graph.create_index(batch, scope, key, definition.unique)?;
    }
    Ok(())
}

fn map_scope(scope: Scope, copied: &Copied) -> Option<Scope> {
    match scope {
        Scope::Label(id) => copied.labels.get(&id).copied().map(Scope::Label),
        Scope::Type(id) => copied.types.get(&id).copied().map(Scope::Type),
    }
}

fn open_readable<V: Vfs>(
    vfs: &V,
    source: &Path,
    destination: &Path,
) -> Result<(Storage<V>, bool), GraphError> {
    match Storage::open_read_only(vfs, source, FRAME_COUNT, Durability::Full) {
        Ok(db) => Ok((db, false)),
        Err(err) => {
            let copy = suffix_path(destination, ".source");
            if export_checkpoint(vfs, source, &copy).is_err() {
                return Err(err.into());
            }
            let db = Storage::open(vfs, &copy, FRAME_COUNT, Durability::Full)?;
            Ok((db, true))
        }
    }
}

fn export_checkpoint<V: Vfs>(vfs: &V, source: &Path, copy: &Path) -> Result<(), StorageError> {
    pizzlydb_storage::export_checkpoint(vfs, source, copy)
}

fn conflicts(source: &Path, destination: &Path) -> bool {
    if source == destination {
        return true;
    }
    let partial = suffix_path(destination, ".partial");
    let exported = suffix_path(destination, ".source");
    let source_wal = wal_path(source);
    [
        partial.clone(),
        wal_path(&partial),
        exported.clone(),
        wal_path(&exported),
    ]
    .iter()
    .any(|path| path == source || path == &source_wal)
}

fn invalid_path() -> GraphError {
    GraphError::Storage(StorageError::Io(VfsError::InvalidPath))
}

fn id_number<I: Identifier>(id: I) -> u128 {
    u128::from_be_bytes(*id.as_bytes())
}

fn omit(report: &mut SalvageReport, kind: OmissionKind, source_id: u128, reason: OmissionReason) {
    report.omissions.push(Omission {
        kind,
        source_id,
        reason,
    });
}

pub(crate) fn absent<V: Vfs>(vfs: &V, path: &Path, role: FileRole) -> Result<(), GraphError> {
    match vfs.open(path, role, &OpenOptions::new().read_only(true)) {
        Err(VfsError::NotFound) => Ok(()),
        Ok(_) => Err(GraphError::Storage(StorageError::Io(
            VfsError::AlreadyExists,
        ))),
        Err(err) => Err(GraphError::Storage(err.into())),
    }
}
