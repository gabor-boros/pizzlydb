//! Full verification, salvage, and bulk load.

mod common;

use std::path::Path;

use pizzlydb_core::{NamespaceId, NodeId, PropertyKeyId, TypeId, Value, encode_value};
use pizzlydb_graph::{
    Direction, GraphBatch, GraphError, GraphStore, LoadOptions, OmissionKind, OmissionReason,
    Scope, bulk_load, salvage, verify_full,
};
use pizzlydb_storage::{
    Durability, FileRole, MemoryVfs, OpenOptions, Storage, StorageError, Vfs, VfsError, VfsFile,
};

use common::{KIND_AT, PATH, create, dump, graph};

const OUT: &str = "out";

fn file_bytes(vfs: &MemoryVfs, path: &str, role: FileRole) -> Vec<u8> {
    let mut file = vfs
        .open(Path::new(path), role, &OpenOptions::new())
        .unwrap();
    let len = usize::try_from(file.len().unwrap()).unwrap();
    let mut buf = vec![0u8; len];
    if len > 0 {
        file.read_exact(0, &mut buf).unwrap();
    }
    buf
}

fn put_key(storage: &Storage<MemoryVfs>, key: &[u8], value: &[u8]) {
    let mut batch = storage.begin_write().unwrap();
    batch.put(key, value).unwrap();
    batch.commit(Durability::Full).unwrap();
}

fn delete_key(storage: &Storage<MemoryVfs>, key: &[u8]) {
    let mut batch = storage.begin_write().unwrap();
    batch.delete(key).unwrap();
    batch.commit(Durability::Full).unwrap();
}

fn kind_key(view: &impl pizzlydb_graph::KeyRead, kind: u8) -> Vec<u8> {
    dump(view)
        .into_keys()
        .find(|key| key.get(KIND_AT) == Some(&kind))
        .expect("key")
}

struct FailRename {
    inner: MemoryVfs,
}

impl Vfs for FailRename {
    type File = pizzlydb_storage::vfs_memory::MemoryFile;

    fn open(
        &self,
        path: &Path,
        role: FileRole,
        options: &OpenOptions,
    ) -> Result<Self::File, VfsError> {
        self.inner.open(path, role, options)
    }

    fn sync_dir(&self, directory: &Path) -> Result<(), VfsError> {
        self.inner.sync_dir(directory)
    }

    fn rename(&self, _from: &Path, _to: &Path, _role: FileRole) -> Result<(), VfsError> {
        Err(VfsError::Io {
            source: std::io::Error::other("rename failed"),
        })
    }

    fn remove(&self, path: &Path, role: FileRole) -> Result<(), VfsError> {
        self.inner.remove(path, role)
    }
}

fn assert_verify_fails(mutate: impl FnOnce(&Storage<MemoryVfs>, &GraphStore)) {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);
    mutate(&storage, &graph);
    let view = storage.snapshot().unwrap();
    let before = dump(&view);
    assert!(matches!(
        verify_full(&graph, &view),
        Err(GraphError::VerificationFailed)
    ));
    assert_eq!(dump(&view), before);
}

