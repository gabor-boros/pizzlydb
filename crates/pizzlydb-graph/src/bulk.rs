//! Load a graph from a line-oriented script.
//!
//! [`bulk_load`] parses the script, writes a new database through the graph
//! API, and publishes it only after [`verify_full`](crate::verify_full)
//! succeeds. A script that fails validation does not create the destination.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::path::{Path, PathBuf};

use pizzlydb_core::{EdgeId, LabelId, NamespaceId, NodeId, PropertyKeyId, TypeId, Value};
use pizzlydb_storage::{
    Durability, FileRole, OpenOptions, Storage, StorageError, Vfs, VfsError, VfsFile,
};

use crate::batch::GraphTxn;
use crate::catalog;
use crate::error::GraphError;
use crate::property::Scope;
use crate::record_store::GraphStore;
use crate::salvage::{self, absent, verify_and_publish};

const FRAME_COUNT: usize = 32;
const PHASE_NAMES: u32 = 0;
const PHASE_NODES: u32 = 1;
const PHASE_LABELS: u32 = 2;
const PHASE_NODE_PROPS: u32 = 3;
const PHASE_EDGES: u32 = 4;
const PHASE_EDGE_PROPS: u32 = 5;
const PHASE_INDEXES: u32 = 6;

/// Budget for sorting one bulk-load script.
///
/// # Examples
///
/// ```
/// use pizzlydb_graph::LoadOptions;
///
/// let options = LoadOptions { temp_budget: 4096 };
/// assert_eq!(options.temp_budget, 4096);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoadOptions {
    /// Maximum bytes of one in-memory sort run, counting `line length + 8`.
    pub temp_budget: u64,
}

impl Default for LoadOptions {
    fn default() -> Self {
        Self {
            temp_budget: u64::MAX,
        }
    }
}

impl LoadOptions {
    /// Options that keep the whole script in one run.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            temp_budget: u64::MAX,
        }
    }
}

/// Commands applied by [`bulk_load`].
///
/// The counts are commands in the script. Spilling a run does not change them.
/// `temp_bytes` is zero when every command fit in the budget.
///
/// # Examples
///
/// ```
/// use pizzlydb_graph::LoadReport;
///
/// let report = LoadReport {
///     names: 1,
///     nodes: 1,
///     label_sets: 0,
///     node_properties: 0,
///     edges: 0,
///     edge_properties: 0,
///     indexes: 0,
///     temp_bytes: 0,
/// };
/// assert_eq!(report.names, 1);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoadReport {
    /// `label`, `type`, and `key` commands.
    pub names: u64,
    /// `node` commands.
    pub nodes: u64,
    /// `labels` commands.
    pub label_sets: u64,
    /// `prop node` commands.
    pub node_properties: u64,
    /// `edge` commands.
    pub edges: u64,
    /// `prop edge` commands.
    pub edge_properties: u64,
    /// `index` commands.
    pub indexes: u64,
    /// Bytes written to sort files.
    pub temp_bytes: u64,
}

