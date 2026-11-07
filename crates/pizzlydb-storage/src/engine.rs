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

use std::any::Any;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use std::time::Instant;

use thiserror::Error;

use crate::btree::{BTree, Cursor, MAX_KEY_LEN, MAX_VALUE_LEN, PageRead, PageWrite, TreeError};
use crate::codec::{get_u64, put_u64};
use crate::page::{
    ALLOCATOR_PAGE_ID, BITMAP_PAGE_ID, HEADER_SIZE, LSN_OFFSET, Lsn, PAGE_SIZE, PageHeader, PageId,
    PageKind, seal,
};
use crate::pager::{OwnedSnapshot, Pager, PagerError, PagerStats, SnapshotGuard, SnapshotSpec};

pub use crate::pager::{SnapshotInfo, SnapshotKind};
use crate::space::{INITIAL_PAGES, Space, SpaceError, SpillImage};
use crate::superblock::{self, Superblock, SuperblockError};
use crate::vfs::{
    FileLock, FileRole, LockMode, OpenOptions, Vfs, VfsError, VfsFile, io_error, read_only_error,
};
use crate::wal::{Wal, WalError};

use crate::maintain;

pub use crate::wal::Durability;

/// Which [`BatchLimits`] field a batch exceeded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitKind {
    /// Staged keys.
    Entries,
    /// Staged key and value bytes.
    StagedBytes,
    /// Dirty page images produced while publishing.
    DirtyPages,
    /// Page images appended to the log by one commit.
    WalBytes,
}

/// Upper bounds for one [`WriteBatch`].
///
/// Defaults are 100_000 entries, 64 MiB of staged bytes, 4_096 dirty pages,
/// and 64 MiB of log images. A batch that crosses a bound returns
/// [`StorageError::TransactionTooLarge`] and leaves the database unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatchLimits {
    /// Maximum distinct staged keys.
    pub max_entries: usize,
    /// Maximum of staged key lengths plus value lengths.
    pub max_staged_bytes: u64,
    /// Maximum dirty pages the publish step may build.
    pub max_dirty_pages: usize,
    /// Maximum bytes of page images one commit may append.
    pub max_wal_bytes: u64,
}

impl Default for BatchLimits {
    fn default() -> Self {
        Self {
            max_entries: 100_000,
            max_staged_bytes: 64 * 1024 * 1024,
            max_dirty_pages: 4_096,
            max_wal_bytes: 64 * 1024 * 1024,
        }
    }
}

/// Scan direction for [`ReadView::scan_range`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Ascending key order.
    Forward,
    /// Descending key order. The range is collected, then yielded backward.
    Reverse,
}

/// Draw a 16-byte file identity from the operating system's CSPRNG.
///
/// Backup, export, restore, bulk load, and salvage use this for every database
/// they create. Callers of [`Storage::create`] can pass the bytes through the
/// `random` closure, or supply their own.
///
/// # Errors
///
/// Returns [`StorageError::Io`] when the generator cannot fill the buffer.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use pizzlydb_storage::{Durability, MemoryVfs, Storage, random_file_id};
///
/// let id = random_file_id().unwrap();
/// let vfs = MemoryVfs::new();
/// let db = Storage::create(&vfs, Path::new("fresh"), 2, Durability::Full, |slot| {
///     *slot = id;
/// })
/// .unwrap();
///
/// assert_eq!(db.file_identity(), id);
/// ```
pub fn random_file_id() -> Result<[u8; 16], StorageError> {
    let mut id = [0u8; 16];
    getrandom::fill(&mut id).map_err(|_| StorageError::Io(io_error("csprng failed")))?;
    Ok(id)
}

pub(crate) const STATE_PAGE_ID: PageId = PageId::new(4);
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
#[non_exhaustive]
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
    /// A page image failed CRC-32C. This is corruption, with the page id kept.
    #[error("page {page_id} checksum mismatch")]
    ChecksumMismatch { page_id: u64 },
    /// Another opener holds the database lock.
    #[error("database file is locked")]
    Locked,
    /// The filesystem cannot store more bytes.
    #[error("no space left on the device")]
    NoSpace,
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
    /// The in-memory log index has reached its configured version cap.
    ///
    /// A checkpoint that can prune versions frees room. This is not a
    /// poisoning error.
    #[error("write-ahead log index is full")]
    WalFull,
    /// A write batch exceeded a configured [`BatchLimits`] field.
    ///
    /// The batch is not published and the handle stays open.
    #[error("transaction exceeds the {limit:?} limit; observed {observed}")]
    TransactionTooLarge { limit: LimitKind, observed: u64 },
    /// A writer was refused because a configured limit is already reached.
    #[error("storage is past its {resource} limit")]
    ResourceExhausted { resource: &'static str },
    /// [`Storage::close`] has run, or a commit changed the log and then failed.
    ///
    /// After a failed commit the process must reopen. A successful
    /// [`Durability::Full`] sync is followed only by an in-memory publish. That
    /// publish does not write again.
    #[error("storage is closed")]
    Closed,
    /// The snapshot was unregistered because it outlived its age limit.
    ///
    /// The read returns no bytes. Retry against a new snapshot.
    #[error("snapshot is too old")]
    SnapshotTooOld,
}

impl From<VfsError> for StorageError {
    fn from(err: VfsError) -> Self {
        match err {
            VfsError::Locked => Self::Locked,
            VfsError::NoSpace => Self::NoSpace,
            other => Self::Io(other),
        }
    }
}

/// When the background task copies the log into the main file.
///
/// `wal_soft_bytes` starts a checkpoint. `wal_hard_bytes` makes
/// [`Storage::begin_write`] return [`StorageError::ResourceExhausted`] while a
/// snapshot is held and the appended log is at least that long. `max_interval`
/// is how often the background task looks. `page_budget` is stored with the
/// policy; one checkpoint still copies every visible page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckpointPolicy {
    /// Appended log length that starts a checkpoint.
    pub wal_soft_bytes: u64,
    /// Appended log length that refuses a new batch while a snapshot is held.
    pub wal_hard_bytes: u64,
    /// Delay between background checks.
    pub max_interval: Duration,
    /// Recorded page budget for one checkpoint step.
    pub page_budget: u32,
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
    inner: Arc<Inner<V>>,
}

struct Inner<V: Vfs> {
    pager: Arc<Pager<V::File, V::File>>,
    db: Mutex<DbState>,
    durability: Durability,
    limits: Mutex<BatchLimits>,
    /// Replaced with a no-op guard by [`Storage::close`]. Drop releases whatever remains.
    lock: Mutex<FileLock>,
    policy: Mutex<Option<CheckpointPolicy>>,
    /// Page ids already copied by a budgeted checkpoint that has not sealed a superblock.
    checkpoint_copied: Mutex<Vec<PageId>>,
    gate: Mutex<()>,
    sync_group: Mutex<SyncGroup>,
    sync_cv: Condvar,
    checker: Mutex<Option<JoinHandle<()>>>,
    shutdown: AtomicBool,
    /// Optional monotonic clock for tests. `None` uses process time.
    time_override: Mutex<Option<Arc<AtomicU64>>>,
    /// Crate-private slot. The transaction manager stores a weak handle here.
    extensions: Mutex<Option<Box<dyn Any + Send + Sync>>>,
}