#[test]
fn full_verify_rejects_one_fault_per_structure() {
    assert_verify_fails(|storage, graph| {
        let mut batch = GraphBatch::begin(storage).unwrap();
        let knows: TypeId = graph.intern(&mut batch, "KNOWS").unwrap();
        let a = graph.create_node(&mut batch).unwrap();
        let b = graph.create_node(&mut batch).unwrap();
        graph.create_edge(&mut batch, a, b, knows).unwrap();
        batch.commit().unwrap();
        let view = storage.snapshot().unwrap();
        let key = kind_key(&view, 0x06);
        drop(view);
        delete_key(storage, &key);
    });

    assert_verify_fails(|storage, graph| {
        let mut batch = GraphBatch::begin(storage).unwrap();
        let knows: TypeId = graph.intern(&mut batch, "KNOWS").unwrap();
        let a = graph.create_node(&mut batch).unwrap();
        let b = graph.create_node(&mut batch).unwrap();
        graph.create_edge(&mut batch, a, b, knows).unwrap();
        batch.commit().unwrap();
        let mut key = graph.namespace().to_bytes().to_vec();
        key.push(0x02);
        key.extend_from_slice(a.as_bytes());
        delete_key(storage, &key);
    });

    assert_verify_fails(|storage, graph| {
        let mut batch = GraphBatch::begin(storage).unwrap();
        let person = graph.intern(&mut batch, "Person").unwrap();
        let email = graph.intern(&mut batch, "email").unwrap();
        let a = graph.create_node(&mut batch).unwrap();
        let b = graph.create_node(&mut batch).unwrap();
        graph.add_label(&mut batch, a, person).unwrap();
        graph.add_label(&mut batch, b, person).unwrap();
        graph
            .set_property(&mut batch, a, email, &Value::from("a@example.com"))
            .unwrap();
        graph
            .create_index(&mut batch, Scope::Label(person), email, true)
            .unwrap();
        batch.commit().unwrap();
        let view = storage.snapshot().unwrap();
        let key = kind_key(&view, 0x0F);
        drop(view);
        put_key(storage, &key, b.as_bytes());
    });

    assert_verify_fails(|storage, graph| {
        let mut batch = GraphBatch::begin(storage).unwrap();
        let person = graph.intern(&mut batch, "Person").unwrap();
        let name = graph.intern(&mut batch, "name").unwrap();
        let node = graph.create_node(&mut batch).unwrap();
        graph.add_label(&mut batch, node, person).unwrap();
        graph
            .set_property(&mut batch, node, name, &Value::from("Ada"))
            .unwrap();
        graph
            .create_index(&mut batch, Scope::Label(person), name, false)
            .unwrap();
        batch.commit().unwrap();
        let view = storage.snapshot().unwrap();
        let key = kind_key(&view, 0x0B);
        drop(view);
        put_key(storage, &key, &encode_value(&Value::from("Bob")).unwrap());
    });

    assert_verify_fails(|storage, graph| {
        let mut batch = GraphBatch::begin(storage).unwrap();
        let person = graph.intern(&mut batch, "Person").unwrap();
        let node = graph.create_node(&mut batch).unwrap();
        graph.add_label(&mut batch, node, person).unwrap();
        batch.commit().unwrap();
        let view = storage.snapshot().unwrap();
        let key = kind_key(&view, 0x07);
        drop(view);
        delete_key(storage, &key);
    });
}

#[test]
fn healthy_salvage_preserves_ids_and_source_bytes() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);
    let mut batch = GraphBatch::begin(&storage).unwrap();
    let other = GraphStore::new(NamespaceId::from_u128(2).unwrap());
    let node = graph.create_node(&mut batch).unwrap();
    other.create_node(&mut batch).unwrap();
    batch.commit().unwrap();
    drop(storage);
    let before = file_bytes(&vfs, PATH, FileRole::Main);
    let wal_before = file_bytes(&vfs, "graph-wal", FileRole::Wal);

    let report = salvage(&vfs, Path::new(PATH), Path::new(OUT), graph.namespace()).unwrap();
    assert_eq!(report.nodes, 1);
    assert!(report.omissions.is_empty());
    assert_eq!(file_bytes(&vfs, PATH, FileRole::Main), before);
    assert_eq!(file_bytes(&vfs, "graph-wal", FileRole::Wal), wal_before);

    let opened = Storage::open(&vfs, Path::new(OUT), 16, Durability::Full).unwrap();
    let view = opened.snapshot().unwrap();
    assert_eq!(graph.get_node(&view, node).unwrap().id, node);
    assert!(matches!(
        other.get_node(&view, NodeId::from_u128(1).unwrap()),
        Err(GraphError::NotFound)
    ));
}