/// Create `destination` from `script` in `namespace`.
///
/// Blank lines and lines whose first non-space character is `#` are ignored.
/// Commands are applied by phase, and in input order within a phase: names,
/// nodes, label sets, node properties, edges, edge properties, then indexes.
/// Engine identifiers are assigned by that order. The same calls in that order
/// produce the same identifiers.
///
/// An existing destination file or log is left unchanged. Validation runs
/// before any destination file is created. The destination file identity is
/// drawn from a CSPRNG.
///
/// # Limitations
///
/// - **Offline only.** Nothing excludes a live writer at `destination`.
///   [Read-only open](https://github.com/gabor-boros/pizzlydb/blob/main/docs/roadmap/015-exclusive-ownership-and-read-only-open.md)
///   is later work.
/// - **Single namespace.** Every record is written into `namespace`.
/// - **Single transaction.** Names, nodes, edges, properties, and indexes are
///   committed as one [`GraphBatch`](crate::GraphBatch).
///   [Bounded bulk load](https://github.com/gabor-boros/pizzlydb/blob/main/docs/roadmap/110-bounded-memory-bulk-loader.md)
///   is later work.
/// - **In-memory input.** `script` is one string. Sort runs may spill when
///   `temp_budget` is set, and the input itself is not streamed.
///
/// # Errors
///
/// Returns [`GraphError::NameConflict`] for a duplicate name or external id,
/// [`GraphError::NotFound`] for an unknown name, node, edge, or key,
/// [`GraphError::EndpointNotLive`] when an edge endpoint was not declared,
/// [`GraphError::InvalidValue`] for a malformed line or value, and
/// [`GraphError::Storage`] with [`StorageError::Full`] when one line exceeds
/// `temp_budget`.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use std::path::Path;
///
/// use pizzlydb_core::{NamespaceId, NodeId};
/// use pizzlydb_graph::{GraphTxn, GraphStore, LoadOptions, bulk_load};
/// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
///
/// let vfs = MemoryVfs::new();
/// let script = "label Person\nnode a\nlabels a Person\n";
/// let report = bulk_load(
///     &vfs,
///     script,
///     Path::new("db"),
///     NamespaceId::from_u128(1)?,
///     LoadOptions::new(),
/// )?;
/// assert_eq!(report.nodes, 1);
///
/// let storage = Storage::open(&vfs, Path::new("db"), 8, Durability::Full)?;
/// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
/// let node = NodeId::from_u128(1)?;
/// assert_eq!(graph.labels(&storage.snapshot()?, node)?.count(), 1);
/// # Ok(())
/// # }
/// ```
pub fn bulk_load<V: Vfs + 'static>(
    vfs: &V,
    script: &str,
    destination: &Path,
    namespace: NamespaceId,
    options: LoadOptions,
) -> Result<LoadReport, GraphError> {
    let mut report = validate(script, options.temp_budget)?;
    absent(vfs, destination, FileRole::Main)?;
    absent(vfs, &salvage::wal_path(destination), FileRole::Wal)?;

    let partial = salvage::suffix_path(destination, ".partial");
    let (runs, temp_bytes) = collect_runs(vfs, script, destination, options.temp_budget)?;
    report.temp_bytes = temp_bytes;

    let file_id = salvage::fresh_identity()?;
    let storage = Storage::create(vfs, &partial, FRAME_COUNT, Durability::Full, |id| {
        *id = file_id;
    })?;
    let graph = GraphStore::new(namespace);
    {
        let mut batch = GraphTxn::begin(&storage)?;
        apply_runs(vfs, &graph, &mut batch, script, &runs, options.temp_budget)?;
        batch.commit()?;
    }
    drop(storage);
    verify_and_publish(vfs, &partial, destination, namespace)?;
    for path in &runs {
        let _ = vfs.remove(path, FileRole::Temp);
    }
    Ok(report)
}

#[derive(Clone, Debug)]
enum Command {
    Name(NameKind, String),
    Node(String),
    Labels {
        node: String,
        labels: Vec<String>,
    },
    Edge {
        id: String,
        source: String,
        target: String,
        edge_type: String,
    },
    Prop {
        on_edge: bool,
        id: String,
        key: String,
        value: Value,
    },
    Index {
        on_edge: bool,
        scope: String,
        key: String,
    },
}

#[derive(Clone, Copy, Debug)]
enum NameKind {
    Label,
    Type,
    Key,
}

impl Command {
    fn phase(&self) -> u32 {
        match self {
            Self::Name(_, _) => PHASE_NAMES,
            Self::Node(_) => PHASE_NODES,
            Self::Labels { .. } => PHASE_LABELS,
            Self::Prop { on_edge: false, .. } => PHASE_NODE_PROPS,
            Self::Edge { .. } => PHASE_EDGES,
            Self::Prop { on_edge: true, .. } => PHASE_EDGE_PROPS,
            Self::Index { .. } => PHASE_INDEXES,
        }
    }
}

struct Names {
    labels: BTreeSet<String>,
    types: BTreeSet<String>,
    keys: BTreeSet<String>,
    nodes: BTreeSet<String>,
    edges: BTreeSet<String>,
}

struct Item {
    phase: u32,
    seq: u32,
    line: String,
}

fn validate(script: &str, budget: u64) -> Result<LoadReport, GraphError> {
    let mut names = Names {
        labels: BTreeSet::new(),
        types: BTreeSet::new(),
        keys: BTreeSet::new(),
        nodes: BTreeSet::new(),
        edges: BTreeSet::new(),
    };
    let mut report = LoadReport {
        names: 0,
        nodes: 0,
        label_sets: 0,
        node_properties: 0,
        edges: 0,
        edge_properties: 0,
        indexes: 0,
        temp_bytes: 0,
    };
    for (index, line) in content_lines(script).enumerate() {
        check_budget(line, budget)?;
        line_seq(index)?;
        let command = parse_line(line)?;
        remember(&mut names, &command)?;
        count(&mut report, &command);
    }
    for line in content_lines(script) {
        check_refs(&names, &parse_line(line)?)?;
    }
    Ok(report)
}

