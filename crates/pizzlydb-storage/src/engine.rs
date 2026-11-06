//! Ordered map of byte keys.
//!
//! A commit writes one log transaction. The transaction includes every changed
//! index page, the allocator images when those changed, and the state page.
//! The state page is [`STATE_PAGE_ID`]. Its body, after the standard header, is
//! a little-endian `u64` root page id, a little-endian `u64` high-water mark,
//! and zeros. The root is zero when the map is empty. A snapshot reads this
//! page at its own sequence, then reads the map from that root.
//!
//! [`superblock`] records the root only at checkpoint. The log commit record
//! does not carry the root. The state page is how a commit names its root
//! before checkpoint.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use thiserror::Error;

use crate::btree::{BTree, Cursor, MAX_KEY_LEN, MAX_VALUE_LEN, PageRead, PageWrite, TreeError};
use crate::codec::{get_u64, put_u64};
use crate::page::{
    ALLOCATOR_PAGE_ID, BITMAP_PAGE_ID, HEADER_SIZE, LSN_OFFSET, Lsn, PAGE_SIZE, PageHeader, PageId,
    PageKind, seal,
};
use crate::pager::{Pager, PagerError, SnapshotGuard};
use crate::space::{INITIAL_PAGES, Space, SpaceError, SpillImage};
use crate::superblock::{self, Superblock, SuperblockError};
use crate::vfs::{FileRole, OpenOptions, Vfs, VfsError, VfsFile, io_error};
use crate::wal::{Wal, WalError};

pub use crate::wal::Durability;

const STATE_PAGE_ID: PageId = PageId::new(4);
const STATE_ROOT_OFFSET: usize = HEADER_SIZE;
const STATE_HIGH_OFFSET: usize = HEADER_SIZE + 8;
const STATE_BODY_END: usize = STATE_HIGH_OFFSET + 8;
const SPILL_HEAD_OFFSET: usize = PAGE_SIZE - 16;
const SPILL_NEXT_OFFSET: usize = HEADER_SIZE;
const MAX_SPILL_HOPS: usize = 4096;

const _: () = assert!(STATE_PAGE_ID.as_u64() == 4);
const _: () = assert!(STATE_BODY_END < PAGE_SIZE);

/// Failure from [`Storage`].
///
/// # Examples
///
/// ```
/// use pizzlydb_storage::StorageError;
///
/// let busy = StorageError::Busy;
/// assert!(matches!(busy, StorageError::Busy));
/// assert!(!matches!(busy, StorageError::Closed));
/// ```
#[derive(Debug, Error)]
pub enum StorageError {
    /// The file store returned an error.
    ///
    /// A failed sync during commit also closes the handle. The next call
    /// returns [`Self::Closed`]. Reopen to read the last synced prefix.
    #[error("storage I/O failed")]
    Io(#[source] VfsError),
    /// A checksum, version field, or structural check failed.
    #[error("storage is corrupt")]
    Corrupt,
    /// A write batch is already open.
    ///
    /// The caller is not queued. Checkpoint also returns this while a batch
    /// is open.
    #[error("a write batch is already open")]
    Busy,
    /// The batch needs a page that cannot be allocated, or a key or value is
    /// longer than this format stores.
    #[error("the write does not fit")]
    Full,
    /// The main file or the log was written by a different format.
    #[error("unsupported storage version")]
    UnsupportedVersion,
    /// [`Storage::close`] has run, or a commit changed the log and then failed.
    ///
    /// After a failed commit the process must reopen. A successful
    /// [`Durability::Full`] sync is followed only by an in-memory publish. That
    /// publish does not write again.
    #[error("storage is closed")]
    Closed,
}

impl From<VfsError> for StorageError {
    fn from(err: VfsError) -> Self {
        Self::Io(err)
    }
}

/// Read and write batches over one ordered map.
///
/// `snapshot` and `begin_write` share the handle. A snapshot taken before
/// `commit` stays on that sequence after `commit` returns. [`StorageError::Busy`]
/// is the runtime writer lock.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use pizzlydb_storage::{Durability, MemoryVfs, Storage, StorageEngine};
///
/// let vfs = MemoryVfs::new();
/// let db = Storage::create(&vfs, Path::new("db"), 4, Durability::Full, |id| {
///     id.fill(7);
/// }).unwrap();
///
/// let mut batch = db.begin_write().unwrap();
/// batch.put(b"a", b"1").unwrap();
/// db.atomic_write_sync(batch).unwrap();
///
/// let view = db.snapshot().unwrap();
/// assert_eq!(view.get(b"a").unwrap().as_deref(), Some(&b"1"[..]));
/// ```
pub trait StorageEngine {
    /// Borrowed view of one committed sequence.
    type ReadView<'a>
    where
        Self: 'a;

    /// Staged puts and deletes. Dropping it publishes nothing.
    type WriteBatch;

    /// Register the current committed sequence.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Closed`] when the handle is shut.
    fn snapshot(&self) -> Result<Self::ReadView<'_>, StorageError>;

    /// Start the only open write batch.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Busy`] when a batch is already open, and
    /// [`StorageError::Closed`] when the handle is shut.
    fn begin_write(&self) -> Result<Self::WriteBatch, StorageError>;

    /// Commit `batch` with the durability stored at create or open.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`WriteBatch::commit`]. A batch that belongs to
    /// another handle returns [`StorageError::Corrupt`].
    fn atomic_write_sync(&self, batch: Self::WriteBatch) -> Result<Lsn, StorageError>;

