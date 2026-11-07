//! Grouped commits share one sync.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use pizzlydb_storage::{
    Durability, FileRole, MemoryVfs, OpenOptions, Storage, Vfs, VfsError, VfsFile,
};

#[test]
fn two_grouped_commits_share_one_sync() {
    let vfs = CountVfs {
        inner: MemoryVfs::new(),
        syncs: Arc::new(AtomicU64::new(0)),
    };
    let delay = Duration::from_millis(150);
    let db = Arc::new(
        Storage::create(
            &vfs,
            Path::new("group"),
            8,
            Durability::Grouped { max_delay: delay },
            |id| id.fill(1),
        )
        .unwrap(),
    );
    let before = vfs.syncs.load(Ordering::Relaxed);
    let first = {
        let db = Arc::clone(&db);
        thread::spawn(move || {
            let mut batch = db.begin_write().unwrap();
            batch.put(b"a", b"1").unwrap();
            batch
                .commit(Durability::Grouped { max_delay: delay })
                .unwrap();
        })
    };
    thread::sleep(Duration::from_millis(40));
    let second = {
        let db = Arc::clone(&db);
        thread::spawn(move || {
            let mut batch = db.begin_write().unwrap();
            batch.put(b"b", b"2").unwrap();
            batch
                .commit(Durability::Grouped {
                    max_delay: Duration::ZERO,
                })
                .unwrap();
        })
    };
    first.join().unwrap();
    second.join().unwrap();
    let extra = vfs.syncs.load(Ordering::Relaxed) - before;
    assert_eq!(extra, 1, "the two commits should share one sync");
    let view = db.snapshot().unwrap();
    assert_eq!(view.get(b"a").unwrap().as_deref(), Some(&b"1"[..]));
    assert_eq!(view.get(b"b").unwrap().as_deref(), Some(&b"2"[..]));
}

struct CountVfs {
    inner: MemoryVfs,
    syncs: Arc<AtomicU64>,
}

impl Vfs for CountVfs {
    type File = CountFile;

    fn open(
        &self,
        path: &Path,
        role: FileRole,
        options: &OpenOptions,
    ) -> Result<Self::File, VfsError> {
        Ok(CountFile {
            inner: self.inner.open(path, role, options)?,
            syncs: Arc::clone(&self.syncs),
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

struct CountFile {
    inner: pizzlydb_storage::vfs_memory::MemoryFile,
    syncs: Arc<AtomicU64>,
}

impl VfsFile for CountFile {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, VfsError> {
        self.inner.read_at(offset, buf)
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<usize, VfsError> {
        self.inner.write_at(offset, data)
    }

    fn sync(&mut self) -> Result<(), VfsError> {
        self.syncs.fetch_add(1, Ordering::Relaxed);
        self.inner.sync()
    }

    fn len(&self) -> Result<u64, VfsError> {
        self.inner.len()
    }

    fn close(self) -> Result<(), VfsError> {
        self.inner.close()
    }

    fn role(&self) -> FileRole {
        FileRole::Main
    }
}
