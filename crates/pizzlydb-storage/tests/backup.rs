//! Backup, restore, publish, and inspect.

use std::cell::Cell;
use std::path::Path;

use pizzlydb_storage::{
    Durability, Fault, FaultInjecting, FaultKind, FileRole, MemoryVfs, OpenOptions, Storage,
    StorageError, Vfs, VfsError, VfsFile, backup, inspect, publish, restore,
};

#[cfg(unix)]
use pizzlydb_storage::PosixVfs;

fn fill(byte: u8) -> impl FnOnce(&mut [u8; 16]) {
    move |id| id.fill(byte)
}

fn bytes<V: Vfs>(vfs: &V, path: &Path, role: FileRole) -> Vec<u8> {
    let mut file = vfs.open(path, role, &OpenOptions::new()).unwrap();
    let len = usize::try_from(file.len().unwrap()).unwrap();
    let mut buf = vec![0u8; len];
    if len > 0 {
        file.read_exact(0, &mut buf).unwrap();
    }
    buf
}

#[test]
fn backup_matches_one_snapshot_and_excludes_a_later_commit() {
    let vfs = MemoryVfs::new();
    let db = Storage::create(&vfs, Path::new("src"), 8, Durability::Full, fill(1)).unwrap();
    let mut batch = db.begin_write().unwrap();
    batch.put(b"a", b"1").unwrap();
    batch.commit(Durability::Full).unwrap();
    drop(db);

    let report = backup(&vfs, Path::new("src"), Path::new("bak")).unwrap();
    assert_eq!(report.bytes_written, report.pages * 8192);
    assert_eq!(report.bytes_read, report.bytes_written);

    let db = Storage::open(&vfs, Path::new("src"), 8, Durability::Full).unwrap();
    let mut batch = db.begin_write().unwrap();
    batch.put(b"b", b"2").unwrap();
    batch.commit(Durability::Full).unwrap();
    drop(db);

    let bak = Storage::open(&vfs, Path::new("bak"), 8, Durability::Full).unwrap();
    let view = bak.snapshot().unwrap();
    assert_eq!(view.get(b"a").unwrap().as_deref(), Some(&b"1"[..]));
    assert_eq!(view.get(b"b").unwrap(), None);
    assert_eq!(view.sequence(), report.snapshot);
}

#[test]
fn restore_leaves_the_backup_unchanged_and_rejects_the_same_path() {
    let vfs = MemoryVfs::new();
    let db = Storage::create(&vfs, Path::new("src"), 8, Durability::Full, fill(1)).unwrap();
    let mut batch = db.begin_write().unwrap();
    batch.put(b"a", b"1").unwrap();
    batch.commit(Durability::Full).unwrap();
    drop(db);
    backup(&vfs, Path::new("src"), Path::new("bak")).unwrap();
    let before = bytes(&vfs, Path::new("bak"), FileRole::Main);

    restore(&vfs, Path::new("bak"), Path::new("live")).unwrap();
    assert_eq!(bytes(&vfs, Path::new("bak"), FileRole::Main), before);
    let live = Storage::open(&vfs, Path::new("live"), 8, Durability::Full).unwrap();
    assert_eq!(
        live.snapshot().unwrap().get(b"a").unwrap().as_deref(),
        Some(&b"1"[..])
    );

    let err = restore(&vfs, Path::new("bak"), Path::new("bak")).unwrap_err();
    assert!(matches!(err, StorageError::Io(VfsError::InvalidPath)));
}

#[test]
fn inspect_does_not_change_the_file_length() {
    let vfs = MemoryVfs::new();
    let db = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, fill(1)).unwrap();
    drop(db);
    let before = bytes(&vfs, Path::new("db"), FileRole::Main).len();
    let report = inspect(&vfs, Path::new("db")).unwrap();
    assert!(report.damage.is_none());
    assert!(report.logical_pages >= 64);
    assert_eq!(bytes(&vfs, Path::new("db"), FileRole::Main).len(), before);
}