    /// Copy committed images into the main file and advance the superblock.
    ///
    /// Keys visible to an open snapshot do not change.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Busy`] when a write batch is open,
    /// [`StorageError::Closed`] when the handle is shut, and
    /// [`StorageError::Io`] or [`StorageError::Corrupt`] when the copy fails.
    fn checkpoint(&self) -> Result<(), StorageError>;
}

/// Open database.
///
/// The log path is the database path with `-wal` appended. `frame_count` is
/// the number of cached images and must be at least 1. `random` fills the file
/// id used by [`Self::create`]. [`Self::open`] reads that id from the main file.
///
/// Dropping the value closes the files. It does not checkpoint and it does not
/// sync a [`Durability::None`] commit.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
///
/// let vfs = MemoryVfs::new();
/// let db = Storage::create(&vfs, Path::new("db"), 4, Durability::Full, |id| {
///     id.fill(4);
/// }).unwrap();
///
/// db.close().unwrap();
/// drop(db);
/// let db = Storage::open(&vfs, Path::new("db"), 4, Durability::None).unwrap();
/// assert!(matches!(db.begin_write(), Ok(_)));
/// ```
#[must_use = "dropping storage closes its files without checkpoint"]
pub struct Storage<V: Vfs> {
    inner: Rc<Inner<V>>,
}

struct Inner<V: Vfs> {
    pager: Pager<V::File, V::File>,
    db: RefCell<DbState>,
    durability: Durability,
}

struct DbState {
    superblock: Superblock,
    current_copy: PageId,
    root: PageId,
    space: Space,
    writer: bool,
    closed: bool,
}

impl<V: Vfs> Storage<V> {
    /// Create a database at `path` and a sibling log.
    ///
    /// `random` is called once and may leave the id as zeros. The call fails
    /// when `path` or the log path already exists.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Io`] when `frame_count` is 0 or a file operation
    /// fails, [`StorageError::Full`] when the first page cannot be reserved,
    /// and [`StorageError::Corrupt`] when a fresh image fails its own checks.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let db = Storage::create(&vfs, Path::new("created"), 2, Durability::Full, |id| {
    ///     id.fill(1);
    /// })
    /// .unwrap();
    ///
    /// assert!(db.snapshot().unwrap().get(b"a").unwrap().is_none());
    /// ```
    pub fn create(
        vfs: &V,
        path: &Path,
        frame_count: usize,
        durability: Durability,
        random: impl FnOnce(&mut [u8; 16]),
    ) -> Result<Self, StorageError> {
        if frame_count == 0 {
            return Err(StorageError::Io(io_error("frame count is zero")));
        }

        let mut file_id = [0u8; 16];
        random(&mut file_id);

        let mut space = Space::create();
        let state_id = space.allocate(PageKind::State).map_err(map_space)?;
        if state_id != STATE_PAGE_ID {
            return Err(StorageError::Corrupt);
        }

        let exclusive = OpenOptions::new().create(true).exclusive(true);
        let mut main = vfs.open(path, FileRole::Main, &exclusive)?;
        let (superblock, current_copy) = write_new_main(&mut main, file_id, &space)?;
        let wal_file = vfs.open(&wal_path(path), FileRole::Wal, &exclusive)?;
        let wal = Wal::create(wal_file, file_id).map_err(map_wal)?;
        let pager = Pager::open(main, wal, frame_count).map_err(map_pager)?;
        vfs.sync_dir(parent_dir(path))?;

        Ok(Self {
            inner: Rc::new(Inner {
                pager,
                db: RefCell::new(DbState {
                    superblock,
                    current_copy,
                    root: PageId::new(0),
                    space,
                    writer: false,
                    closed: false,
                }),
                durability,
            }),
        })
    }

    /// Open a database created by [`Self::create`].
    ///
    /// The sibling log must exist. An empty log adopts the superblock
    /// checkpoint sequence. A log whose recorded sequence is behind that
    /// checkpoint is corrupt.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Io`] when `frame_count` is 0 or a file is
    /// missing, [`StorageError::UnsupportedVersion`] for a different format,
    /// and [`StorageError::Corrupt`] for a damaged file or a log that is
    /// behind the checkpoint.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage, StorageError};
    ///
    /// let vfs = MemoryVfs::new();
    /// let missing = Storage::open(&vfs, Path::new("missing"), 2, Durability::Full);
    /// assert!(matches!(missing, Err(StorageError::Io(_))));
    /// ```
    pub fn open(
        vfs: &V,
        path: &Path,
        frame_count: usize,
        durability: Durability,
    ) -> Result<Self, StorageError> {
        if frame_count == 0 {
            return Err(StorageError::Io(io_error("frame count is zero")));
        }

        let mut main = vfs.open(path, FileRole::Main, &OpenOptions::new())?;
        let (superblock, current_copy) = Superblock::open(&mut main).map_err(map_super)?;
        let wal_file = vfs.open(&wal_path(path), FileRole::Wal, &OpenOptions::new())?;
        let wal = Wal::open(wal_file, superblock.file_id()).map_err(map_wal)?;
        let checkpoint = superblock.checkpoint_lsn();
        let pager = Pager::open(main, wal, frame_count).map_err(map_pager)?;
        pager.recover_floor(checkpoint).map_err(map_pager)?;
        let (root, space) = load_catalog(&pager)?;

        Ok(Self {
            inner: Rc::new(Inner {
                pager,
                db: RefCell::new(DbState {
                    superblock,
                    current_copy,
                    root,
                    space,
                    writer: false,
                    closed: false,
                }),
                durability,
            }),
        })
    }