struct SyncGroup {
    flushing: bool,
    epoch: u64,
    failed: bool,
}

struct DbState {
    superblock: Superblock,
    current_copy: PageId,
    root: PageId,
    space: Space,
    writer: bool,
    closed: bool,
    degraded: bool,
    read_only: bool,
}

impl<V: Vfs> Clone for Storage<V> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
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

        let lock = vfs.lock(path, LockMode::Exclusive)?;
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
        let pager = Arc::new(Pager::open(main, wal, frame_count).map_err(map_pager)?);
        vfs.sync_dir(parent_dir(path))?;

        let storage = Self {
            inner: Arc::new(Inner {
                pager,
                db: Mutex::new(DbState {
                    superblock,
                    current_copy,
                    root: PageId::new(0),
                    space,
                    writer: false,
                    closed: false,
                    degraded: false,
                    read_only: false,
                }),
                durability,
                limits: Mutex::new(BatchLimits::default()),
                lock: Mutex::new(lock),
                policy: Mutex::new(None),
                checkpoint_copied: Mutex::new(Vec::new()),
                gate: Mutex::new(()),
                sync_group: Mutex::new(SyncGroup {
                    flushing: false,
                    epoch: 0,
                    failed: false,
                }),
                sync_cv: Condvar::new(),
                checker: Mutex::new(None),
                shutdown: AtomicBool::new(false),
                time_override: Mutex::new(None),
                extensions: Mutex::new(None),
            }),
        };
        Ok(storage)
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
        Self::open_with(vfs, path, frame_count, durability, false, false)
    }

    /// Open `path` without writing it.
    ///
    /// The opener takes a shared lock. A read-write opener already holding the
    /// exclusive lock makes this call return [`StorageError::Locked`].
    /// [`Self::begin_write`] and [`Self::checkpoint`] fail. Snapshots succeed.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::open`], plus [`StorageError::Locked`] when
    /// the shared lock conflicts.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage, StorageError};
    ///
    /// let vfs = MemoryVfs::new();
    /// let db = Storage::create(&vfs, Path::new("ro"), 2, Durability::Full, |id| id.fill(1)).unwrap();
    /// drop(db);
    /// let db = Storage::open_read_only(&vfs, Path::new("ro"), 2, Durability::Full).unwrap();
    /// assert!(matches!(db.begin_write(), Err(StorageError::Io(_))));
    /// ```
    pub fn open_read_only(
        vfs: &V,
        path: &Path,
        frame_count: usize,
        durability: Durability,
    ) -> Result<Self, StorageError> {
        Self::open_with(vfs, path, frame_count, durability, true, false)
    }

    /// Read-only open that does not take the advisory lock.
    ///
    /// A concurrent writer can change the file while this handle reads it.
    /// The handle still refuses [`Self::begin_write`].
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::open_read_only`] other than
    /// [`StorageError::Locked`].
    pub fn open_read_only_diagnostic(
        vfs: &V,
        path: &Path,
        frame_count: usize,
        durability: Durability,
    ) -> Result<Self, StorageError> {
        Self::open_with(vfs, path, frame_count, durability, true, true)
    }

    fn open_with(
        vfs: &V,
        path: &Path,
        frame_count: usize,
        durability: Durability,
        read_only: bool,
        allow_concurrent_writer: bool,
    ) -> Result<Self, StorageError> {
        if frame_count == 0 {
            return Err(StorageError::Io(io_error("frame count is zero")));
        }

        let lock = if read_only && allow_concurrent_writer {
            FileLock::noop()
        } else if read_only {
            vfs.lock(path, LockMode::Shared)?
        } else {
            vfs.lock(path, LockMode::Exclusive)?
        };

        let options = OpenOptions::new().read_only(read_only);
        let mut main = vfs.open(path, FileRole::Main, &options)?;
        let (superblock, current_copy) = Superblock::open(&mut main).map_err(map_super)?;
        let wal_file = vfs.open(&wal_path(path), FileRole::Wal, &options)?;
        let wal = Wal::open(wal_file, superblock.file_id()).map_err(map_wal)?;
        let checkpoint = superblock.checkpoint_lsn();
        let pager = Arc::new(Pager::open(main, wal, frame_count).map_err(map_pager)?);
        pager.recover_floor(checkpoint).map_err(map_pager)?;
        let (root, space) = load_catalog(&pager)?;

        let storage = Self {
            inner: Arc::new(Inner {
                pager,
                db: Mutex::new(DbState {
                    superblock,
                    current_copy,
                    root,
                    space,
                    writer: false,
                    closed: false,
                    degraded: false,
                    read_only,
                }),
                durability,
                limits: Mutex::new(BatchLimits::default()),
                lock: Mutex::new(lock),
                policy: Mutex::new(None),
                checkpoint_copied: Mutex::new(Vec::new()),
                gate: Mutex::new(()),
                sync_group: Mutex::new(SyncGroup {
                    flushing: false,
                    epoch: 0,
                    failed: false,
                }),
                sync_cv: Condvar::new(),
                checker: Mutex::new(None),
                shutdown: AtomicBool::new(false),
                time_override: Mutex::new(None),
                extensions: Mutex::new(None),
            }),
        };
        Ok(storage)
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
    /// Replace the bounds checked by the next [`WriteBatch`].
    pub fn set_batch_limits(&self, limits: BatchLimits) {
        *self.inner.limits.lock().expect("storage") = limits;
    }

    /// Sync the log without publishing a new commit.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Io`] when the sync fails. A failure closes the
    /// handle.
    pub fn sync(&self) -> Result<(), StorageError> {
        self.ensure_open()?;
        self.inner.pager.sync_wal().map_err(|err| {
            self.inner.db.lock().expect("storage").closed = true;
            map_pager(err)
        })
    }

    pub fn snapshot(&self) -> Result<ReadView<V>, StorageError> {
        self.snapshot_with(SnapshotKind::Query, String::new(), None)
    }

    /// Register a snapshot with an owner tag and an optional age limit.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Closed`] when the handle is shut.
    pub fn snapshot_with(
        &self,
        kind: SnapshotKind,
        tag: String,
        max_age: Option<Duration>,
    ) -> Result<ReadView<V>, StorageError> {
        self.ensure_open()?;
        let max_age_ms = max_age.map(|age| u64::try_from(age.as_millis()).unwrap_or(u64::MAX));
        let snapshot = self.inner.pager.snapshot_owned(SnapshotSpec {
            kind,
            tag,
            now_ms: self.now_ms(),
            max_age_ms,
        });
        Ok(ReadView {
            storage: self.clone(),
            snapshot: Some(snapshot),
        })
    }

    /// Hold a snapshot that stays registered until `ttl` passes without [`SnapshotLease::renew`].
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Closed`] when the handle is shut.
    pub fn lease(
        &self,
        kind: SnapshotKind,
        tag: impl Into<String>,
        ttl: Duration,
    ) -> Result<SnapshotLease<V>, StorageError> {
        let view = self.snapshot_with(kind, tag.into(), Some(ttl))?;
        Ok(SnapshotLease { view })
    }

    /// Durability stored at create or open.
    #[must_use]
    pub fn durability(&self) -> Durability {
        self.inner.durability
    }

    /// Milliseconds on the storage clock.
    ///
    /// The clock is process time until [`Self::set_time_ms`] replaces it.
    #[must_use]
    pub fn now_ms(&self) -> u64 {
        if let Some(clock) = self.inner.time_override.lock().expect("storage").as_ref() {
            return clock.load(Ordering::Relaxed);
        }
        system_now_ms()
    }

    /// Replace the storage clock. Later calls see `now_ms` until it changes again.
    pub fn set_time_ms(&self, now_ms: u64) {
        let mut slot = self.inner.time_override.lock().expect("storage");
        match slot.as_ref() {
            Some(clock) => clock.store(now_ms, Ordering::Relaxed),
            None => *slot = Some(Arc::new(AtomicU64::new(now_ms))),
        }
    }

    /// Unregister snapshots that are older than their limit.
    ///
    /// A reaped snapshot fails its next read with [`StorageError::SnapshotTooOld`].
    pub fn reap_snapshots(&self) -> usize {
        self.inner.pager.reap_snapshots(self.now_ms())
    }

    /// Registered snapshots and the log bytes the oldest one pins.
    #[must_use]
    pub fn inspect_snapshots(&self) -> Vec<SnapshotInfo> {
        self.inner.pager.inspect_snapshots(self.now_ms())
    }

    /// Appended write-ahead log length.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Io`] when the length cannot be read.
    pub fn appended_wal_bytes(&self) -> Result<u64, StorageError> {
        self.inner.pager.wal_appended_bytes().map_err(map_pager)
    }

    /// Slot used by `pizzlydb-txn` to keep one manager per database handle.
    #[must_use]
    pub fn extensions(&self) -> &Mutex<Option<Box<dyn Any + Send + Sync>>> {
        &self.inner.extensions
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
        self.enforce_wal_hard_limit()?;
        let mut db = self.inner.db.lock().expect("storage");
        if db.closed {
            return Err(StorageError::Closed);
        }
        if db.read_only {
            return Err(StorageError::Io(read_only_error()));
        }

        if db.degraded {
            return Err(StorageError::NoSpace);
        }
        if db.writer {
            return Err(StorageError::Busy);
        }
        db.writer = true;
        drop(db);

        let limits = *self.inner.limits.lock().expect("storage");
        Ok(WriteBatch {
            inner: Arc::clone(&self.inner),
            stage: BTreeMap::new(),
            staged_bytes: 0,
            limits,
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
        if !Arc::ptr_eq(&self.inner, &batch.inner) {
            return Err(StorageError::Corrupt);
        }
        let durability = self.inner.durability;
        batch.commit(durability)
    }

    /// Copy the oldest visible sequence into the main file.
    ///
    /// The sequence is the oldest open snapshot, or the committed sequence
    /// when no snapshot is open. Pages retired before the pager horizon are
    /// reclaimed first. That bitmap is written into the main file only when
    /// this checkpoint covers the committed sequence. The log prefix covered
    /// by the checkpoint sequence is removed. An open snapshot still reads
    /// the same keys.
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
        self.checkpoint_limited(None)
    }

    fn checkpoint_limited(&self, budget: Option<u32>) -> Result<(), StorageError> {
        let _gate = self.inner.gate.lock().expect("storage");
        {
            let db = self.inner.db.lock().expect("storage");
            if db.closed {
                return Err(StorageError::Closed);
            }
            if db.read_only {
                return Err(StorageError::Io(read_only_error()));
            }
            if db.writer {
                return Err(StorageError::Busy);
            }
        }

        self.inner.pager.sync_wal().map_err(map_pager)?;
        let reclaimed = maintain::reclaim_to_horizon(self)?;
        let guard = self.inner.pager.snapshot();
        let visible = self
            .inner
            .pager
            .oldest_snapshot()
            .ok_or(StorageError::Corrupt)?;
        let committed = self.inner.pager.committed_lsn();

        let state = read_page(&self.inner.pager, visible, STATE_PAGE_ID)?;
        let (root, high_water) = decode_state(&state)?;

        let pending: Vec<PageId> = {
            let copied = self.inner.checkpoint_copied.lock().expect("storage");
            self.inner
                .pager
                .wal_page_ids(visible)
                .into_iter()
                .filter(|id| id.as_u64() >= 2 && !copied.contains(id))
                .collect()
        };
        let limit = budget.map_or(pending.len(), |count| count as usize);
        for id in pending.into_iter().take(limit) {
            let pin = self.inner.pager.pin(id, visible).map_err(map_pager)?;
            self.inner.pager.write_main(&pin).map_err(map_pager)?;
            self.inner
                .checkpoint_copied
                .lock()
                .expect("storage")
                .push(id);
        }
        let finished = {
            let copied = self.inner.checkpoint_copied.lock().expect("storage");
            self.inner
                .pager
                .wal_page_ids(visible)
                .into_iter()
                .filter(|id| id.as_u64() >= 2)
                .all(|id| copied.contains(&id))
        };
        if !finished {
            return Ok(());
        }
        self.inner
            .checkpoint_copied
            .lock()
            .expect("storage")
            .clear();

        if reclaimed && visible == committed {
            self.write_reclaimed_space(visible)?;
        }
        self.inner.pager.sync_main().map_err(map_pager)?;

        let wal_generation = self.inner.pager.wal_generation();
        let (file_id, generation, current) = {
            let db = self.inner.db.lock().expect("storage");
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
        .map_err(map_super)?
        .with_wal_generation(wal_generation);

        let image = record.encode(next_copy).map_err(map_super)?;
        self.inner
            .pager
            .write_page(next_copy, &image)
            .map_err(map_pager)?;

        self.inner.pager.sync_main().map_err(map_pager)?;
        {
            let mut db = self.inner.db.lock().expect("storage");
            db.superblock = record;
            db.current_copy = next_copy;
        }

        if let Err(err) = self.inner.pager.prune_wal(visible) {
            self.inner.db.lock().expect("storage").closed = true;
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
        let policy = self.inner.policy.lock().expect("storage").is_some();
        let (closed, writer, read_only) = {
            let db = self.inner.db.lock().expect("storage");
            (db.closed, db.writer, db.read_only)
        };
        if closed {
            return Err(StorageError::Closed);
        }
        if policy && !writer && !read_only {
            self.checkpoint()?;
        }
        self.inner.shutdown.store(true, Ordering::Relaxed);
        let mut db = self.inner.db.lock().expect("storage");
        db.closed = true;
        drop(db);
        *self.inner.lock.lock().expect("storage") = FileLock::noop();
        Ok(())
    }

    /// Install `policy` and start the background checker if it is not running.
    ///
    /// The task calls [`Self::checkpoint`] when the appended log reaches
    /// `wal_soft_bytes`. Dropping the last handle lets the task exit.
    pub fn set_checkpoint_policy(&self, policy: CheckpointPolicy)
    where
        V: 'static,
    {
        *self.inner.policy.lock().expect("storage") = Some(policy);
        self.ensure_checkpointer();
    }

    /// Checkpoint when the installed policy says the log is past its soft limit.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::checkpoint`] when a checkpoint is due.
    pub fn poll_checkpoint(&self) -> Result<(), StorageError> {
        let policy = *self.inner.policy.lock().expect("storage");
        let Some(policy) = policy else {
            return Ok(());
        };

        let appended = self.inner.pager.wal_appended_bytes().map_err(map_pager)?;
        if appended >= policy.wal_soft_bytes {
            let budget = (policy.page_budget > 0).then_some(policy.page_budget);
            self.checkpoint_limited(budget)?;
        }

        Ok(())
    }

    /// Frames applied from the log when this handle was opened.
    #[must_use]
    pub fn recovered_wal_frames(&self) -> u64 {
        self.inner.pager.recovered_frames()
    }

    fn enforce_wal_hard_limit(&self) -> Result<(), StorageError> {
        let policy = *self.inner.policy.lock().expect("storage");
        let Some(policy) = policy else {
            return Ok(());
        };

        let appended = self.inner.pager.wal_appended_bytes().map_err(map_pager)?;
        if appended < policy.wal_hard_bytes {
            return Ok(());
        }

        if self.inner.pager.snapshot_count() > 0 {
            return Err(StorageError::ResourceExhausted { resource: "wal" });
        }

        self.checkpoint()?;
        Ok(())
    }

    fn ensure_checkpointer(&self)
    where
        V: 'static,
    {
        let mut slot = self.inner.checker.lock().expect("storage");
        if slot.is_some() {
            return;
        }
        let weak = Arc::downgrade(&self.inner);
        *slot = Some(thread::spawn(move || checker_loop(weak)));
    }

    fn ensure_open(&self) -> Result<(), StorageError> {
        if self.inner.db.lock().expect("storage").closed {
            Err(StorageError::Closed)
        } else {
            Ok(())
        }
    }

    pub(crate) fn reclaim_horizon(&self) -> Result<bool, StorageError> {
        if self.inner.db.lock().expect("storage").closed {
            return Ok(false);
        }

        let horizon = self.inner.pager.snapshot_lsn();
        let mut db = self.inner.db.lock().expect("storage");
        if db.closed {
            return Ok(false);
        }

        let before = db.space.clone();
        if let Err(err) = db.space.reclaim(horizon) {
            db.closed = true;
            return Err(map_space(err));
        }

        Ok(db.space != before)
    }

    fn write_reclaimed_space(&self, lsn: Lsn) -> Result<(), StorageError> {
        let encoded = self.inner.db.lock().expect("storage").space.encode();
        let mut header = encoded.header;
        let mut bitmap = encoded.bitmap;
        stamp(&mut header, lsn)?;
        stamp(&mut bitmap, lsn)?;
        self.inner
            .pager
            .write_page(ALLOCATOR_PAGE_ID, &header)
            .map_err(map_pager)?;
        self.inner
            .pager
            .write_page(BITMAP_PAGE_ID, &bitmap)
            .map_err(map_pager)?;

        for extra in encoded.extra_bitmaps {
            let mut bytes = extra.bytes;
            stamp(&mut bytes, lsn)?;
            self.inner
                .pager
                .write_page(extra.page_id, &bytes)
                .map_err(map_pager)?;
        }

        for spill in encoded.spills {
            let mut bytes = spill.bytes;
            stamp(&mut bytes, lsn)?;
            self.inner
                .pager
                .write_page(spill.page_id, &bytes)
                .map_err(map_pager)?;
        }

        Ok(())
    }

    /// Page image visible at `lsn`. `lsn` must be a registered snapshot.
    pub(crate) fn page_image(&self, lsn: Lsn, id: PageId) -> Result<[u8; PAGE_SIZE], StorageError> {
        self.inner.pager.read_image(id, lsn).map_err(map_pager)
    }

    pub(crate) fn main_len(&self) -> Result<u64, StorageError> {
        self.inner.pager.main_len().map_err(map_pager)
    }

    pub(crate) fn logical_pages(&self) -> u64 {
        self.logical_page_count()
    }

    /// Pages the superblock says the main file contains.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let db = Storage::create(&vfs, Path::new("pages"), 2, Durability::Full, |id| {
    ///     id.fill(1);
    /// })
    /// .unwrap();
    ///
    /// assert!(db.logical_page_count() >= 2);
    /// ```
    #[must_use]
    pub fn logical_page_count(&self) -> u64 {
        self.inner
            .db
            .lock()
            .expect("storage")
            .superblock
            .logical_page_count()
    }

    /// Buffer-pool counters since this handle was opened.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let db = Storage::create(&vfs, Path::new("stats"), 2, Durability::Full, |id| {
    ///     id.fill(1);
    /// })
    /// .unwrap();
    ///
    /// assert_eq!(db.stats().hits(), 0);
    /// ```
    #[must_use]
    pub fn stats(&self) -> PagerStats {
        self.inner.pager.stats()
    }

    /// File identity stored in the superblock.
    ///
    /// The identity binds the main file to its log. A log written for a
    /// different identity fails [`Storage::open`] with [`StorageError::Corrupt`].
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let db = Storage::create(&vfs, Path::new("id"), 2, Durability::Full, |id| {
    ///     id.fill(4);
    /// })
    /// .unwrap();
    ///
    /// assert_eq!(db.file_identity(), [4; 16]);
    /// ```
    #[must_use]
    pub fn file_identity(&self) -> [u8; 16] {
        self.inner.db.lock().expect("storage").superblock.file_id()
    }

    pub(crate) fn superblock_slots(&self) -> (PageId, PageId) {
        let db = self.inner.db.lock().expect("storage");
        (
            db.superblock.allocator_page_id(),
            db.superblock.free_space_map_page_id(),
        )
    }

    pub(crate) fn committed_space(&self) -> Space {
        self.inner.db.lock().expect("storage").space.clone()
    }

    pub(crate) fn verify_tree(
        &self,
        lsn: Lsn,
        root: PageId,
        space: &Space,
    ) -> Result<(), StorageError> {
        let pages = AllocatedPages {
            inner: SnapshotPages {
                pager: &self.inner.pager,
                lsn,
            },
            space,
        };
        BTree::from_root(root).verify(&pages).map_err(map_tree)
    }

    #[cfg(test)]
    fn is_page_allocated(&self, id: PageId) -> bool {
        self.inner
            .db
            .lock()
            .expect("storage")
            .space
            .is_allocated(id)
    }

    #[cfg(test)]
    fn retired_pages(&self) -> Vec<PageId> {
        let space = &self.inner.db.lock().expect("storage").space;
        (0..space.high_water())
            .map(PageId::new)
            .filter(|id| space.is_retired(*id))
            .collect()
    }

    #[cfg(test)]
    fn reuse_candidate(&self, retired: &[PageId]) -> Option<PageId> {
        let mut space = self.inner.db.lock().expect("storage").space.clone();
        for _ in 0..space.high_water() {
            let id = space.allocate(PageKind::BTreeLeaf).ok()?;
            if retired.contains(&id) {
                return Some(id);
            }
        }
        None
    }
}

impl<V: Vfs> StorageEngine for Storage<V> {
    type ReadView<'a>
        = ReadView<V>
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
pub struct ReadView<V: Vfs> {
    storage: Storage<V>,
    snapshot: Option<OwnedSnapshot<V::File, V::File>>,
}

/// Renewable registration of one snapshot.
///
/// Pagination and bootstrap hold a lease instead of an unbounded read view.
/// [`Self::renew`] moves the age clock forward. A lapsed lease is removed by
/// [`Storage::reap_snapshots`], and the next read returns
/// [`StorageError::SnapshotTooOld`].
#[must_use = "dropping the lease releases its snapshot"]
pub struct SnapshotLease<V: Vfs> {
    view: ReadView<V>,
}

impl<V: Vfs> SnapshotLease<V> {
    /// Borrow the pinned snapshot.
    pub fn view(&self) -> &ReadView<V> {
        &self.view
    }

    /// Keep the snapshot registered for another `ttl`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::SnapshotTooOld`] when the registration is already gone.
    pub fn renew(&self, ttl: Duration) -> Result<(), StorageError> {
        let Some(snapshot) = self.view.snapshot.as_ref() else {
            return Err(StorageError::SnapshotTooOld);
        };
        let max_age_ms = u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX);
        self.view
            .storage
            .inner
            .pager
            .renew_snapshot(snapshot.id(), self.view.storage.now_ms(), Some(max_age_ms))
            .map_err(|err| match err {
                PagerError::SnapshotNotFound => StorageError::SnapshotTooOld,
                other => map_pager(other),
            })
    }
}

impl<V: Vfs> ReadView<V> {
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
        self.registered().lsn()
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
        self.ensure_current()?;
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
    pub fn scan<'b>(&'b self, from: Option<&[u8]>) -> Result<Scan<'b, V>, StorageError> {
        self.ensure_current()?;
        let root = self.lookup_root()?;
        let pages = self.pages();
        let cursor = BTree::from_root(root)
            .scan_from(&pages, from)
            .map_err(map_tree)?;

        Ok(Scan {
            view: self,
            cursor,
            done: false,
            end: None,
            reverse: None,
        })
    }

    /// Scan keys in `[start, end)`.
    ///
    /// `end` of [`None`] means there is no upper bound. [`Direction::Reverse`]
    /// collects the range and then yields it from the last key. The count is
    /// exact, so [`Self::estimate_range`] reports equal lower and upper bounds.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Closed`] when the handle is shut, and
    /// [`StorageError::Corrupt`] when a page on the path is invalid.
    pub fn scan_range<'b>(
        &'b self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        direction: Direction,
    ) -> Result<Scan<'b, V>, StorageError> {
        if direction == Direction::Reverse {
            let mut rows = Vec::new();
            let mut forward = self.scan_range(start, end, Direction::Forward)?;
            for item in forward.by_ref() {
                rows.push(item?);
            }
            return Ok(Scan {
                view: self,
                cursor: Cursor::empty(),
                done: false,
                end: None,
                reverse: Some(rows),
            });
        }

        self.ensure_current()?;
        let root = self.lookup_root()?;
        let pages = self.pages();
        let cursor = BTree::from_root(root)
            .scan_from(&pages, start)
            .map_err(map_tree)?;

        Ok(Scan {
            view: self,
            cursor,
            done: false,
            end: end.map(|bound| bound.to_vec()),
            reverse: None,
        })
    }

    /// Exact key count and byte size of `[start, end)`.
    ///
    /// The estimate is exact: the lower and upper key bounds are equal. Bytes
    /// count stored key and value lengths, not page images. Uniform and skewed
    /// key sets use the same count.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::scan_range`].
    pub fn estimate_range(
        &self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
    ) -> Result<RangeEstimate, StorageError> {
        let mut keys = 0u64;
        let mut bytes = 0u64;
        for item in self.scan_range(start, end, Direction::Forward)? {
            let (key, value) = item?;
            keys = keys.saturating_add(1);
            bytes = bytes
                .saturating_add(key.len() as u64)
                .saturating_add(value.len() as u64);
        }

        Ok(RangeEstimate {
            keys_lower: keys,
            keys_upper: keys,
            bytes,
        })
    }

    /// `true` when the registration was reaped.
    #[must_use]
    pub fn is_expired(&self) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(OwnedSnapshot::is_expired)
    }

    fn ensure_open(&self) -> Result<(), StorageError> {
        self.storage.ensure_open()
    }

    fn ensure_current(&self) -> Result<(), StorageError> {
        if self
            .snapshot
            .as_ref()
            .is_some_and(OwnedSnapshot::is_expired)
        {
            return Err(StorageError::SnapshotTooOld);
        }
        self.ensure_open()
    }

    fn registered(&self) -> &OwnedSnapshot<V::File, V::File> {
        self.snapshot
            .as_ref()
            .expect("snapshot is registered until the view is dropped")
    }

    fn lookup_root(&self) -> Result<PageId, StorageError> {
        let image = read_page(
            &self.storage.inner.pager,
            self.registered().lsn(),
            STATE_PAGE_ID,
        )?;

        let (root, _) = decode_state(&image)?;
        Ok(root)
    }

    fn pages(&self) -> SnapshotPages<'_, V::File, V::File> {
        SnapshotPages {
            pager: &self.storage.inner.pager,
            lsn: self.registered().lsn(),
        }
    }
}