fn remember(names: &mut Names, command: &Command) -> Result<(), GraphError> {
    match command {
        Command::Name(NameKind::Label, name) => insert(&mut names.labels, name),
        Command::Name(NameKind::Type, name) => insert(&mut names.types, name),
        Command::Name(NameKind::Key, name) => insert(&mut names.keys, name),
        Command::Node(id) => insert(&mut names.nodes, id),
        Command::Edge { id, .. } => insert(&mut names.edges, id),
        Command::Labels { .. } | Command::Prop { .. } | Command::Index { .. } => Ok(()),
    }
}

fn insert(set: &mut BTreeSet<String>, name: &str) -> Result<(), GraphError> {
    if set.insert(name.to_owned()) {
        Ok(())
    } else {
        Err(GraphError::NameConflict)
    }
}

fn count(report: &mut LoadReport, command: &Command) {
    match command.phase() {
        PHASE_NAMES => report.names += 1,
        PHASE_NODES => report.nodes += 1,
        PHASE_LABELS => report.label_sets += 1,
        PHASE_NODE_PROPS => report.node_properties += 1,
        PHASE_EDGES => report.edges += 1,
        PHASE_EDGE_PROPS => report.edge_properties += 1,
        _ => report.indexes += 1,
    }
}

fn check_refs(names: &Names, command: &Command) -> Result<(), GraphError> {
    match command {
        Command::Labels { node, labels } => {
            require_set(&names.nodes, node)?;
            for label in labels {
                require_set(&names.labels, label)?;
            }
            Ok(())
        }
        Command::Edge {
            source,
            target,
            edge_type,
            ..
        } => {
            if !names.nodes.contains(source) || !names.nodes.contains(target) {
                return Err(GraphError::EndpointNotLive);
            }
            require_set(&names.types, edge_type)
        }
        Command::Prop {
            on_edge, id, key, ..
        } => {
            if *on_edge {
                require_set(&names.edges, id)?;
            } else {
                require_set(&names.nodes, id)?;
            }
            require_set(&names.keys, key)
        }
        Command::Index {
            on_edge,
            scope,
            key,
        } => {
            if *on_edge {
                require_set(&names.types, scope)?;
            } else {
                require_set(&names.labels, scope)?;
            }
            require_set(&names.keys, key)
        }
        Command::Name(_, _) | Command::Node(_) => Ok(()),
    }
}

fn require_set(set: &BTreeSet<String>, name: &str) -> Result<(), GraphError> {
    if set.contains(name) {
        Ok(())
    } else {
        Err(GraphError::NotFound)
    }
}

fn collect_runs<V: Vfs>(
    vfs: &V,
    script: &str,
    destination: &Path,
    budget: u64,
) -> Result<(Vec<PathBuf>, u64), GraphError> {
    let mut run = Vec::new();
    let mut weight = 0u64;
    let mut spilled = false;
    let mut paths = Vec::new();
    let mut temp_bytes = 0u64;
    for (index, line) in content_lines(script).enumerate() {
        let line_weight = check_budget(line, budget)?;
        if weight > 0 && weight.saturating_add(line_weight) > budget {
            temp_bytes = add_bytes(temp_bytes, spill(vfs, destination, paths.len(), &mut run)?)?;
            paths.push(sort_path(destination, paths.len()));
            weight = 0;
            spilled = true;
        }
        let command = parse_line(line)?;
        run.push(Item {
            phase: command.phase(),
            seq: line_seq(index)?,
            line: line.to_owned(),
        });
        weight = weight.saturating_add(line_weight);
    }
    if spilled && !run.is_empty() {
        temp_bytes = add_bytes(temp_bytes, spill(vfs, destination, paths.len(), &mut run)?)?;
        paths.push(sort_path(destination, paths.len()));
    }
    Ok((paths, temp_bytes))
}