    /// Register the current committed sequence.
    ///
    /// Drop the view to release the registration. Checkpoint will not move
    /// the superblock past this sequence while the view is alive.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Closed`] when the handle is shut.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::{Durability, Lsn, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let db = Storage::create(&vfs, Path::new("snap"), 2, Durability::Full, |id| {
    ///     id.fill(2);
    /// })
    /// .unwrap();
    ///
    /// assert_eq!(db.snapshot().unwrap().sequence(), Lsn::new(0));
    /// ```
    pub fn snapshot(&self) -> Result<ReadView<'_, V>, StorageError> {
        self.ensure_open()?;
        let snapshot = self.inner.pager.snapshot();
        Ok(ReadView {
            storage: self,
            snapshot,
        })
    }

    /// Start the only open write batch.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Busy`] when a batch is already open, and
    /// [`StorageError::Closed`] when the handle is shut.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage, StorageError};
    ///
    /// let vfs = MemoryVfs::new();
    /// let db = Storage::create(&vfs, Path::new("busy"), 2, Durability::Full, |id| {
    ///     id.fill(3);
    /// })
    /// .unwrap();
    ///
    /// let batch = db.begin_write().unwrap();
    /// assert!(matches!(db.begin_write(), Err(StorageError::Busy)));
    /// drop(batch);
    /// assert!(db.begin_write().is_ok());
    /// ```
    pub fn begin_write(&self) -> Result<WriteBatch<V>, StorageError> {
        let mut db = self.inner.db.borrow_mut();
        if db.closed {
            return Err(StorageError::Closed);
        }
        if db.writer {
            return Err(StorageError::Busy);
        }
        db.writer = true;
        drop(db);

        Ok(WriteBatch {
            inner: Rc::clone(&self.inner),
            stage: BTreeMap::new(),
            committed: false,
        })
    }

    /// Commit `batch` with the durability passed to create or open.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Corrupt`] when `batch` was started on another
    /// handle. See [`WriteBatch::commit`] for the other errors.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let db = Storage::create(&vfs, Path::new("atomic"), 2, Durability::Full, |id| {
    ///     id.fill(5);
    /// })
    /// .unwrap();
    ///
    /// let mut batch = db.begin_write().unwrap();
    /// batch.put(b"k", b"v").unwrap();
    /// let lsn = db.atomic_write_sync(batch).unwrap();
    /// assert!(lsn.as_u64() > 0);
    /// ```
    pub fn atomic_write_sync(&self, batch: WriteBatch<V>) -> Result<Lsn, StorageError> {
        if !Rc::ptr_eq(&self.inner, &batch.inner) {
            return Err(StorageError::Corrupt);
        }
        let durability = self.inner.durability;
        batch.commit(durability)
    }

    /// Copy the oldest visible sequence into the main file.
    ///
    /// The sequence is the oldest open snapshot, or the committed sequence
    /// when no snapshot is open. The log prefix covered by that sequence is
    /// removed. An open snapshot still reads the same keys.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Busy`] when a write batch is open,
    /// [`StorageError::Closed`] when the handle is shut, and
    /// [`StorageError::Io`] or [`StorageError::Corrupt`] when a copy fails.
    /// A failed log rewrite after the superblock is published closes the handle.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let db = Storage::create(&vfs, Path::new("check"), 2, Durability::Full, |id| {
    ///     id.fill(6);
    /// })
    /// .unwrap();
    ///
    /// db.checkpoint().unwrap();
    /// ```
    pub fn checkpoint(&self) -> Result<(), StorageError> {
        {
            let db = self.inner.db.borrow();
            if db.closed {
                return Err(StorageError::Closed);
            }
            if db.writer {
                return Err(StorageError::Busy);
            }
        }

        self.inner.pager.sync_wal().map_err(map_pager)?;
        let guard = self.inner.pager.snapshot();
        let visible = self
            .inner
            .pager
            .oldest_snapshot()
            .ok_or(StorageError::Corrupt)?;

        let state = read_page(&self.inner.pager, visible, STATE_PAGE_ID)?;
        let (root, high_water) = decode_state(&state)?;

        for id in self.inner.pager.wal_page_ids(visible) {
            if id.as_u64() < 2 {
                continue;
            }
            let pin = self.inner.pager.pin(id, visible).map_err(map_pager)?;
            self.inner.pager.write_main(&pin).map_err(map_pager)?;
        }
        self.inner.pager.sync_main().map_err(map_pager)?;

        let (file_id, generation, current) = {
            let db = self.inner.db.borrow();
            (
                db.superblock.file_id(),
                db.superblock.generation(),
                db.current_copy,
            )
        };

        let next_generation = generation.checked_add(1).ok_or(StorageError::Corrupt)?;
        let next_copy = Superblock::other_copy(current).map_err(map_super)?;
        let record = Superblock::new(
            file_id,
            next_generation,
            visible,
            high_water,
            ALLOCATOR_PAGE_ID,
            PageId::new(0),
            root,
        )
        .map_err(map_super)?;

        let image = record.encode(next_copy).map_err(map_super)?;
        self.inner
            .pager
            .write_page(next_copy, &image)
            .map_err(map_pager)?;

        self.inner.pager.sync_main().map_err(map_pager)?;
        {
            let mut db = self.inner.db.borrow_mut();
            db.superblock = record;
            db.current_copy = next_copy;
        }

        if let Err(err) = self.inner.pager.prune_wal(visible) {
            self.inner.db.borrow_mut().closed = true;
            return Err(map_pager(err));
        }
        drop(guard);

        Ok(())
    }