impl<V: Vfs> Drop for ReadView<V> {
    fn drop(&mut self) {
        self.snapshot.take();
        let _ = maintain::reclaim_to_horizon(&self.storage);
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
pub struct Scan<'a, V: Vfs> {
    view: &'a ReadView<V>,
    cursor: Cursor,
    done: bool,
    end: Option<Vec<u8>>,
    reverse: Option<Vec<(Vec<u8>, Vec<u8>)>>,
}

/// Key count and stored byte size of a range.
///
/// `keys_lower` and `keys_upper` are equal: the scan counted every key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RangeEstimate {
    /// Smallest key count consistent with the tree.
    pub keys_lower: u64,
    /// Largest key count consistent with the tree.
    pub keys_upper: u64,
    /// Sum of key and value lengths.
    pub bytes: u64,
}

impl<V: Vfs> Iterator for Scan<'_, V> {
    type Item = Result<(Vec<u8>, Vec<u8>), StorageError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        if self.view.storage.inner.db.lock().expect("storage").closed {
            self.done = true;
            return Some(Err(StorageError::Closed));
        }

        if let Some(rows) = &mut self.reverse {
            return rows.pop().map(Ok);
        }

        let pages = self.view.pages();
        match self.cursor.next(&pages) {
            Ok(Some(entry)) => {
                if self
                    .end
                    .as_ref()
                    .is_some_and(|end| entry.key >= end.as_slice())
                {
                    self.done = true;
                    return None;
                }
                Some(Ok((entry.key.to_vec(), entry.value.to_vec())))
            }
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
    inner: Arc<Inner<V>>,
    stage: BTreeMap<Vec<u8>, Option<Vec<u8>>>,
    staged_bytes: u64,
    limits: BatchLimits,
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
        self.stage_change(key, Some(value))
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
        self.stage_change(key, None)
    }

    /// Stage removal of every committed or staged key in `[start, end)`.
    ///
    /// The deletion is applied on commit as one key at a time. Pages written are
    /// proportional to the number of keys in the range, not to the database size.
    /// A range that covers a whole subtree still rewrites the pages that hold
    /// those keys.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::TransactionTooLarge`] when the staged tombstones
    /// cross [`BatchLimits`], and [`StorageError::Closed`] when the handle is shut.
    pub fn delete_range(&mut self, start: &[u8], end: &[u8]) -> Result<(), StorageError> {
        self.ensure_open()?;
        if start >= end {
            return Ok(());
        }

        let keys: Vec<Vec<u8>> = self
            .stage
            .range(start.to_vec()..end.to_vec())
            .map(|(key, _)| key.clone())
            .collect();

        for key in keys {
            self.stage_change(&key, None)?;
        }

        let storage = Storage {
            inner: Arc::clone(&self.inner),
        };
        let view = storage.snapshot()?;

        let mut found = Vec::new();
        for item in view.scan_range(Some(start), Some(end), Direction::Forward)? {
            found.push(item?.0);
        }
        drop(view);

        for key in found {
            self.stage_change(&key, None)?;
        }

        Ok(())
    }

    fn stage_change(&mut self, key: &[u8], value: Option<&[u8]>) -> Result<(), StorageError> {
        let incoming =
            (key.len() as u64).saturating_add(value.map(|bytes| bytes.len() as u64).unwrap_or(0));

        let previous = self.stage.get(key).map(|stored| {
            (key.len() as u64)
                .saturating_add(stored.as_ref().map(|bytes| bytes.len() as u64).unwrap_or(0))
        });

        let next_entries = if previous.is_some() {
            self.stage.len()
        } else {
            self.stage.len().saturating_add(1)
        };

        if next_entries > self.limits.max_entries {
            return Err(StorageError::TransactionTooLarge {
                limit: LimitKind::Entries,
                observed: u64::try_from(next_entries).unwrap_or(u64::MAX),
            });
        }

        let next_bytes = self
            .staged_bytes
            .saturating_sub(previous.unwrap_or(0))
            .saturating_add(incoming);

        if next_bytes > self.limits.max_staged_bytes {
            return Err(StorageError::TransactionTooLarge {
                limit: LimitKind::StagedBytes,
                observed: next_bytes,
            });
        }

        self.staged_bytes = next_bytes;
        self.stage
            .insert(key.to_vec(), value.map(|bytes| bytes.to_vec()));

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
        let result = self.publish(durability, true, |_| Ok(Vec::new()));
        self.committed = result.is_ok();
        result
    }

    /// Publish the batch. `hook` runs after the commit LSN is chosen and may
    /// stage more keys, including a record that stores that LSN.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::commit`], and the error returned by `hook`.
    pub fn commit_with<F>(mut self, durability: Durability, hook: F) -> Result<Lsn, StorageError>
    where
        F: FnOnce(Lsn) -> Result<Vec<(Vec<u8>, Option<Vec<u8>>)>, StorageError>,
    {
        let result = self.publish(durability, true, hook);
        self.committed = result.is_ok();
        result
    }

    /// Publish the batch and return before the durability wait.
    ///
    /// [`CommitAck::acknowledge`] performs that wait. The caller can release
    /// its own commit lock between the two calls.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::commit_with`] that happen before sync.
    pub fn commit_unacked<F>(
        mut self,
        durability: Durability,
        hook: F,
    ) -> Result<CommitAck<V>, StorageError>
    where
        F: FnOnce(Lsn) -> Result<Vec<(Vec<u8>, Option<Vec<u8>>)>, StorageError>,
    {
        let lsn = self.publish(durability, false, hook)?;
        self.committed = true;
        Ok(CommitAck {
            storage: Storage {
                inner: Arc::clone(&self.inner),
            },
            durability,
            lsn,
        })
    }

    fn ensure_open(&self) -> Result<(), StorageError> {
        if self.inner.db.lock().expect("storage").closed {
            Err(StorageError::Closed)
        } else {
            Ok(())
        }
    }

    fn join_group(&self, max_delay: Duration) -> Result<(), StorageError> {
        join_group(&self.inner, max_delay)
    }

    fn publish<F>(
        &mut self,
        durability: Durability,
        wait: bool,
        hook: F,
    ) -> Result<Lsn, StorageError>
    where
        F: FnOnce(Lsn) -> Result<Vec<(Vec<u8>, Option<Vec<u8>>)>, StorageError>,
    {
        self.ensure_open()?;
        let base = self.inner.pager.committed_lsn();
        let next = Lsn::new(base.as_u64().checked_add(1).ok_or(StorageError::Corrupt)?);
        for (key, value) in hook(next)? {
            self.stage_change(&key, value.as_deref())?;
        }
        let gate = self.inner.gate.lock().expect("storage");
        if self.stage.is_empty() {
            return Ok(base);
        }

        let (root, space) = {
            let db = self.inner.db.lock().expect("storage");
            (db.root, db.space.clone())
        };

        let mut overlay = Overlay::open(&self.inner.pager, space.clone(), next);
        let mut tree = BTree::from_root(root);
        for (key, value) in &self.stage {
            let result = match value {
                Some(bytes) => tree.insert(&mut overlay, key, bytes),
                None => tree.remove(&mut overlay, key),
            };

            if let Err(err) = result {
                let mapped = map_tree(err);
                if matches!(mapped, StorageError::Io(_)) {
                    self.inner.db.lock().expect("storage").closed = true;
                }
                return Err(mapped);
            }
        }

        let changed = tree.root() != root || overlay.space != space || !overlay.dirty.is_empty();
        if !changed {
            return Ok(base);
        }

        overlay.store_space_images();
        overlay.store_state(tree.root())?;
        let images = overlay.stamped(next)?;
        let dirty_pages = overlay.dirty.len();
        let new_space = overlay.space.clone();
        let new_root = tree.root();
        drop(overlay);

        if dirty_pages > self.limits.max_dirty_pages {
            return Err(StorageError::TransactionTooLarge {
                limit: LimitKind::DirtyPages,
                observed: u64::try_from(dirty_pages).unwrap_or(u64::MAX),
            });
        }

        let wal_bytes = (images.len() as u64).saturating_mul(PAGE_SIZE as u64);
        if wal_bytes > self.limits.max_wal_bytes {
            return Err(StorageError::TransactionTooLarge {
                limit: LimitKind::WalBytes,
                observed: wal_bytes,
            });
        }

        let wal_mode = if wait { durability } else { Durability::None };
        match self.inner.pager.commit_pages(&images, wal_mode) {
            Ok(lsn) => {
                let grouped = wait && matches!(durability, Durability::Grouped { .. });
                {
                    let mut db = self.inner.db.lock().expect("storage");
                    db.root = new_root;
                    db.space = new_space;
                    if grouped {
                        db.writer = false;
                    }
                }
                if let Durability::Grouped { max_delay } = durability
                    && wait
                {
                    self.committed = true;
                    drop(gate);
                    self.join_group(max_delay)?;
                }
                Ok(lsn)
            }
            Err(err) => {
                let mapped = map_pager(err);
                let mut db = self.inner.db.lock().expect("storage");
                if matches!(mapped, StorageError::NoSpace) {
                    db.degraded = true;
                } else {
                    db.closed = true;
                }

                Err(mapped)
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
        self.inner.db.lock().expect("storage").writer = false;
    }
}

/// A commit that is visible in memory and still waiting for its durability sync.
pub struct CommitAck<V: Vfs> {
    storage: Storage<V>,
    durability: Durability,
    lsn: Lsn,
}

impl<V: Vfs> CommitAck<V> {
    /// Log sequence assigned to the commit.
    #[must_use]
    pub const fn lsn(&self) -> Lsn {
        self.lsn
    }

    /// Wait until the durability mode is satisfied.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Io`] when the sync fails. A failure closes the handle.
    pub fn acknowledge(self) -> Result<Lsn, StorageError> {
        match self.durability {
            Durability::Full => self.storage.sync().map(|()| self.lsn),
            Durability::Grouped { max_delay } => {
                join_group(&self.storage.inner, max_delay)?;
                Ok(self.lsn)
            }
            Durability::None | Durability::Deferred { .. } => Ok(self.lsn),
        }
    }
}

fn join_group<V: Vfs>(inner: &Inner<V>, max_delay: Duration) -> Result<(), StorageError> {
    let mut group = inner.sync_group.lock().expect("storage");
    if group.flushing {
        let epoch = group.epoch;
        while group.epoch == epoch {
            group = inner.sync_cv.wait(group).expect("storage");
        }
        return if group.failed {
            Err(StorageError::Io(io_error("group sync failed")))
        } else {
            Ok(())
        };
    }

    group.flushing = true;
    drop(group);
    if !max_delay.is_zero() {
        thread::sleep(max_delay);
    }
    let result = inner.pager.sync_group().map_err(map_pager);
    if result.is_err() {
        inner.pager.discard_wal_tail();
        inner.db.lock().expect("storage").closed = true;
    }
    let mut group = inner.sync_group.lock().expect("storage");
    group.flushing = false;
    group.failed = result.is_err();
    group.epoch = group.epoch.wrapping_add(1);
    inner.sync_cv.notify_all();
    result
}

fn system_now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    u64::try_from(START.get_or_init(Instant::now).elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn stamp(image: &mut [u8; PAGE_SIZE], lsn: Lsn) -> Result<(), StorageError> {
    put_u64(image, LSN_OFFSET, lsn.as_u64()).map_err(|_| StorageError::Corrupt)?;
    seal(image).map_err(|_| StorageError::Corrupt)?;
    Ok(())
}

struct AllocatedPages<'a, M: VfsFile, W: VfsFile> {
    inner: SnapshotPages<'a, M, W>,
    space: &'a Space,
}

impl<M: VfsFile, W: VfsFile> PageRead for AllocatedPages<'_, M, W> {
    type Pin<'a>
        = Vec<u8>
    where
        Self: 'a;

    fn pin(&self, id: PageId) -> Result<Self::Pin<'_>, TreeError> {
        if !self.space.is_allocated(id) {
            return Err(TreeError::Corrupt);
        }
        self.inner.pin(id)
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
        let pin = self.pager.pin(id, self.lsn).map_err(map_pager_tree)?;
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
            .map_err(map_pager_tree)?;

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

        for extra in encoded.extra_bitmaps {
            self.insert(extra.page_id, extra.bytes);
        }

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

    let extras = read_bitmap_chain(pager, lsn, &bitmap)?;
    let space =
        Space::load(&header, &bitmap, &spills, &extras).map_err(|_| StorageError::Corrupt)?;
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

fn read_bitmap_chain<M: VfsFile, W: VfsFile>(
    pager: &Pager<M, W>,
    lsn: Lsn,
    first: &[u8; PAGE_SIZE],
) -> Result<Vec<SpillImage>, StorageError> {
    let mut next = get_u64(first, PAGE_SIZE - 8).map_err(|_| StorageError::Corrupt)?;
    let mut out = Vec::new();
    let mut hops = 0usize;

    while next != 0 {
        hops = hops.saturating_add(1);
        if hops > 65_536 {
            return Err(StorageError::Corrupt);
        }

        let page_id = PageId::new(next);
        let bytes = read_page(pager, lsn, page_id)?;
        next = get_u64(&bytes, PAGE_SIZE - 8).map_err(|_| StorageError::Corrupt)?;
        out.push(SpillImage { page_id, bytes });
    }

    Ok(out)
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

pub(crate) fn decode_state(page: &[u8]) -> Result<(PageId, u64), StorageError> {
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

/// Path of the log beside `path`.
///
/// The file name gains a `-wal` suffix. `db` becomes `db-wal`. The directory
/// is unchanged. An empty path is returned unchanged.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use pizzlydb_storage::wal_path;
///
/// assert_eq!(wal_path(Path::new("db")), Path::new("db-wal"));
/// ```
#[must_use]
pub fn wal_path(path: &Path) -> PathBuf {
    let Some(name) = path.file_name() else {
        return path.to_path_buf();
    };

    let mut suffixed = name.to_os_string();
    suffixed.push("-wal");

    let mut out = path.to_path_buf();
    out.set_file_name(suffixed);
    out
}

pub(crate) fn parent_dir(path: &Path) -> &Path {
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

fn map_pager_tree(err: PagerError) -> TreeError {
    match err {
        PagerError::Io(_) | PagerError::Wal(crate::wal::WalError::Io(_)) => TreeError::Io,
        _ => TreeError::Corrupt,
    }
}

fn map_tree(err: TreeError) -> StorageError {
    match err {
        TreeError::Full | TreeError::KeyTooLarge | TreeError::ValueTooLarge => StorageError::Full,
        TreeError::Io => StorageError::Io(io_error("page read failed")),
        TreeError::Corrupt | TreeError::DoubleFree => StorageError::Corrupt,
    }
}

pub(crate) fn map_pager(err: PagerError) -> StorageError {
    match err {
        PagerError::Io(VfsError::NoSpace) => StorageError::NoSpace,
        PagerError::Io(VfsError::Locked) => StorageError::Locked,
        PagerError::Io(err) => StorageError::Io(err),
        PagerError::Wal(err) => map_wal(err),
        PagerError::PoolExhausted => StorageError::ResourceExhausted {
            resource: "buffer-pool",
        },
        PagerError::ChecksumMismatch { page_id } => StorageError::ChecksumMismatch { page_id },
        PagerError::Corrupt
        | PagerError::SnapshotNotFound
        | PagerError::Shared
        | PagerError::NotDirty => StorageError::Corrupt,
    }
}

fn checker_loop<V: Vfs>(weak: Weak<Inner<V>>) {
    while let Some(inner) = weak.upgrade() {
        if inner.shutdown.load(Ordering::Relaxed) {
            break;
        }

        let policy = *inner.policy.lock().expect("storage");
        let wait = policy
            .map(|policy| policy.max_interval)
            .unwrap_or(Duration::from_secs(1));

        if let Some(policy) = policy {
            let appended = inner.pager.wal_appended_bytes().unwrap_or(0);
            if appended >= policy.wal_soft_bytes {
                let storage = Storage {
                    inner: Arc::clone(&inner),
                };
                let _ = storage.poll_checkpoint();
            }
        }
        drop(inner);

        thread::sleep(if wait.is_zero() {
            Duration::from_millis(10)
        } else {
            wait
        });
    }
}

pub(crate) fn map_wal(err: WalError) -> StorageError {
    match err {
        WalError::Io(VfsError::NoSpace) => StorageError::NoSpace,
        WalError::Io(VfsError::Locked) => StorageError::Locked,
        WalError::Io(err) => StorageError::Io(err),
        WalError::UnsupportedVersion => StorageError::UnsupportedVersion,
        WalError::Torn
        | WalError::ChecksumMismatch
        | WalError::BadMagic
        | WalError::Corrupt
        | WalError::FileIdMismatch => StorageError::Corrupt,
        WalError::Full => StorageError::WalFull,
    }
}

pub(crate) fn map_super(err: SuperblockError) -> StorageError {
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

    #[test]
    fn held_snapshot_blocks_reclaim_until_it_drops() {
        use super::Storage;
        use crate::Durability;
        use crate::vfs_memory::MemoryVfs;

        let vfs = MemoryVfs::new();
        let db = Storage::create(&vfs, Path::new("reclaim"), 8, Durability::Full, |id| {
            id.fill(1);
        })
        .unwrap();
        let blob = vec![9_u8; 20_000];
        let mut batch = db.begin_write().unwrap();
        batch.put(b"k", &blob).unwrap();
        batch.commit(Durability::Full).unwrap();

        let view = db.snapshot().unwrap();
        let mut batch = db.begin_write().unwrap();
        batch.delete(b"k").unwrap();
        batch.commit(Durability::Full).unwrap();

        let retired = db.retired_pages();
        assert!(!retired.is_empty());
        db.checkpoint().unwrap();
        assert_eq!(view.get(b"k").unwrap().as_deref(), Some(blob.as_slice()));
        for id in &retired {
            assert!(db.is_page_allocated(*id));
        }
        assert!(db.reuse_candidate(&retired).is_none());

        drop(view);
        for id in &retired {
            assert!(!db.is_page_allocated(*id));
        }
        assert!(db.reuse_candidate(&retired).is_some());
        assert!(db.snapshot().unwrap().get(b"k").unwrap().is_none());
    }

    #[test]
    fn a_held_snapshot_hits_the_wal_hard_limit() {
        use super::Storage;
        use crate::vfs_memory::MemoryVfs;
        use crate::{CheckpointPolicy, Durability, StorageError};
        use std::time::Duration;

        let vfs = MemoryVfs::new();
        let db = Storage::create(&vfs, Path::new("hard"), 8, Durability::Full, |id| {
            id.fill(2);
        })
        .unwrap();
        let mut batch = db.begin_write().unwrap();
        batch.put(b"k", b"v").unwrap();
        batch.commit(Durability::Full).unwrap();
        db.set_checkpoint_policy(CheckpointPolicy {
            wal_soft_bytes: u64::MAX,
            wal_hard_bytes: 1,
            max_interval: Duration::from_secs(3600),
            page_budget: 0,
        });
        let _view = db.snapshot().unwrap();
        assert!(matches!(
            db.begin_write(),
            Err(StorageError::ResourceExhausted { resource: "wal" })
        ));
    }

    #[test]
    fn page_budget_seals_the_superblock_only_after_the_copy() {
        use super::Storage;
        use crate::vfs_memory::MemoryVfs;
        use crate::{CheckpointPolicy, Durability};
        use std::time::Duration;

        let vfs = MemoryVfs::new();
        let db = Storage::create(&vfs, Path::new("budget"), 8, Durability::Full, |id| {
            id.fill(3);
        })
        .unwrap();
        let mut batch = db.begin_write().unwrap();
        for key in 0..8_u8 {
            batch.put(&[key], &[key]).unwrap();
        }
        batch.commit(Durability::Full).unwrap();
        let before = db.inner.pager.wal_appended_bytes().unwrap();
        db.set_checkpoint_policy(CheckpointPolicy {
            wal_soft_bytes: 1,
            wal_hard_bytes: u64::MAX,
            max_interval: Duration::from_secs(3600),
            page_budget: 1,
        });
        db.poll_checkpoint().unwrap();
        assert_eq!(db.inner.pager.wal_appended_bytes().unwrap(), before);
        let mut sealed = false;
        for _ in 0..64 {
            db.poll_checkpoint().unwrap();
            if db.inner.pager.wal_appended_bytes().unwrap() < before {
                sealed = true;
                break;
            }
        }
        assert!(sealed, "budgeted checkpoint never sealed the superblock");
    }

    #[cfg(unix)]
    #[test]
    fn a_sparse_file_covers_two_gibibytes_of_page_ids() {
        use super::STATE_PAGE_ID;
        use super::{fresh_superblock, offset_of};
        use crate::page::PAGE_SIZE;
        use crate::space::Space;
        use crate::superblock::{COPY_A, COPY_B};
        use crate::verify::verify_quick;
        use crate::vfs::{FileRole, OpenOptions, Vfs, VfsFile};
        use crate::vfs_posix::PosixVfs;
        use crate::wal::Wal;

        let mut space = Space::create();
        let state_id = space.allocate(crate::page::PageKind::State).unwrap();
        assert_eq!(state_id, STATE_PAGE_ID);
        let pages = (2u64 * 1024 * 1024 * 1024) / u64::try_from(PAGE_SIZE).unwrap();
        space.grow_to(pages).unwrap();
        assert!(space.high_water() * u64::try_from(PAGE_SIZE).unwrap() >= 2 * 1024 * 1024 * 1024);
        let encoded = space.encode();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wide");
        let vfs = PosixVfs::new();
        let mut main = vfs
            .open(
                &path,
                FileRole::Main,
                &OpenOptions::new().create(true).exclusive(true),
            )
            .unwrap();
        main.set_len(space.high_water() * u64::try_from(PAGE_SIZE).unwrap())
            .unwrap();
        let file_id = [4u8; 16];
        let record = fresh_superblock(file_id, 1, space.high_water())
            .unwrap()
            .with_wal_generation(1);
        let older = fresh_superblock(file_id, 0, space.high_water())
            .unwrap()
            .with_wal_generation(1);
        main.write_all(0, &record.encode(COPY_A).unwrap()).unwrap();
        main.write_all(
            u64::try_from(PAGE_SIZE).unwrap(),
            &older.encode(COPY_B).unwrap(),
        )
        .unwrap();
        main.write_all(
            offset_of(crate::page::ALLOCATOR_PAGE_ID).unwrap(),
            &encoded.header,
        )
        .unwrap();
        main.write_all(
            offset_of(crate::page::BITMAP_PAGE_ID).unwrap(),
            &encoded.bitmap,
        )
        .unwrap();
        for extra in &encoded.extra_bitmaps {
            main.write_all(offset_of(extra.page_id).unwrap(), &extra.bytes)
                .unwrap();
        }
        let state = encode_state(PageId::new(0), space.high_water(), Lsn::new(0)).unwrap();
        main.write_all(offset_of(STATE_PAGE_ID).unwrap(), &state)
            .unwrap();
        main.sync().unwrap();
        drop(main);
        let wal = vfs
            .open(
                &super::wal_path(&path),
                FileRole::Wal,
                &OpenOptions::new().create(true).exclusive(true),
            )
            .unwrap();
        drop(Wal::create(wal, file_id).unwrap());
        vfs.sync_dir(dir.path()).unwrap();
        verify_quick(&vfs, &path).unwrap();
    }
}