fn spill<V: Vfs>(
    vfs: &V,
    destination: &Path,
    index: usize,
    items: &mut Vec<Item>,
) -> Result<u64, GraphError> {
    items.sort_by_key(|item| (item.phase, item.seq));
    let path = sort_path(destination, index);
    let mut bytes = Vec::new();
    push_u32(
        &mut bytes,
        u32::try_from(items.len()).map_err(|_| GraphError::InvalidValue)?,
    );
    for item in items.drain(..) {
        push_u32(&mut bytes, item.phase);
        push_u32(&mut bytes, item.seq);
        let len = u32::try_from(item.line.len()).map_err(|_| GraphError::InvalidValue)?;
        push_u32(&mut bytes, len);
        bytes.extend(item.line.into_bytes());
    }
    let mut file = vfs
        .open(
            &path,
            FileRole::Temp,
            &OpenOptions::new().create(true).exclusive(true),
        )
        .map_err(io_err)?;
    file.write_all(0, &bytes).map_err(io_err)?;
    file.sync().map_err(io_err)?;
    u64::try_from(bytes.len()).map_err(|_| GraphError::InvalidValue)
}

fn apply_runs<V: Vfs>(
    vfs: &V,
    graph: &GraphStore,
    batch: &mut GraphTxn<V>,
    script: &str,
    runs: &[PathBuf],
    budget: u64,
) -> Result<(), GraphError> {
    if runs.is_empty() {
        let mut items = memory_items(script, budget)?;
        items.sort_by_key(|item| (item.phase, item.seq));
        return apply_items(graph, batch, &items);
    }

    let mut readers = Vec::new();
    let mut heap = BinaryHeap::new();
    for (index, path) in runs.iter().enumerate() {
        let mut reader = open_spill(vfs, path)?;
        if let Some(item) = reader.next_item()? {
            heap.push(HeapItem::from_item(item, index));
        }
        readers.push(reader);
    }
    let mut ids = Ids {
        nodes: BTreeMap::new(),
        edges: BTreeMap::new(),
    };
    while let Some(item) = heap.pop() {
        apply_command(graph, batch, &mut ids, &parse_line(&item.line)?)?;
        if let Some(next) = readers[item.run].next_item()? {
            heap.push(HeapItem::from_item(next, item.run));
        }
    }
    Ok(())
}

fn memory_items(script: &str, budget: u64) -> Result<Vec<Item>, GraphError> {
    let mut items = Vec::new();
    for (index, line) in content_lines(script).enumerate() {
        check_budget(line, budget)?;
        let command = parse_line(line)?;
        items.push(Item {
            phase: command.phase(),
            seq: line_seq(index)?,
            line: line.to_owned(),
        });
    }
    Ok(items)
}

fn apply_items<V: Vfs>(
    graph: &GraphStore,
    batch: &mut GraphTxn<V>,
    items: &[Item],
) -> Result<(), GraphError> {
    let mut ids = Ids {
        nodes: BTreeMap::new(),
        edges: BTreeMap::new(),
    };
    for item in items {
        apply_command(graph, batch, &mut ids, &parse_line(&item.line)?)?;
    }
    Ok(())
}

struct Ids {
    nodes: BTreeMap<String, NodeId>,
    edges: BTreeMap<String, EdgeId>,
}

fn apply_command<V: Vfs>(
    graph: &GraphStore,
    batch: &mut GraphTxn<V>,
    ids: &mut Ids,
    command: &Command,
) -> Result<(), GraphError> {
    match command {
        Command::Name(NameKind::Label, name) => {
            graph.intern::<LabelId, _>(batch, name)?;
        }
        Command::Name(NameKind::Type, name) => {
            graph.intern::<TypeId, _>(batch, name)?;
        }
        Command::Name(NameKind::Key, name) => {
            graph.intern::<PropertyKeyId, _>(batch, name)?;
        }
        Command::Node(id) => {
            let node = graph.create_node(batch)?;
            ids.nodes.insert(id.clone(), node);
        }
        Command::Labels { node, labels } => {
            let node = *ids.nodes.get(node).ok_or(GraphError::NotFound)?;
            let mut mapped = Vec::new();
            for label in labels {
                mapped.push(lookup_label(graph, batch, label)?);
            }
            graph.set_labels(batch, node, &mapped)?;
        }
        Command::Edge {
            id,
            source,
            target,
            edge_type,
        } => {
            let source = *ids.nodes.get(source).ok_or(GraphError::EndpointNotLive)?;
            let target = *ids.nodes.get(target).ok_or(GraphError::EndpointNotLive)?;
            let edge_type = lookup_type(graph, batch, edge_type)?;
            let edge = graph.create_edge(batch, source, target, edge_type)?;
            ids.edges.insert(id.clone(), edge);
        }
        Command::Prop {
            on_edge,
            id,
            key,
            value,
        } => {
            let key = lookup_key(graph, batch, key)?;
            if *on_edge {
                let edge = *ids.edges.get(id).ok_or(GraphError::NotFound)?;
                graph.set_property(batch, edge, key, value)?;
            } else {
                let node = *ids.nodes.get(id).ok_or(GraphError::NotFound)?;
                graph.set_property(batch, node, key, value)?;
            }
        }
        Command::Index {
            on_edge,
            scope,
            key,
        } => {
            let key = lookup_key(graph, batch, key)?;
            let scope = if *on_edge {
                Scope::Type(lookup_type(graph, batch, scope)?)
            } else {
                Scope::Label(lookup_label(graph, batch, scope)?)
            };
            graph.create_index(batch, scope, key, false)?;
        }
    }
    Ok(())
}