    /// Reject later calls.
    ///
    /// This does not checkpoint and does not sync a [`Durability::None`] commit.
    /// An open batch can still be dropped. Its `commit` returns
    /// [`StorageError::Closed`].
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Closed`] when the handle is already shut.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage, StorageError};
    ///
    /// let vfs = MemoryVfs::new();
    /// let db = Storage::create(&vfs, Path::new("shut"), 2, Durability::Full, |id| {
    ///     id.fill(8);
    /// })
    /// .unwrap();
    ///
    /// db.close().unwrap();
    /// assert!(matches!(db.snapshot(), Err(StorageError::Closed)));
    /// ```
    pub fn close(&self) -> Result<(), StorageError> {
        let mut db = self.inner.db.borrow_mut();
        if db.closed {
            return Err(StorageError::Closed);
        }
        db.closed = true;
        Ok(())
    }

    fn ensure_open(&self) -> Result<(), StorageError> {
        if self.inner.db.borrow().closed {
            Err(StorageError::Closed)
        } else {
            Ok(())
        }
    }
}

impl<V: Vfs> StorageEngine for Storage<V> {
    type ReadView<'a>
        = ReadView<'a, V>
    where
        Self: 'a;

    type WriteBatch = WriteBatch<V>;

    fn snapshot(&self) -> Result<Self::ReadView<'_>, StorageError> {
        Storage::snapshot(self)
    }

    fn begin_write(&self) -> Result<Self::WriteBatch, StorageError> {
        Storage::begin_write(self)
    }

    fn atomic_write_sync(&self, batch: Self::WriteBatch) -> Result<Lsn, StorageError> {
        Storage::atomic_write_sync(self, batch)
    }

    fn checkpoint(&self) -> Result<(), StorageError> {
        Storage::checkpoint(self)
    }
}

/// One committed sequence.
///
/// `get` and `scan` do not observe a batch that commits after this view is
/// created. Dropping the view releases its registration.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
///
/// let vfs = MemoryVfs::new();
/// let db = Storage::create(&vfs, Path::new("view"), 4, Durability::Full, |id| {
///     id.fill(9);
/// }).unwrap();
///
/// let before = db.snapshot().unwrap();
/// let mut batch = db.begin_write().unwrap();
/// batch.put(b"a", b"1").unwrap();
/// batch.commit(Durability::Full).unwrap();
///
/// assert!(before.get(b"a").unwrap().is_none());
/// assert_eq!(
///     db.snapshot().unwrap().get(b"a").unwrap().as_deref(),
///     Some(&b"1"[..])
/// );
/// ```
#[must_use = "dropping the view releases its snapshot"]
pub struct ReadView<'a, V: Vfs> {
    storage: &'a Storage<V>,
    snapshot: SnapshotGuard<'a, V::File, V::File>,
}

impl<'a, V: Vfs> ReadView<'a, V> {
    /// Sequence captured when the view was created.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::{Durability, Lsn, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let db = Storage::create(&vfs, Path::new("seq"), 2, Durability::Full, |id| {
    ///     id.fill(1);
    /// })
    /// .unwrap();
    ///
    /// assert_eq!(db.snapshot().unwrap().sequence(), Lsn::new(0));
    /// ```
    #[must_use]
    pub fn sequence(&self) -> Lsn {
        self.snapshot.lsn()
    }

    /// Copy the value stored at `key`, or [`None`] when it is absent.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Full`] when `key` is longer than the format
    /// stores, [`StorageError::Corrupt`] when an image fails a check, and
    /// [`StorageError::Closed`] when the handle is shut.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let db = Storage::create(&vfs, Path::new("get"), 2, Durability::Full, |id| {
    ///     id.fill(1);
    /// })
    /// .unwrap();
    ///
    /// assert!(db.snapshot().unwrap().get(b"missing").unwrap().is_none());
    /// ```
    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        self.ensure_open()?;
        let root = self.lookup_root()?;
        let pages = self.pages();
        BTree::from_root(root).get(&pages, key).map_err(map_tree)
    }

    /// Scan keys in unsigned byte order, starting at `from`.
    ///
    /// [`None`] starts at the first key. The empty key sorts first. The scan
    /// reads one sequence. It does not implement [`Copy`].
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Full`] when `from` is longer than the format
    /// stores, [`StorageError::Corrupt`] when an image fails a check, and
    /// [`StorageError::Closed`] when the handle is shut.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let db = Storage::create(&vfs, Path::new("scan"), 4, Durability::Full, |id| {
    ///     id.fill(1);
    /// })
    /// .unwrap();
    ///
    /// let mut batch = db.begin_write().unwrap();
    /// batch.put(b"b", b"2").unwrap();
    /// batch.put(b"a", b"1").unwrap();
    /// batch.commit(Durability::Full).unwrap();
    ///
    /// let view = db.snapshot().unwrap();
    /// let keys: Vec<Vec<u8>> = view
    ///     .scan(None)
    ///     .unwrap()
    ///     .map(|item| item.unwrap().0)
    ///     .collect();
    /// assert_eq!(keys, vec![b"a".to_vec(), b"b".to_vec()]);
    /// ```
    pub fn scan<'b>(&'b self, from: Option<&[u8]>) -> Result<Scan<'b, 'a, V>, StorageError> {
        self.ensure_open()?;
        let root = self.lookup_root()?;
        let pages = self.pages();
        let cursor = BTree::from_root(root)
            .scan_from(&pages, from)
            .map_err(map_tree)?;

        Ok(Scan {
            view: self,
            cursor,
            done: false,
        })
    }

    fn ensure_open(&self) -> Result<(), StorageError> {
        self.storage.ensure_open()
    }

    fn lookup_root(&self) -> Result<PageId, StorageError> {
        let image = read_page(
            &self.storage.inner.pager,
            self.snapshot.lsn(),
            STATE_PAGE_ID,
        )?;

        let (root, _) = decode_state(&image)?;
        Ok(root)
    }

    fn pages(&self) -> SnapshotPages<'_, V::File, V::File> {
        SnapshotPages {
            pager: &self.storage.inner.pager,
            lsn: self.snapshot.lsn(),
        }
    }
}