struct FailRename {
    inner: MemoryVfs,
    renames: Cell<usize>,
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
        self.renames.set(self.renames.get() + 1);
        Err(VfsError::Io {
            source: std::io::Error::other("rename failed"),
        })
    }

    fn remove(&self, path: &Path, role: FileRole) -> Result<(), VfsError> {
        self.inner.remove(path, role)
    }
}

#[test]
fn publish_fault_on_the_first_rename_leaves_the_destination() {
    let vfs = FailRename {
        inner: MemoryVfs::new(),
        renames: Cell::new(0),
    };
    let db = Storage::create(&vfs, Path::new("live"), 8, Durability::Full, fill(1)).unwrap();
    let mut batch = db.begin_write().unwrap();
    batch.put(b"keep", b"yes").unwrap();
    batch.commit(Durability::Full).unwrap();
    drop(db);
    let partial = Storage::create(&vfs, Path::new("next"), 8, Durability::Full, fill(2)).unwrap();
    drop(partial);
    let before = bytes(&vfs, Path::new("live"), FileRole::Main);

    let err = publish(&vfs, Path::new("next"), Path::new("live")).unwrap_err();
    assert!(matches!(err, StorageError::Io(_)));
    assert_eq!(vfs.renames.get(), 1);
    assert_eq!(bytes(&vfs, Path::new("live"), FileRole::Main), before);
    let live = Storage::open(&vfs, Path::new("live"), 8, Durability::Full).unwrap();
    assert_eq!(
        live.snapshot().unwrap().get(b"keep").unwrap().as_deref(),
        Some(&b"yes"[..])
    );
}

#[test]
fn fault_injecting_counts_rename() {
    let vfs = FaultInjecting::new(
        MemoryVfs::new(),
        [Fault {
            at: 1,
            kind: FaultKind::Error,
        }],
    );
    let err = vfs
        .rename(Path::new("a"), Path::new("b"), FileRole::Main)
        .unwrap_err();
    assert!(matches!(err, VfsError::Io { .. }));
}

#[cfg(unix)]
#[test]
fn posix_backup_and_restore_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let vfs = PosixVfs::new();
    let src = dir.path().join("src");
    let bak = dir.path().join("bak");
    let live = dir.path().join("live");

    let db = Storage::create(&vfs, &src, 8, Durability::Full, fill(3)).unwrap();
    let mut batch = db.begin_write().unwrap();
    batch.put(b"a", b"1").unwrap();
    batch.commit(Durability::Full).unwrap();
    drop(db);

    backup(&vfs, &src, &bak).unwrap();
    let before = std::fs::read(&bak).unwrap();
    restore(&vfs, &bak, &live).unwrap();
    assert_eq!(std::fs::read(&bak).unwrap(), before);
    let opened = Storage::open(&vfs, &live, 8, Durability::Full).unwrap();
    assert_eq!(
        opened.snapshot().unwrap().get(b"a").unwrap().as_deref(),
        Some(&b"1"[..])
    );
}

#[test]
fn backup_gets_a_fresh_identity_and_rejects_the_source_wal() {
    let vfs = MemoryVfs::new();
    let db = Storage::create(&vfs, Path::new("src"), 8, Durability::Full, fill(9)).unwrap();
    let source_id = db.file_identity();
    let mut batch = db.begin_write().unwrap();
    batch.put(b"a", b"1").unwrap();
    batch.commit(Durability::Full).unwrap();
    drop(db);

    backup(&vfs, Path::new("src"), Path::new("bak")).unwrap();
    let copy = Storage::open(&vfs, Path::new("bak"), 8, Durability::Full).unwrap();
    let copy_id = copy.file_identity();
    assert_ne!(copy_id, source_id);
    drop(copy);

    let source_wal = bytes(
        &vfs,
        &pizzlydb_storage::wal_path(Path::new("src")),
        FileRole::Wal,
    );
    vfs.replace(
        &pizzlydb_storage::wal_path(Path::new("bak")),
        FileRole::Wal,
        &source_wal,
    )
    .unwrap();
    match Storage::open(&vfs, Path::new("bak"), 8, Durability::Full) {
        Err(StorageError::Corrupt) => {}
        Err(err) => panic!("expected Corrupt, got {err}"),
        Ok(_) => panic!("expected the source WAL to be rejected"),
    }
}
