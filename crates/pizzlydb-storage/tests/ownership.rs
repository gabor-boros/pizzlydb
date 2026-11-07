//! Exclusive ownership, read-only open, and disk-full degradation.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use pizzlydb_storage::{
    Durability, FileRole, MemoryVfs, OpenOptions, Storage, StorageError, Vfs, VfsError, VfsFile,
};

#[test]
fn a_second_read_write_open_is_locked_until_the_first_drops() {
    let vfs = MemoryVfs::new();
    let path = Path::new("db");
    let db = Storage::create(&vfs, path, 4, Durability::Full, |id| id.fill(1)).unwrap();
    let second = Storage::open(&vfs, path, 4, Durability::Full);
    assert!(matches!(second, Err(StorageError::Locked)));
    drop(db);
    let db = Storage::open(&vfs, path, 4, Durability::Full).unwrap();
    assert!(db.snapshot().unwrap().get(b"k").unwrap().is_none());
}

#[test]
fn read_only_open_rejects_writes_and_shares_until_a_writer_exists() {
    let vfs = MemoryVfs::new();
    let path = Path::new("db");
    let db = Storage::create(&vfs, path, 4, Durability::Full, |id| id.fill(2)).unwrap();
    let mut batch = db.begin_write().unwrap();
    batch.put(b"k", b"v").unwrap();
    batch.commit(Durability::Full).unwrap();
    drop(db);

    let first = Storage::open_read_only(&vfs, path, 4, Durability::Full).unwrap();
    let second = Storage::open_read_only(&vfs, path, 4, Durability::Full).unwrap();
    assert_eq!(
        first.snapshot().unwrap().get(b"k").unwrap().as_deref(),
        Some(&b"v"[..])
    );
    assert!(matches!(second.begin_write(), Err(StorageError::Io(_))));
    drop(first);
    drop(second);

    let writer = Storage::open(&vfs, path, 4, Durability::Full).unwrap();
    let blocked = Storage::open_read_only(&vfs, path, 4, Durability::Full);
    assert!(matches!(blocked, Err(StorageError::Locked)));
    drop(writer);
    let diagnostic = Storage::open_read_only_diagnostic(&vfs, path, 4, Durability::Full).unwrap();
    assert!(diagnostic.snapshot().is_ok());
}

#[test]
fn no_space_degrades_the_handle_and_keeps_snapshots() {
    let inner = MemoryVfs::new();
    let vfs = FullVfs {
        inner,
        writes: Arc::new(AtomicU64::new(0)),
    };
    let path = Path::new("db");
    let db = Storage::create(&vfs, path, 4, Durability::Full, |id| id.fill(3)).unwrap();
    let mut batch = db.begin_write().unwrap();
    batch.put(b"k", b"v").unwrap();
    let err = batch.commit(Durability::Full).unwrap_err();
    assert!(matches!(err, StorageError::NoSpace));
    assert!(db.snapshot().is_ok());
    assert!(matches!(db.begin_write(), Err(StorageError::NoSpace)));
}

struct FullVfs {
    inner: MemoryVfs,
    writes: Arc<AtomicU64>,
}

impl Vfs for FullVfs {
    type File = FullFile;

    fn open(
        &self,
        path: &Path,
        role: FileRole,
        options: &OpenOptions,
    ) -> Result<Self::File, VfsError> {
        Ok(FullFile {
            inner: self.inner.open(path, role, options)?,
            writes: Arc::clone(&self.writes),
            role,
        })
    }

    fn sync_dir(&self, directory: &Path) -> Result<(), VfsError> {
        self.inner.sync_dir(directory)
    }

    fn rename(&self, from: &Path, to: &Path, role: FileRole) -> Result<(), VfsError> {
        self.inner.rename(from, to, role)
    }

    fn remove(&self, path: &Path, role: FileRole) -> Result<(), VfsError> {
        self.inner.remove(path, role)
    }

    fn lock(
        &self,
        path: &Path,
        mode: pizzlydb_storage::LockMode,
    ) -> Result<pizzlydb_storage::vfs::FileLock, VfsError> {
        self.inner.lock(path, mode)
    }
}

struct FullFile {
    inner: pizzlydb_storage::vfs_memory::MemoryFile,
    writes: Arc<AtomicU64>,
    role: FileRole,
}

impl VfsFile for FullFile {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, VfsError> {
        self.inner.read_at(offset, buf)
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<usize, VfsError> {
        if self.role == FileRole::Wal && self.writes.fetch_add(1, Ordering::Relaxed) > 2 {
            return Err(VfsError::NoSpace);
        }
        self.inner.write_at(offset, data)
    }

    fn sync(&mut self) -> Result<(), VfsError> {
        self.inner.sync()
    }

    fn len(&self) -> Result<u64, VfsError> {
        self.inner.len()
    }

    fn close(self) -> Result<(), VfsError> {
        self.inner.close()
    }

    fn role(&self) -> FileRole {
        self.role
    }
}