#[test]
fn corrupt_node_shifts_later_ids_and_omits_the_edge() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);
    let mut batch = GraphBatch::begin(&storage).unwrap();
    let a = graph.create_node(&mut batch).unwrap();
    let b = graph.create_node(&mut batch).unwrap();
    let knows: TypeId = graph.intern(&mut batch, "KNOWS").unwrap();
    let name: PropertyKeyId = graph.intern(&mut batch, "name").unwrap();
    graph
        .set_property(&mut batch, b, name, &Value::from("kept"))
        .unwrap();
    graph.create_edge(&mut batch, a, b, knows).unwrap();
    batch.commit().unwrap();
    let mut key = graph.namespace().to_bytes().to_vec();
    key.push(0x02);
    key.extend_from_slice(a.as_bytes());
    put_key(&storage, &key, &[0xff]);
    drop(storage);

    let report = salvage(&vfs, Path::new(PATH), Path::new(OUT), graph.namespace()).unwrap();
    assert_eq!((report.nodes, report.edges), (1, 0));
    assert!(report.omissions.iter().any(|item| {
        item.kind == OmissionKind::Node
            && item.reason == OmissionReason::Corrupt
            && item.source_id == a.to_u128()
    }));
    assert!(
        report.omissions.iter().any(|item| {
            item.kind == OmissionKind::Edge && item.reason == OmissionReason::Missing
        })
    );

    let opened = Storage::open(&vfs, Path::new(OUT), 16, Durability::Full).unwrap();
    let view = opened.snapshot().unwrap();
    let kept = NodeId::from_u128(1).unwrap();
    assert_eq!(
        graph.get_property(&view, kept, name).unwrap(),
        Some(Value::from("kept"))
    );
    assert!(matches!(
        graph.get_edge(&view, pizzlydb_core::EdgeId::from_u128(1).unwrap()),
        Err(GraphError::NotFound)
    ));
}

#[test]
fn discarded_log_keeps_the_checkpoint_only() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    let graph = graph(1);
    let mut batch = GraphBatch::begin(&storage).unwrap();
    let first = graph.create_node(&mut batch).unwrap();
    batch.commit().unwrap();
    storage.checkpoint().unwrap();
    let mut batch = GraphBatch::begin(&storage).unwrap();
    let second = graph.create_node(&mut batch).unwrap();
    batch.commit().unwrap();
    drop(storage);
    vfs.remove(Path::new("graph-wal"), FileRole::Wal).unwrap();

    let report = salvage(&vfs, Path::new(PATH), Path::new(OUT), graph.namespace()).unwrap();
    assert_eq!(report.nodes, 1);
    assert_eq!(
        report.omissions,
        vec![pizzlydb_graph::Omission {
            kind: OmissionKind::Wal,
            source_id: 0,
            reason: OmissionReason::Wal,
        }]
    );
    let opened = Storage::open(&vfs, Path::new(OUT), 16, Durability::Full).unwrap();
    let view = opened.snapshot().unwrap();
    assert!(graph.get_node(&view, first).is_ok());
    assert!(matches!(
        graph.get_node(&view, second),
        Err(GraphError::NotFound)
    ));
}

#[test]
fn damaged_superblocks_publish_nothing() {
    let vfs = MemoryVfs::new();
    let storage = create(&vfs);
    drop(storage);
    let before = file_bytes(&vfs, PATH, FileRole::Main);
    {
        let mut file = vfs
            .open(Path::new(PATH), FileRole::Main, &OpenOptions::new())
            .unwrap();
        file.write_all(0, &vec![0u8; 8192 * 2]).unwrap();
        file.sync().unwrap();
    }
    let damaged = file_bytes(&vfs, PATH, FileRole::Main);
    let seeded =
        Storage::create(&vfs, Path::new(OUT), 16, Durability::Full, |id| id.fill(9)).unwrap();
    let mut batch = seeded.begin_write().unwrap();
    batch.put(b"keep", b"yes").unwrap();
    batch.commit(Durability::Full).unwrap();
    drop(seeded);

    let err = salvage(&vfs, Path::new(PATH), Path::new(OUT), graph(1).namespace()).unwrap_err();
    assert!(matches!(err, GraphError::Storage(_)));
    assert_eq!(file_bytes(&vfs, PATH, FileRole::Main), damaged);
    assert_ne!(damaged, before);
    let opened = Storage::open(&vfs, Path::new(OUT), 16, Durability::Full).unwrap();
    assert_eq!(
        opened.snapshot().unwrap().get(b"keep").unwrap().as_deref(),
        Some(&b"yes"[..])
    );
}