/// Forward scan of one snapshot.
///
/// Each step returns owned key and value bytes. The scan stops after the
/// first error.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
///
/// let vfs = MemoryVfs::new();
/// let db = Storage::create(&vfs, Path::new("empty-scan"), 2, Durability::Full, |id| {
///     id.fill(1);
/// }).unwrap();
///
/// let view = db.snapshot().unwrap();
/// assert!(view.scan(None).unwrap().next().is_none());
/// ```
#[must_use = "a scan must be driven to read keys"]
pub struct Scan<'a, 'db, V: Vfs> {
    view: &'a ReadView<'db, V>,
    cursor: Cursor,
    done: bool,
}

impl<V: Vfs> Iterator for Scan<'_, '_, V> {
    type Item = Result<(Vec<u8>, Vec<u8>), StorageError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        if self.view.storage.inner.db.borrow().closed {
            self.done = true;
            return Some(Err(StorageError::Closed));
        }

        let pages = self.view.pages();
        match self.cursor.next(&pages) {
            Ok(Some(entry)) => Some(Ok((entry.key.to_vec(), entry.value.to_vec()))),
            Ok(None) => {
                self.done = true;
                None
            }
            Err(err) => {
                self.done = true;
                Some(Err(map_tree(err)))
            }
        }
    }
}

/// Staged puts and deletes.
///
/// The map is unchanged until [`Self::commit`] returns [`Ok`]. Dropping the
/// batch, including when `commit` returns an error before the log accepts it,
/// publishes nothing. The durable flag is set only after the log accepts the
/// batch. [`Durability::Full`] means that acceptance happened after sync.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
///
/// let vfs = MemoryVfs::new();
/// let db = Storage::create(&vfs, Path::new("batch"), 4, Durability::Full, |id| {
///     id.fill(1);
/// }).unwrap();
///
/// let mut batch = db.begin_write().unwrap();
/// batch.put(b"a", b"1").unwrap();
/// batch.delete(b"a").unwrap();
/// batch.commit(Durability::Full).unwrap();
/// assert!(db.snapshot().unwrap().get(b"a").unwrap().is_none());
/// ```
#[must_use = "dropping the batch rolls it back"]
pub struct WriteBatch<V: Vfs> {
    inner: Rc<Inner<V>>,
    stage: BTreeMap<Vec<u8>, Option<Vec<u8>>>,
    committed: bool,
}

impl<V: Vfs> WriteBatch<V> {
    /// Stage `value` at `key`.
    ///
    /// A later `put` or [`Self::delete`] for the same key replaces this one.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Full`] when `key` or `value` exceeds the stored
    /// limit, and [`StorageError::Closed`] when the handle is shut.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let db = Storage::create(&vfs, Path::new("put"), 4, Durability::Full, |id| {
    ///     id.fill(1);
    /// })
    /// .unwrap();
    ///
    /// let mut batch = db.begin_write().unwrap();
    /// batch.put(b"a", b"1").unwrap();
    /// batch.commit(Durability::None).unwrap();
    /// ```
    pub fn put(&mut self, key: &[u8], value: &[u8]) -> Result<(), StorageError> {
        self.ensure_open()?;
        if key.len() > MAX_KEY_LEN || value.len() > MAX_VALUE_LEN {
            return Err(StorageError::Full);
        }
        self.stage.insert(key.to_vec(), Some(value.to_vec()));
        Ok(())
    }

    /// Stage removal of `key`.
    ///
    /// Removing an absent key changes nothing when the batch commits.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Full`] when `key` exceeds the stored limit, and
    /// [`StorageError::Closed`] when the handle is shut.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let db = Storage::create(&vfs, Path::new("delete"), 4, Durability::Full, |id| {
    ///     id.fill(1);
    /// })
    /// .unwrap();
    ///
    /// let mut batch = db.begin_write().unwrap();
    /// batch.put(b"a", b"1").unwrap();
    /// batch.delete(b"a").unwrap();
    /// batch.commit(Durability::Full).unwrap();
    /// ```
    pub fn delete(&mut self, key: &[u8]) -> Result<(), StorageError> {
        self.ensure_open()?;
        if key.len() > MAX_KEY_LEN {
            return Err(StorageError::Full);
        }
        self.stage.insert(key.to_vec(), None);
        Ok(())
    }

    /// Publish the stage, or leave the map unchanged when it is empty.
    ///
    /// [`Durability::Full`] returns [`Ok`] only after the commit record is
    /// synced. [`Durability::None`] records the commit and does not sync. A
    /// crash before a later sync can drop a `None` commit. The in-memory
    /// publish after a successful sync does not write.
    ///
    /// An empty stage, and a stage that does not change any stored byte,
    /// return the current sequence and do not append a commit.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Full`] when a page cannot be allocated,
    /// [`StorageError::Corrupt`] when an existing image is invalid, and
    /// [`StorageError::Io`] when the log write or sync fails. A log failure
    /// closes the handle.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let db = Storage::create(&vfs, Path::new("commit"), 4, Durability::Full, |id| {
    ///     id.fill(1);
    /// })
    /// .unwrap();
    ///
    /// let mut batch = db.begin_write().unwrap();
    /// batch.put(b"a", b"1").unwrap();
    /// let lsn = batch.commit(Durability::Full).unwrap();
    /// assert!(lsn.as_u64() > 0);
    /// ```
    pub fn commit(mut self, durability: Durability) -> Result<Lsn, StorageError> {
        let result = self.publish(durability);
        self.committed = result.is_ok();
        result
    }

