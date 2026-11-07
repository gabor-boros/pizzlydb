//! `NoSpace` at one write or sync site must not drop an acknowledged commit.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pizzlydb_storage::vfs::{FileLock, FileRole, OpenOptions, Vfs, VfsError, VfsFile};
use pizzlydb_storage::vfs_memory::{MemoryFile, MemoryVfs};
use pizzlydb_storage::{Durability, LockMode, MAX_VALUE_LEN, Storage, StorageError};

const PAGE: u64 = 8192;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Site {
    WalAppend,
    WalHeader,
    MainExtend,
    MainPage,
    Superblock,
    Sync,
}

struct SiteVfs {
    inner: MemoryVfs,
    site: Site,
    armed: Arc<AtomicBool>,
    hit: Arc<AtomicBool>,
}

impl SiteVfs {
    fn new(site: Site) -> Self {
        Self {
            inner: MemoryVfs::new(),
            site,
            armed: Arc::new(AtomicBool::new(false)),
            hit: Arc::new(AtomicBool::new(false)),
        }
    }

    fn arm(&self) {
        self.armed.store(true, Ordering::Relaxed);
    }

    fn hit(&self) -> bool {
        self.hit.load(Ordering::Relaxed)
    }
}

struct SiteFile {
    inner: MemoryFile,
    site: Site,
    armed: Arc<AtomicBool>,
    hit: Arc<AtomicBool>,
}

impl SiteFile {
    fn armed(&self) -> bool {
        self.armed.load(Ordering::Relaxed)
    }

    fn fail(&self) -> VfsError {
        self.hit.store(true, Ordering::Relaxed);
        VfsError::NoSpace
    }
}

impl Vfs for SiteVfs {
    type File = SiteFile;

    fn open(
        &self,
        path: &Path,
        role: FileRole,
        options: &OpenOptions,
    ) -> Result<Self::File, VfsError> {
        Ok(SiteFile {
            inner: self.inner.open(path, role, options)?,
            site: self.site,
            armed: Arc::clone(&self.armed),
            hit: Arc::clone(&self.hit),
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

    fn lock(&self, path: &Path, mode: LockMode) -> Result<FileLock, VfsError> {
        self.inner.lock(path, mode)
    }
}

impl VfsFile for SiteFile {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, VfsError> {
        self.inner.read_at(offset, buf)
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<usize, VfsError> {
        if self.armed() && self.matches_write(offset)? {
            return Err(self.fail());
        }
        self.inner.write_at(offset, data)
    }

    fn sync(&mut self) -> Result<(), VfsError> {
        if self.armed() && self.site == Site::Sync {
            return Err(self.fail());
        }
        self.inner.sync()
    }

    fn len(&self) -> Result<u64, VfsError> {
        self.inner.len()
    }

    fn set_len(&mut self, len: u64) -> Result<(), VfsError> {
        if self.armed() && self.site == Site::MainExtend && len > self.inner.len()? {
            return Err(self.fail());
        }
        self.inner.set_len(len)
    }

    fn close(self) -> Result<(), VfsError> {
        self.inner.close()
    }

    fn role(&self) -> FileRole {
        self.inner.role()
    }
}

impl SiteFile {
    fn matches_write(&self, offset: u64) -> Result<bool, VfsError> {
        let role = self.role();
        Ok(match self.site {
            Site::WalAppend => role == FileRole::Wal && offset > 0,
            Site::WalHeader => role == FileRole::Wal && offset == 0,
            Site::MainExtend => role == FileRole::Main && offset >= self.len()?,
            Site::MainPage => role == FileRole::Main && offset >= PAGE * 2,
            Site::Superblock => role == FileRole::Main && offset < PAGE * 2,
            Site::Sync => false,
        })
    }
}

fn put(db: &Storage<SiteVfs>, key: &[u8], value: &[u8]) -> Result<(), StorageError> {
    let mut batch = db.begin_write()?;
    batch.put(key, value)?;
    batch.commit(Durability::Full)?;
    Ok(())
}

#[test]
fn nospace_at_each_write_site_keeps_acknowledged_commits() {
    for site in [
        Site::WalAppend,
        Site::WalHeader,
        Site::MainExtend,
        Site::MainPage,
        Site::Superblock,
        Site::Sync,
    ] {
        let vfs = SiteVfs::new(site);
        let path = Path::new("db");
        let db = Storage::create(&vfs, path, 4, Durability::Full, |id| id.fill(3)).unwrap();
        put(&db, b"kept", b"yes").unwrap();

        if site == Site::MainExtend {
            let wide = vec![7_u8; MAX_VALUE_LEN];
            put(&db, b"wide", &wide).unwrap();
        }

        vfs.arm();
        let failed = match site {
            Site::WalAppend | Site::Sync => put(&db, b"new", b"no"),
            Site::WalHeader | Site::MainExtend | Site::MainPage | Site::Superblock => {
                db.checkpoint()
            }
        };
        assert!(
            matches!(failed, Err(StorageError::NoSpace)),
            "site {site:?} returned {failed:?}"
        );
        assert!(vfs.hit(), "site {site:?} did not inject NoSpace");
        drop(db);

        let db = Storage::open(&vfs, path, 4, Durability::Full).unwrap();
        let view = db.snapshot().unwrap();
        assert_eq!(view.get(b"kept").unwrap().as_deref(), Some(&b"yes"[..]));
        if matches!(site, Site::WalAppend | Site::Sync) {
            assert!(view.get(b"new").unwrap().is_none());
        }
        drop(view);
        drop(db);
        if let Err(err) = pizzlydb_storage::verify_quick(&vfs, path) {
            panic!("site {site:?} verify: {err:?}");
        }
    }
}