#[test]
fn rename_fault_publishes_neither_destination_nor_source() {
    let vfs = FailRename {
        inner: MemoryVfs::new(),
    };
    let storage =
        Storage::create(&vfs, Path::new(OUT), 16, Durability::Full, |id| id.fill(4)).unwrap();
    let mut batch = storage.begin_write().unwrap();
    batch.put(b"keep", b"yes").unwrap();
    batch.commit(Durability::Full).unwrap();
    drop(storage);

    let storage =
        Storage::create(&vfs, Path::new(PATH), 16, Durability::Full, |id| id.fill(5)).unwrap();
    let graph = graph(1);
    let mut batch = GraphBatch::begin(&storage).unwrap();
    graph.create_node(&mut batch).unwrap();
    batch.commit().unwrap();
    drop(storage);
    let before = file_bytes(&vfs.inner, PATH, FileRole::Main);

    let err = salvage(&vfs, Path::new(PATH), Path::new(OUT), graph.namespace()).unwrap_err();
    assert!(matches!(err, GraphError::Storage(_)));
    assert_eq!(file_bytes(&vfs.inner, PATH, FileRole::Main), before);
    let opened = Storage::open(&vfs, Path::new(OUT), 16, Durability::Full).unwrap();
    assert_eq!(
        opened.snapshot().unwrap().get(b"keep").unwrap().as_deref(),
        Some(&b"yes"[..])
    );
}

const CANONICAL: &str = "\
label Person
label City
type LIVES
key name
key population
node a
node b
node c
labels a Person
labels b City
prop node a name string Ada
prop node b population int 3
prop node c name bytes 6869
edge e1 a b LIVES
prop edge e1 name string home
index label Person name
";

fn replay_canonical(storage: &Storage<MemoryVfs>) {
    let graph = graph(1);
    let mut batch = GraphBatch::begin(storage).unwrap();
    let person = graph.intern(&mut batch, "Person").unwrap();
    let city = graph.intern(&mut batch, "City").unwrap();
    let lives: TypeId = graph.intern(&mut batch, "LIVES").unwrap();
    let name = graph.intern(&mut batch, "name").unwrap();
    let population = graph.intern(&mut batch, "population").unwrap();
    let a = graph.create_node(&mut batch).unwrap();
    let b = graph.create_node(&mut batch).unwrap();
    let c = graph.create_node(&mut batch).unwrap();
    graph.set_labels(&mut batch, a, &[person]).unwrap();
    graph.set_labels(&mut batch, b, &[city]).unwrap();
    graph
        .set_property(&mut batch, a, name, &Value::from("Ada"))
        .unwrap();
    graph
        .set_property(&mut batch, b, population, &Value::Int(3))
        .unwrap();
    graph
        .set_property(&mut batch, c, name, &Value::Bytes(b"hi".to_vec()))
        .unwrap();
    let edge = graph.create_edge(&mut batch, a, b, lives).unwrap();
    graph
        .set_property(&mut batch, edge, name, &Value::from("home"))
        .unwrap();
    graph
        .create_index(&mut batch, Scope::Label(person), name, false)
        .unwrap();
    batch.commit().unwrap();
}