fn lookup_label<V: Vfs>(
    graph: &GraphStore,
    batch: &GraphTxn<V>,
    name: &str,
) -> Result<LabelId, GraphError> {
    graph.lookup(batch, name)?.ok_or(GraphError::NotFound)
}

fn lookup_type<V: Vfs>(
    graph: &GraphStore,
    batch: &GraphTxn<V>,
    name: &str,
) -> Result<TypeId, GraphError> {
    graph.lookup(batch, name)?.ok_or(GraphError::NotFound)
}

fn lookup_key<V: Vfs>(
    graph: &GraphStore,
    batch: &GraphTxn<V>,
    name: &str,
) -> Result<PropertyKeyId, GraphError> {
    graph.lookup(batch, name)?.ok_or(GraphError::NotFound)
}

struct HeapItem {
    phase: u32,
    seq: u32,
    line: String,
    run: usize,
}

impl HeapItem {
    fn from_item(item: Item, run: usize) -> Self {
        Self {
            phase: item.phase,
            seq: item.seq,
            line: item.line,
            run,
        }
    }
}

impl PartialEq for HeapItem {
    fn eq(&self, other: &Self) -> bool {
        (self.phase, self.seq, self.run) == (other.phase, other.seq, other.run)
    }
}

impl Eq for HeapItem {}

impl PartialOrd for HeapItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapItem {
    fn cmp(&self, other: &Self) -> Ordering {
        (other.phase, other.seq, other.run).cmp(&(self.phase, self.seq, self.run))
    }
}

struct SpillReader<F> {
    file: F,
    left: u32,
    offset: u64,
}

fn open_spill<V: Vfs>(vfs: &V, path: &Path) -> Result<SpillReader<V::File>, GraphError> {
    let mut file = vfs
        .open(path, FileRole::Temp, &OpenOptions::new().read_only(true))
        .map_err(io_err)?;
    let left = read_u32(&mut file, &mut 0)?;
    Ok(SpillReader {
        file,
        left,
        offset: 4,
    })
}

impl<F: VfsFile> SpillReader<F> {
    fn next_item(&mut self) -> Result<Option<Item>, GraphError> {
        if self.left == 0 {
            return Ok(None);
        }
        self.left -= 1;
        let phase = read_u32(&mut self.file, &mut self.offset)?;
        let seq = read_u32(&mut self.file, &mut self.offset)?;
        let len = read_u32(&mut self.file, &mut self.offset)?;
        if phase > PHASE_INDEXES {
            return Err(GraphError::InvalidValue);
        }
        let size = usize::try_from(len).map_err(|_| GraphError::Corrupt)?;
        let mut bytes = vec![0u8; size];
        read_exact(&mut self.file, &mut self.offset, &mut bytes)?;
        let line = String::from_utf8(bytes).map_err(|_| GraphError::Corrupt)?;
        Ok(Some(Item { phase, seq, line }))
    }
}