    fn ensure_open(&self) -> Result<(), StorageError> {
        if self.inner.db.borrow().closed {
            Err(StorageError::Closed)
        } else {
            Ok(())
        }
    }

    fn publish(&mut self, durability: Durability) -> Result<Lsn, StorageError> {
        self.ensure_open()?;
        if self.stage.is_empty() {
            return Ok(self.inner.pager.committed_lsn());
        }

        let (root, space) = {
            let db = self.inner.db.borrow();
            (db.root, db.space.clone())
        };
        let base = self.inner.pager.committed_lsn();
        let next = Lsn::new(base.as_u64().checked_add(1).ok_or(StorageError::Corrupt)?);

        let mut overlay = Overlay::open(&self.inner.pager, space.clone(), next);
        let mut tree = BTree::from_root(root);
        for (key, value) in &self.stage {
            match value {
                Some(bytes) => tree.insert(&mut overlay, key, bytes).map_err(map_tree)?,
                None => tree.remove(&mut overlay, key).map_err(map_tree)?,
            }
        }

        let changed = tree.root() != root || overlay.space != space || !overlay.dirty.is_empty();
        if !changed {
            return Ok(base);
        }

        overlay.store_space_images();
        overlay.store_state(tree.root())?;
        let images = overlay.stamped(next)?;
        let new_space = overlay.space.clone();
        let new_root = tree.root();
        drop(overlay);

        match self.inner.pager.commit_pages(&images, durability) {
            Ok(lsn) => {
                let mut db = self.inner.db.borrow_mut();
                db.root = new_root;
                db.space = new_space;
                Ok(lsn)
            }
            Err(err) => {
                self.inner.db.borrow_mut().closed = true;
                Err(map_pager(err))
            }
        }
    }
}

impl<V: Vfs> Drop for WriteBatch<V> {
    fn drop(&mut self) {
        // `committed` is true only after the log has accepted the batch.
        // Either way the stage is discarded with this value, and the writer
        // lock is released. The published map is left in place.
        let _published = self.committed;
        self.inner.db.borrow_mut().writer = false;
    }
}

struct SnapshotPages<'a, M: VfsFile, W: VfsFile> {
    pager: &'a Pager<M, W>,
    lsn: Lsn,
}

impl<M: VfsFile, W: VfsFile> PageRead for SnapshotPages<'_, M, W> {
    type Pin<'a>
        = Vec<u8>
    where
        Self: 'a;

    fn pin(&self, id: PageId) -> Result<Self::Pin<'_>, TreeError> {
        let pin = self
            .pager
            .pin(id, self.lsn)
            .map_err(|_| TreeError::Corrupt)?;
        Ok(pin.bytes().to_vec())
    }
}

struct Overlay<'a, M: VfsFile, W: VfsFile> {
    pager: &'a Pager<M, W>,
    snapshot: SnapshotGuard<'a, M, W>,
    images: HashMap<PageId, [u8; PAGE_SIZE]>,
    dirty: HashSet<PageId>,
    space: Space,
    retire_lsn: Lsn,
}

impl<'a, M: VfsFile, W: VfsFile> Overlay<'a, M, W> {
    fn open(pager: &'a Pager<M, W>, space: Space, retire_lsn: Lsn) -> Self {
        let snapshot = pager.snapshot();
        Self {
            pager,
            snapshot,
            images: HashMap::new(),
            dirty: HashSet::new(),
            space,
            retire_lsn,
        }
    }

    fn load(&self, id: PageId) -> Result<[u8; PAGE_SIZE], TreeError> {
        if let Some(image) = self.images.get(&id) {
            return Ok(*image);
        }

        let pin = self
            .pager
            .pin(id, self.snapshot.lsn())
            .map_err(|_| TreeError::Corrupt)?;

        copy_image(&pin.bytes()).map_err(|_| TreeError::Corrupt)
    }

    fn insert(&mut self, id: PageId, image: [u8; PAGE_SIZE]) {
        self.images.insert(id, image);
        self.dirty.insert(id);
    }

    fn store_space_images(&mut self) {
        let encoded = self.space.encode();
        self.insert(ALLOCATOR_PAGE_ID, encoded.header);
        self.insert(BITMAP_PAGE_ID, encoded.bitmap);
        for spill in encoded.spills {
            self.insert(spill.page_id, spill.bytes);
        }
    }

    fn store_state(&mut self, root: PageId) -> Result<(), StorageError> {
        let image = encode_state(root, self.space.high_water(), Lsn::new(0))?;
        self.insert(STATE_PAGE_ID, image);
        Ok(())
    }

    fn stamped(&mut self, lsn: Lsn) -> Result<Vec<(PageId, [u8; PAGE_SIZE])>, StorageError> {
        let ids: Vec<PageId> = self.dirty.iter().copied().collect();
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let image = self.images.get_mut(&id).ok_or(StorageError::Corrupt)?;
            put_u64(image, LSN_OFFSET, lsn.as_u64()).map_err(|_| StorageError::Corrupt)?;
            seal(image).map_err(|_| StorageError::Corrupt)?;
            out.push((id, *image));
        }