#[test]
fn bulk_load_matches_a_phased_replay_including_a_spill() {
    let vfs = MemoryVfs::new();
    let report = bulk_load(
        &vfs,
        CANONICAL,
        Path::new("loaded"),
        NamespaceId::from_u128(1).unwrap(),
        LoadOptions::new(),
    )
    .unwrap();
    assert_eq!(report.names, 5);
    assert_eq!(report.nodes, 3);
    assert_eq!(report.edges, 1);
    assert_eq!(report.indexes, 1);
    assert_eq!(report.temp_bytes, 0);

    let loaded = Storage::open(&vfs, Path::new("loaded"), 32, Durability::Full).unwrap();
    let graph = graph(1);
    let view = loaded.snapshot().unwrap();
    let node = NodeId::from_u128(1).unwrap();
    let name = PropertyKeyId::from_u128(1).unwrap();
    let population = PropertyKeyId::from_u128(2).unwrap();
    assert_eq!(
        graph.get_property(&view, node, name).unwrap(),
        Some(Value::from("Ada"))
    );
    assert_eq!(
        graph
            .get_property(&view, NodeId::from_u128(2).unwrap(), population)
            .unwrap(),
        Some(Value::Int(3))
    );
    assert_eq!(
        graph
            .get_property(&view, NodeId::from_u128(3).unwrap(), name)
            .unwrap(),
        Some(Value::Bytes(b"hi".to_vec()))
    );
    let edge = graph
        .get_edge(&view, pizzlydb_core::EdgeId::from_u128(1).unwrap())
        .unwrap();
    assert_eq!(
        (edge.source, edge.target),
        (NodeId::from_u128(1).unwrap(), NodeId::from_u128(2).unwrap())
    );
    assert_eq!(
        graph.get_property(&view, edge.id, name).unwrap(),
        Some(Value::from("home"))
    );
    let index = graph
        .find_index(
            &view,
            Scope::Label(pizzlydb_core::LabelId::from_u128(1).unwrap()),
            name,
            false,
        )
        .unwrap()
        .unwrap();
    let hits = graph
        .index_lookup(&view, index, &Value::from("Ada"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(hits.len(), 1);

    let replay_vfs = MemoryVfs::new();
    let replay = Storage::create(
        &replay_vfs,
        Path::new("replay"),
        32,
        Durability::Full,
        |id| {
            id.fill(1);
        },
    )
    .unwrap();
    replay_canonical(&replay);
    let replay_view = replay.snapshot().unwrap();
    assert_eq!(dump(&view), dump(&replay_view));

    let spilled = "type KNOWS\nnode b\nedge e a b KNOWS\nnode a\n";
    let vfs = MemoryVfs::new();
    let report = bulk_load(
        &vfs,
        spilled,
        Path::new("spill"),
        NamespaceId::from_u128(1).unwrap(),
        LoadOptions { temp_budget: 24 },
    )
    .unwrap();
    assert!(report.temp_bytes > 0);
    let opened = Storage::open(&vfs, Path::new("spill"), 32, Durability::Full).unwrap();
    let view = opened.snapshot().unwrap();
    let edge = graph
        .get_edge(&view, pizzlydb_core::EdgeId::from_u128(1).unwrap())
        .unwrap();
    assert_eq!(
        (edge.source, edge.target),
        (NodeId::from_u128(2).unwrap(), NodeId::from_u128(1).unwrap())
    );
    assert_eq!(
        graph
            .neighbors(&view, edge.source, Direction::Out)
            .unwrap()
            .count(),
        1
    );
}

struct RejectedScript {
    script: &'static str,
    budget: u64,
    check: fn(&GraphError) -> bool,
}

#[test]
fn rejected_scripts_leave_the_seeded_destination() {
    let cases = [
        RejectedScript {
            script: "node a\nnode a\n",
            budget: u64::MAX,
            check: |err| matches!(err, GraphError::NameConflict),
        },
        RejectedScript {
            script: "node a\ntype KNOWS\nedge e z a KNOWS\n",
            budget: u64::MAX,
            check: |err| matches!(err, GraphError::EndpointNotLive),
        },
        RejectedScript {
            script: "node a\nkey name\nprop node a name float nan\n",
            budget: u64::MAX,
            check: |err| matches!(err, GraphError::InvalidValue),
        },
        RejectedScript {
            script: "node a\nkey name\nprop node a name nope\n",
            budget: u64::MAX,
            check: |err| matches!(err, GraphError::InvalidValue),
        },
        RejectedScript {
            script: "node a\n",
            budget: 0,
            check: |err| matches!(err, GraphError::Storage(StorageError::Full)),
        },
    ];

    for case in cases {
        let vfs = MemoryVfs::new();
        let storage =
            Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1)).unwrap();
        let mut batch = storage.begin_write().unwrap();
        batch.put(b"marker", b"keep").unwrap();
        batch.commit(Durability::Full).unwrap();
        drop(storage);
        let err = bulk_load(
            &vfs,
            case.script,
            Path::new("db"),
            NamespaceId::from_u128(1).unwrap(),
            LoadOptions {
                temp_budget: case.budget,
            },
        )
        .unwrap_err();
        assert!((case.check)(&err), "{err:?}");
        let opened = Storage::open(&vfs, Path::new("db"), 8, Durability::Full).unwrap();
        assert_eq!(
            opened
                .snapshot()
                .unwrap()
                .get(b"marker")
                .unwrap()
                .as_deref(),
            Some(&b"keep"[..])
        );
        let missing = vfs
            .open(Path::new("db.partial"), FileRole::Main, &OpenOptions::new())
            .unwrap_err();
        assert!(matches!(missing, VfsError::NotFound));
    }
}