fn read_u32<F: VfsFile>(file: &mut F, offset: &mut u64) -> Result<u32, GraphError> {
    let mut buf = [0u8; 4];
    read_exact(file, offset, &mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

fn read_exact<F: VfsFile>(
    file: &mut F,
    offset: &mut u64,
    buf: &mut [u8],
) -> Result<(), GraphError> {
    match file.read_exact(*offset, buf) {
        Ok(()) => {
            *offset += u64::try_from(buf.len()).map_err(|_| GraphError::Corrupt)?;
            Ok(())
        }
        Err(VfsError::Truncated) => Err(GraphError::Corrupt),
        Err(err) => Err(io_err(err)),
    }
}

fn content_lines(script: &str) -> impl Iterator<Item = &str> {
    script.split('\n').filter_map(|line| {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let trimmed = line.trim_start_matches([' ', '\t']);
        if trimmed.is_empty() || trimmed.starts_with('#') {
            None
        } else {
            Some(line)
        }
    })
}

fn parse_line(line: &str) -> Result<Command, GraphError> {
    let mut cursor = Cursor::new(line);
    let word = cursor.token().ok_or(GraphError::InvalidValue)?;
    match word {
        "label" => Ok(Command::Name(NameKind::Label, one_name(&mut cursor)?)),
        "type" => Ok(Command::Name(NameKind::Type, one_name(&mut cursor)?)),
        "key" => Ok(Command::Name(NameKind::Key, one_name(&mut cursor)?)),
        "node" => Ok(Command::Node(one_token(&mut cursor)?)),
        "labels" => parse_labels(&mut cursor),
        "edge" => parse_edge(&mut cursor),
        "prop" => parse_prop(&mut cursor),
        "index" => parse_index(&mut cursor),
        _ => Err(GraphError::InvalidValue),
    }
}

fn one_name(cursor: &mut Cursor<'_>) -> Result<String, GraphError> {
    let name = one_token(cursor)?;
    catalog::valid_name(name.as_bytes())?;
    Ok(name)
}

fn one_token(cursor: &mut Cursor<'_>) -> Result<String, GraphError> {
    let token = cursor.token().ok_or(GraphError::InvalidValue)?;
    if cursor.token().is_some() {
        return Err(GraphError::InvalidValue);
    }
    Ok(token.to_owned())
}

fn parse_labels(cursor: &mut Cursor<'_>) -> Result<Command, GraphError> {
    let node = cursor.token().ok_or(GraphError::InvalidValue)?.to_owned();
    let mut labels = Vec::new();
    while let Some(label) = cursor.token() {
        catalog::valid_name(label.as_bytes())?;
        labels.push(label.to_owned());
    }
    if labels.is_empty() {
        return Err(GraphError::InvalidValue);
    }
    Ok(Command::Labels { node, labels })
}

fn parse_edge(cursor: &mut Cursor<'_>) -> Result<Command, GraphError> {
    let id = cursor.token().ok_or(GraphError::InvalidValue)?.to_owned();
    let source = cursor.token().ok_or(GraphError::InvalidValue)?.to_owned();
    let target = cursor.token().ok_or(GraphError::InvalidValue)?.to_owned();
    let edge_type = cursor.token().ok_or(GraphError::InvalidValue)?.to_owned();
    catalog::valid_name(edge_type.as_bytes())?;
    if cursor.token().is_some() {
        return Err(GraphError::InvalidValue);
    }
    Ok(Command::Edge {
        id,
        source,
        target,
        edge_type,
    })
}

fn parse_prop(cursor: &mut Cursor<'_>) -> Result<Command, GraphError> {
    let owner = cursor.token().ok_or(GraphError::InvalidValue)?;
    let on_edge = match owner {
        "node" => false,
        "edge" => true,
        _ => return Err(GraphError::InvalidValue),
    };
    let id = cursor.token().ok_or(GraphError::InvalidValue)?.to_owned();
    let key = cursor.token().ok_or(GraphError::InvalidValue)?.to_owned();
    catalog::valid_name(key.as_bytes())?;
    let value = parse_value(cursor)?;
    Ok(Command::Prop {
        on_edge,
        id,
        key,
        value,
    })
}

fn parse_index(cursor: &mut Cursor<'_>) -> Result<Command, GraphError> {
    let owner = cursor.token().ok_or(GraphError::InvalidValue)?;
    let on_edge = match owner {
        "label" => false,
        "type" => true,
        _ => return Err(GraphError::InvalidValue),
    };
    let scope = cursor.token().ok_or(GraphError::InvalidValue)?.to_owned();
    let key = cursor.token().ok_or(GraphError::InvalidValue)?.to_owned();
    catalog::valid_name(scope.as_bytes())?;
    catalog::valid_name(key.as_bytes())?;
    if cursor.token().is_some() {
        return Err(GraphError::InvalidValue);
    }
    Ok(Command::Index {
        on_edge,
        scope,
        key,
    })
}

fn parse_value(cursor: &mut Cursor<'_>) -> Result<Value, GraphError> {
    let tag = cursor.token().ok_or(GraphError::InvalidValue)?;
    match tag {
        "null" => {
            if cursor.token().is_some() {
                return Err(GraphError::InvalidValue);
            }
            Ok(Value::Null)
        }
        "bool" => {
            let word = cursor.token().ok_or(GraphError::InvalidValue)?;
            if cursor.token().is_some() {
                return Err(GraphError::InvalidValue);
            }
            match word {
                "true" => Ok(Value::Bool(true)),
                "false" => Ok(Value::Bool(false)),
                _ => Err(GraphError::InvalidValue),
            }
        }
        "int" => {
            let word = cursor.token().ok_or(GraphError::InvalidValue)?;
            if cursor.token().is_some() || word.as_bytes().first() == Some(&b'+') {
                return Err(GraphError::InvalidValue);
            }
            word.parse::<i64>()
                .map(Value::Int)
                .map_err(|_| GraphError::InvalidValue)
        }
        "float" => {
            let word = cursor.token().ok_or(GraphError::InvalidValue)?;
            if cursor.token().is_some() {
                return Err(GraphError::InvalidValue);
            }
            let parsed = word.parse::<f64>().map_err(|_| GraphError::InvalidValue)?;
            if parsed.is_nan() {
                return Err(GraphError::InvalidValue);
            }
            let parsed = if parsed == 0.0 { 0.0 } else { parsed };
            Ok(Value::Float(parsed))
        }
        "string" => Ok(Value::from(cursor.string_rest())),
        "bytes" => {
            let word = cursor.token().ok_or(GraphError::InvalidValue)?;
            if cursor.token().is_some() {
                return Err(GraphError::InvalidValue);
            }
            Ok(Value::Bytes(decode_hex(word)?))
        }
        _ => Err(GraphError::InvalidValue),
    }
}

fn decode_hex(text: &str) -> Result<Vec<u8>, GraphError> {
    if text.is_empty() || !text.len().is_multiple_of(2) {
        return Err(GraphError::InvalidValue);
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(text.len() / 2);
    let mut index = 0;
    while index < bytes.len() {
        let hi = hex_digit(bytes[index])?;
        let lo = hex_digit(bytes[index + 1])?;
        out.push((hi << 4) | lo);
        index += 2;
    }
    Ok(out)
}

fn hex_digit(byte: u8) -> Result<u8, GraphError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(GraphError::InvalidValue),
    }
}

struct Cursor<'a> {
    line: &'a str,
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(line: &'a str) -> Self {
        Self { line, pos: 0 }
    }

    fn token(&mut self) -> Option<&'a str> {
        let bytes = self.line.as_bytes();
        while self.pos < bytes.len() && is_sep(bytes[self.pos]) {
            self.pos += 1;
        }
        let start = self.pos;
        if start == bytes.len() {
            return None;
        }
        while self.pos < bytes.len() && !is_sep(bytes[self.pos]) {
            self.pos += 1;
        }
        Some(&self.line[start..self.pos])
    }

    fn string_rest(&mut self) -> &'a str {
        let bytes = self.line.as_bytes();
        if self.pos < bytes.len() && is_sep(bytes[self.pos]) {
            self.pos += 1;
        }
        &self.line[self.pos..]
    }
}

fn is_sep(byte: u8) -> bool {
    byte == b' ' || byte == b'\t'
}

fn check_budget(line: &str, budget: u64) -> Result<u64, GraphError> {
    let len = u64::try_from(line.len()).map_err(|_| full())?;
    let weight = len.checked_add(8).ok_or_else(full)?;
    if weight > budget {
        Err(full())
    } else {
        Ok(weight)
    }
}

fn io_err(err: VfsError) -> GraphError {
    GraphError::Storage(err.into())
}

fn line_seq(index: usize) -> Result<u32, GraphError> {
    match u32::try_from(index) {
        Ok(seq) if seq < u32::MAX => Ok(seq),
        _ => Err(GraphError::InvalidValue),
    }
}

fn full() -> GraphError {
    GraphError::Storage(StorageError::Full)
}

fn add_bytes(total: u64, more: u64) -> Result<u64, GraphError> {
    total.checked_add(more).ok_or_else(full)
}

fn push_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn sort_path(destination: &Path, index: usize) -> PathBuf {
    salvage::suffix_path(destination, &format!(".sort.{index}"))
}