        Ok(out)
    }
}

impl<M: VfsFile, W: VfsFile> PageRead for Overlay<'_, M, W> {
    type Pin<'a>
        = Vec<u8>
    where
        Self: 'a;

    fn pin(&self, id: PageId) -> Result<Self::Pin<'_>, TreeError> {
        self.load(id).map(|image| image.to_vec())
    }
}

impl<M: VfsFile, W: VfsFile> PageWrite for Overlay<'_, M, W> {
    type PinMut<'a>
        = &'a mut [u8]
    where
        Self: 'a;

    fn allocate(&mut self, kind: PageKind) -> Result<PageId, TreeError> {
        let id = self.space.allocate(kind).map_err(map_space_tree)?;
        let mut image = [0u8; PAGE_SIZE];

        PageHeader::new(kind, id, Lsn::new(0))
            .encode(&mut image)
            .map_err(|_| TreeError::Corrupt)?;

        self.insert(id, image);
        Ok(id)
    }

    fn retire(&mut self, id: PageId) -> Result<(), TreeError> {
        self.space
            .retire(id, self.retire_lsn)
            .map_err(map_space_tree)
    }

    fn pin_mut(&mut self, id: PageId) -> Result<Self::PinMut<'_>, TreeError> {
        if !self.images.contains_key(&id) {
            let image = self.load(id)?;
            self.images.insert(id, image);
        }

        self.dirty.insert(id);
        self.images
            .get_mut(&id)
            .map(AsMut::as_mut)
            .ok_or(TreeError::Corrupt)
    }
}

fn write_new_main(
    file: &mut impl VfsFile,
    file_id: [u8; 16],
    space: &Space,
) -> Result<(Superblock, PageId), StorageError> {
    let page_size = u64::try_from(PAGE_SIZE).map_err(|_| StorageError::Corrupt)?;
    let last = (INITIAL_PAGES - 1)
        .checked_mul(page_size)
        .ok_or(StorageError::Corrupt)?;

    file.write_all(last, &[0u8; PAGE_SIZE])?;

    let record = fresh_superblock(file_id, 1, space.high_water())?;
    let older = fresh_superblock(file_id, 0, space.high_water())?;
    let page_a = record.encode(superblock::COPY_A).map_err(map_super)?;
    let page_b = older.encode(superblock::COPY_B).map_err(map_super)?;
    file.write_all(0, &page_a)?;
    file.write_all(page_size, &page_b)?;

    let encoded = space.encode();
    file.write_all(offset_of(ALLOCATOR_PAGE_ID)?, &encoded.header)?;
    file.write_all(offset_of(BITMAP_PAGE_ID)?, &encoded.bitmap)?;

    let state = encode_state(PageId::new(0), space.high_water(), Lsn::new(0))?;
    file.write_all(offset_of(STATE_PAGE_ID)?, &state)?;
    file.sync()?;

    Ok((record, superblock::COPY_A))
}

fn fresh_superblock(
    file_id: [u8; 16],
    generation: u64,
    high_water: u64,
) -> Result<Superblock, StorageError> {
    Superblock::new(
        file_id,
        generation,
        Lsn::new(0),
        high_water,
        ALLOCATOR_PAGE_ID,
        PageId::new(0),
        PageId::new(0),
    )
    .map_err(map_super)
}

fn load_catalog<M: VfsFile, W: VfsFile>(
    pager: &Pager<M, W>,
) -> Result<(PageId, Space), StorageError> {
    let guard = pager.snapshot();
    let lsn = guard.lsn();
    let state = read_page(pager, lsn, STATE_PAGE_ID)?;
    let (root, high_water) = decode_state(&state)?;
    let header = read_page(pager, lsn, ALLOCATOR_PAGE_ID)?;
    let bitmap = read_page(pager, lsn, BITMAP_PAGE_ID)?;
    let spills = read_spills(pager, lsn, &header)?;

    let space = Space::load(&header, &bitmap, &spills).map_err(|_| StorageError::Corrupt)?;
    if space.high_water() != high_water {
        return Err(StorageError::Corrupt);
    }

    if root.as_u64() != 0 && !space.is_allocated(root) {
        return Err(StorageError::Corrupt);
    }

    if !space.is_allocated(STATE_PAGE_ID) {
        return Err(StorageError::Corrupt);
    }

    drop(guard);
    Ok((root, space))
}

fn read_spills<M: VfsFile, W: VfsFile>(
    pager: &Pager<M, W>,
    lsn: Lsn,
    header: &[u8],
) -> Result<Vec<SpillImage>, StorageError> {
    let mut next = get_u64(header, SPILL_HEAD_OFFSET).map_err(|_| StorageError::Corrupt)?;
    let mut spills = Vec::new();
    let mut seen = Vec::new();

    while next != 0 {
        if spills.len() >= MAX_SPILL_HOPS {
            return Err(StorageError::Corrupt);
        }
        let id = PageId::new(next);
        if seen.contains(&id) {
            return Err(StorageError::Corrupt);
        }
        seen.push(id);
        let bytes = read_page(pager, lsn, id)?;
        next = get_u64(&bytes, SPILL_NEXT_OFFSET).map_err(|_| StorageError::Corrupt)?;
        spills.push(SpillImage { page_id: id, bytes });
    }

    Ok(spills)
}