#[test]
fn bulk_rename_fault_does_not_publish() {
    let vfs = FailRename {
        inner: MemoryVfs::new(),
    };
    let err = bulk_load(
        &vfs,
        "node a\n",
        Path::new("fresh"),
        NamespaceId::from_u128(1).unwrap(),
        LoadOptions::new(),
    )
    .unwrap_err();
    assert!(matches!(err, GraphError::Storage(_)));
    let missing = vfs
        .open(Path::new("fresh"), FileRole::Main, &OpenOptions::new())
        .unwrap_err();
    assert!(matches!(missing, VfsError::NotFound));
}

#[test]
fn bulk_loads_get_distinct_file_identities() {
    let vfs = MemoryVfs::new();
    let script = "node a\n";
    let namespace = NamespaceId::from_u128(1).unwrap();
    bulk_load(
        &vfs,
        script,
        Path::new("one"),
        namespace,
        LoadOptions::new(),
    )
    .unwrap();
    bulk_load(
        &vfs,
        script,
        Path::new("two"),
        namespace,
        LoadOptions::new(),
    )
    .unwrap();

    let one = Storage::open(&vfs, Path::new("one"), 8, Durability::Full).unwrap();
    let two = Storage::open(&vfs, Path::new("two"), 8, Durability::Full).unwrap();
    assert_ne!(one.file_identity(), two.file_identity());
    assert_ne!(one.file_identity(), [1; 16]);
}

#[test]
fn salvage_reports_an_omitted_namespace_and_a_fresh_identity() {
    let vfs = MemoryVfs::new();
    let storage = Storage::create(&vfs, Path::new("src"), 8, Durability::Full, |id| {
        id.fill(3);
    })
    .unwrap();
    let source_id = storage.file_identity();
    let kept = GraphStore::new(NamespaceId::from_u128(1).unwrap());
    let other = GraphStore::new(NamespaceId::from_u128(2).unwrap());
    let mut batch = GraphBatch::begin(&storage).unwrap();
    kept.create_node(&mut batch).unwrap();
    other.create_node(&mut batch).unwrap();
    batch.commit().unwrap();
    drop(storage);

    let report = salvage(&vfs, Path::new("src"), Path::new("dst"), kept.namespace()).unwrap();
    assert_eq!(report.omitted_namespaces, vec![other.namespace()]);
    let destination = Storage::open(&vfs, Path::new("dst"), 8, Durability::Full).unwrap();
    assert_ne!(destination.file_identity(), source_id);
    let missing = other.get_node(
        &destination.snapshot().unwrap(),
        NodeId::from_u128(1).unwrap(),
    );
    assert!(matches!(missing, Err(GraphError::NotFound)));
}

#[cfg(unix)]
#[test]
fn salvage_reads_a_mode_0444_database() {
    use std::os::unix::fs::PermissionsExt;

    use pizzlydb_storage::{PosixVfs, wal_path};

    let dir = std::env::temp_dir().join(format!("pizzly-salvage-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let vfs = PosixVfs::new();
    let source = dir.join("src");
    let destination = dir.join("dst");
    let db = Storage::create(&vfs, &source, 8, Durability::Full, |id| id.fill(3)).unwrap();
    drop(db);
    for file in [&source, &wal_path(&source)] {
        let mut permissions = std::fs::metadata(file).unwrap().permissions();
        permissions.set_mode(0o444);
        std::fs::set_permissions(file, permissions).unwrap();
    }
    let report = salvage(
        &vfs,
        &source,
        &destination,
        NamespaceId::from_u128(1).unwrap(),
    )
    .unwrap();
    assert_eq!(report.nodes, 0);
}