fn read_page<M: VfsFile, W: VfsFile>(
    pager: &Pager<M, W>,
    lsn: Lsn,
    id: PageId,
) -> Result<[u8; PAGE_SIZE], StorageError> {
    let pin = pager.pin(id, lsn).map_err(map_pager)?;
    copy_image(&pin.bytes())
}

fn copy_image(bytes: &[u8]) -> Result<[u8; PAGE_SIZE], StorageError> {
    let mut image = [0u8; PAGE_SIZE];
    if bytes.len() != PAGE_SIZE {
        return Err(StorageError::Corrupt);
    }

    image.copy_from_slice(bytes);
    Ok(image)
}

fn encode_state(root: PageId, high_water: u64, lsn: Lsn) -> Result<[u8; PAGE_SIZE], StorageError> {
    let mut page = [0u8; PAGE_SIZE];
    put_u64(&mut page, STATE_ROOT_OFFSET, root.as_u64()).map_err(|_| StorageError::Corrupt)?;
    put_u64(&mut page, STATE_HIGH_OFFSET, high_water).map_err(|_| StorageError::Corrupt)?;

    PageHeader::new(PageKind::State, STATE_PAGE_ID, lsn)
        .encode(&mut page)
        .map_err(|_| StorageError::Corrupt)?;

    Ok(page)
}

fn decode_state(page: &[u8]) -> Result<(PageId, u64), StorageError> {
    if page.len() != PAGE_SIZE {
        return Err(StorageError::Corrupt);
    }

    let header = PageHeader::decode(page).map_err(|_| StorageError::Corrupt)?;
    if header.kind() != PageKind::State || header.page_id() != STATE_PAGE_ID {
        return Err(StorageError::Corrupt);
    }

    if page[STATE_BODY_END..].iter().any(|byte| *byte != 0) {
        return Err(StorageError::Corrupt);
    }

    let root = PageId::new(get_u64(page, STATE_ROOT_OFFSET).map_err(|_| StorageError::Corrupt)?);
    let high_water = get_u64(page, STATE_HIGH_OFFSET).map_err(|_| StorageError::Corrupt)?;
    Ok((root, high_water))
}

fn offset_of(id: PageId) -> Result<u64, StorageError> {
    let page_size = u64::try_from(PAGE_SIZE).map_err(|_| StorageError::Corrupt)?;
    id.as_u64()
        .checked_mul(page_size)
        .ok_or(StorageError::Corrupt)
}

fn wal_path(path: &Path) -> PathBuf {
    let Some(name) = path.file_name() else {
        return path.to_path_buf();
    };

    let mut suffixed = name.to_os_string();
    suffixed.push("-wal");

    let mut out = path.to_path_buf();
    out.set_file_name(suffixed);
    out
}

fn parent_dir(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

fn map_space(err: SpaceError) -> StorageError {
    match err {
        SpaceError::Full => StorageError::Full,
        SpaceError::Corrupt | SpaceError::DoubleFree => StorageError::Corrupt,
    }
}

fn map_space_tree(err: SpaceError) -> TreeError {
    match err {
        SpaceError::Full => TreeError::Full,
        SpaceError::DoubleFree => TreeError::DoubleFree,
        SpaceError::Corrupt => TreeError::Corrupt,
    }
}

fn map_tree(err: TreeError) -> StorageError {
    match err {
        TreeError::Full | TreeError::KeyTooLarge | TreeError::ValueTooLarge => StorageError::Full,
        TreeError::Corrupt | TreeError::DoubleFree => StorageError::Corrupt,
    }
}

fn map_pager(err: PagerError) -> StorageError {
    match err {
        PagerError::Io(err) => StorageError::Io(err),
        PagerError::Wal(err) => map_wal(err),
        PagerError::PoolExhausted => StorageError::Io(io_error("buffer pool exhausted")),
        PagerError::ChecksumMismatch
        | PagerError::Corrupt
        | PagerError::SnapshotNotFound
        | PagerError::Shared
        | PagerError::NotDirty => StorageError::Corrupt,
    }
}

fn map_wal(err: WalError) -> StorageError {
    match err {
        WalError::Io(err) => StorageError::Io(err),
        WalError::UnsupportedVersion => StorageError::UnsupportedVersion,
        WalError::Torn
        | WalError::ChecksumMismatch
        | WalError::BadMagic
        | WalError::Corrupt
        | WalError::FileIdMismatch => StorageError::Corrupt,
    }
}

fn map_super(err: SuperblockError) -> StorageError {
    match err {
        SuperblockError::Io(err) => StorageError::Io(err),
        SuperblockError::UnsupportedVersion => StorageError::UnsupportedVersion,
        SuperblockError::Torn
        | SuperblockError::ChecksumMismatch
        | SuperblockError::BadMagic
        | SuperblockError::Corrupt => StorageError::Corrupt,
    }
}

#[cfg(test)]
mod tests {
    use super::{decode_state, encode_state, wal_path};
    use crate::page::{Lsn, PageId};
    use crate::space::INITIAL_PAGES;
    use std::path::Path;

    #[test]
    fn state_page_round_trip() {
        let page = encode_state(PageId::new(5), INITIAL_PAGES, Lsn::new(3)).unwrap();
        let (root, high) = decode_state(&page).unwrap();
        assert_eq!(root, PageId::new(5));
        assert_eq!(high, INITIAL_PAGES);
    }

    #[test]
    fn wal_path_appends_suffix_to_the_file_name() {
        assert_eq!(wal_path(Path::new("db")), Path::new("db-wal"));
        assert_eq!(wal_path(Path::new("dir/db")), Path::new("dir/db-wal"));
    }
}
