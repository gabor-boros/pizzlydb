//! Fixed-size buffer pool for page images.
//!
//! A frame stores one version of one page. The cache key is the [`PageId`] and
//! the [`Lsn`] in that frame, so two versions of one page can sit in two frames.
//! [`Pin`] decreases that frame's pin count when it drops. [`SnapshotGuard`]
//! removes its registration when it drops.
//!
//! Eviction walks the frames with a CLOCK hand and gives a referenced frame one
//! more pass. It does not select a frame that is pinned, dirty, or not durable
//! in the write-ahead log or the main file. If no frame qualifies,
//! [`Pager::pin`] returns [`PagerError::PoolExhausted`].
//!
//! [`Pager::pin`] requires a registered snapshot LSN. It returns the newest
//! committed image at or before that LSN: a clean cached frame, otherwise the
//! newest matching write-ahead-log image, otherwise the main-file page. A
//! main-file image newer than the snapshot is [`PagerError::SnapshotNotFound`]
//! and is not cached. Dirty frames are skipped by that search.
//!
//! [`Pager::commit_frame`] appends the dirty image, syncs the write-ahead-log
//! commit, and only then clears the dirty bit. A failed sync leaves the bit
//! set. [`Pager::write_main`] copies the current frame into the main file and
//! does not sync that file or clear the dirty bit.
//!
//! [`PagerStats`] counts hits, misses, evictions, and I/O. Those counters are
//! not a durability signal.
//!
//! [`Pager`] is not [`Sync`]. One thread mutates it. [`Pin`] and
//! [`SnapshotGuard`] borrow that pager and are not [`Sync`] either. Move the
//! pager to another thread only when no guard is alive.

use std::cell::{Ref, RefCell};
use std::ops::Deref;

use thiserror::Error;

use crate::codec::{CodecError, put_u64};
use crate::page::{LSN_OFFSET, Lsn, PAGE_SIZE, PageHeader, PageId, seal};
use crate::vfs::{VfsError, VfsFile};
use crate::wal::{CommittedPage, Durability, Wal, WalError};

/// Failure while pinning, admitting, or publishing a page image.
///
/// # Examples
///
/// ```ignore
/// use pizzlydb_storage::pager::PagerError;
///
/// let err = PagerError::PoolExhausted;
/// assert!(matches!(err, PagerError::PoolExhausted));
/// ```
#[derive(Debug, Error)]
pub enum PagerError {
    /// Every frame is pinned, dirty, or not durable, or `frame_count` was zero.
    #[error("buffer pool exhausted")]
    PoolExhausted,
    /// `pin` was called with an LSN that no snapshot guard currently registers.
    #[error("snapshot is not registered")]
    SnapshotNotFound,
    /// The page image failed its CRC-32C check.
    #[error("page checksum mismatch")]
    ChecksumMismatch,
    /// The image is the wrong length, has a bad header, or its page id does not
    /// match the requested page. Also returned when the committed LSN cannot
    /// advance.
    #[error("page image rejected")]
    Corrupt,
    /// `replace` or `commit_frame` found another guard on the frame, or a
    /// [`Bytes`] guard still borrows the image.
    #[error("frame is held by another guard")]
    Shared,
    /// [`Pager::pin_dirty`] found no dirty frame for the page.
    #[error("no dirty image for this page")]
    NotDirty,
    /// The main file returned an error.
    #[error("pager i/o failed")]
    Io(#[source] VfsError),
    /// The write-ahead log returned an error.
    #[error("write-ahead log failed")]
    Wal(#[source] WalError),
}

impl From<VfsError> for PagerError {
    fn from(err: VfsError) -> Self {
        Self::Io(err)
    }
}

impl From<WalError> for PagerError {
    fn from(err: WalError) -> Self {
        Self::Wal(err)
    }
}

/// Hit, miss, eviction, and I/O counters.
///
/// The counters describe cache traffic. They do not report whether a commit
/// reached disk.
///
/// # Examples
///
/// ```ignore
/// use std::path::Path;
///
/// use pizzlydb_storage::page::{Lsn, PageHeader, PageId, PageKind, PAGE_SIZE};
/// use pizzlydb_storage::pager::Pager;
/// use pizzlydb_storage::vfs::{FileRole, MemoryVfs, OpenOptions, Vfs, VfsFile};
/// use pizzlydb_storage::wal::Wal;
///
/// let vfs = MemoryVfs::new();
/// let mut main = vfs.open(
///     Path::new("db"),
///     FileRole::Main,
///     &OpenOptions::new().create(true),
/// ).unwrap();
///
/// let mut image = [0x11; PAGE_SIZE];
///
/// PageHeader::new(PageKind::BTreeLeaf, PageId::new(0), Lsn::new(0))
///     .encode(&mut image)
///     .unwrap();
///
/// main.write_all(0, &image).unwrap();
///
/// let wal_file = vfs.open(
///     Path::new("db.wal"),
///     FileRole::Wal,
///     &OpenOptions::new().create(true),
/// ).unwrap();
///
/// let wal = Wal::create(wal_file, [7; 16]).unwrap();
/// let pager = Pager::open(main, wal, 1).unwrap();
/// let snapshot = pager.snapshot();
/// let pin = pager.pin(PageId::new(0), snapshot.lsn()).unwrap();
///
/// assert_eq!(pager.stats().misses(), 1);
/// assert_eq!(pager.stats().read_io(), 1);
/// assert_eq!(pager.stats().hits(), 0);
/// assert_eq!(pager.stats().evictions(), 0);
/// assert_eq!(pager.stats().write_io(), 0);
/// drop(pin);
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PagerStats {
    hits: u64,
    misses: u64,
    evictions: u64,
    read_io: u64,
    write_io: u64,
}

impl PagerStats {
    /// Pins served from a clean cached image.
    #[must_use]
    pub const fn hits(&self) -> u64 {
        self.hits
    }

    /// Pins that admitted an image into a frame.
    #[must_use]
    pub const fn misses(&self) -> u64 {
        self.misses
    }

    /// Occupied frames replaced by a later admit.
    #[must_use]
    pub const fn evictions(&self) -> u64 {
        self.evictions
    }

    /// Main-file page reads that were admitted.
    #[must_use]
    pub const fn read_io(&self) -> u64 {
        self.read_io
    }

    /// Main-file page writes performed by [`Pager::write_main`].
    #[must_use]
    pub const fn write_io(&self) -> u64 {
        self.write_io
    }
}

/// Bounded page cache over a main file and a write-ahead log.
///
/// `frame_count` is fixed for the life of the pager and is at least 1.
/// Dropping the pager drops both files and does not sync them.
///
/// `M` is the main file. `W` is the log file. The pager is not [`Sync`].
///
/// # Examples
///
/// ```ignore
/// use std::path::Path;
///
/// use pizzlydb_storage::page::Lsn;
/// use pizzlydb_storage::pager::Pager;
/// use pizzlydb_storage::vfs::{FileRole, MemoryVfs, OpenOptions, Vfs};
/// use pizzlydb_storage::wal::Wal;
///
/// let vfs = MemoryVfs::new();
/// let main = vfs.open(
///     Path::new("db"),
///     FileRole::Main,
///     &OpenOptions::new().create(true),
/// ).unwrap();
///
/// let wal_file = vfs.open(
///     Path::new("db.wal"),
///     FileRole::Wal,
///     &OpenOptions::new().create(true),
/// ).unwrap();
///
/// let wal = Wal::create(wal_file, [7; 16]).unwrap();
/// let pager = Pager::open(main, wal, 2).unwrap();
///
/// assert_eq!(pager.snapshot_lsn(), Lsn::new(1));
/// ```
#[must_use = "dropping the pager drops both files and does not sync them"]
pub struct Pager<M: VfsFile, W: VfsFile> {
    frames: Vec<RefCell<[u8; PAGE_SIZE]>>,
    state: RefCell<State<M, W>>,
}

impl<M: VfsFile, W: VfsFile> Pager<M, W> {
    /// Allocate `frame_count` frames over `main` and `wal`.
    ///
    /// The committed LSN starts at [`Wal::durable_lsn`]. A `frame_count` of
    /// zero returns [`PagerError::PoolExhausted`] and drops both handles.
    ///
    /// # Errors
    ///
    /// Returns [`PagerError::PoolExhausted`] when `frame_count` is zero.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::pager::{Pager, PagerError};
    /// use pizzlydb_storage::vfs::{FileRole, MemoryVfs, OpenOptions, Vfs};
    /// use pizzlydb_storage::wal::Wal;
    ///
    /// let vfs = MemoryVfs::new();
    /// let main = vfs.open(
    ///     Path::new("db"),
    ///     FileRole::Main,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// let wal_file = vfs.open(
    ///     Path::new("db.wal"),
    ///     FileRole::Wal,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// let wal = Wal::create(wal_file, [7; 16]).unwrap();
    ///
    /// let err = Pager::open(main, wal, 0).unwrap_err();
    /// assert!(matches!(err, PagerError::PoolExhausted));
    /// ```
    pub fn open(main: M, wal: Wal<W>, frame_count: usize) -> Result<Self, PagerError> {
        if frame_count == 0 {
            return Err(PagerError::PoolExhausted);
        }

        let mut frames = Vec::with_capacity(frame_count);
        for _ in 0..frame_count {
            frames.push(RefCell::new([0u8; PAGE_SIZE]));
        }

        let committed_lsn = wal.durable_lsn();
        Ok(Self {
            frames,
            state: RefCell::new(State {
                main,
                wal,
                meta: vec![FrameMeta::empty(); frame_count],
                clock_hand: 0,
                snapshots: Vec::new(),
                next_snapshot_id: 1,
                next_txn_id: 1,
                committed_lsn,
                stats: PagerStats::default(),
            }),
        })
    }

    /// Register the current committed LSN and return a guard.
    ///
    /// The guard's LSN does not move when a later [`Self::commit_frame`]
    /// advances the pager. Drop the guard to unregister it.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::page::Lsn;
    /// use pizzlydb_storage::pager::Pager;
    /// use pizzlydb_storage::vfs::{FileRole, MemoryVfs, OpenOptions, Vfs};
    /// use pizzlydb_storage::wal::Wal;
    ///
    /// let vfs = MemoryVfs::new();
    /// let main = vfs.open(
    ///     Path::new("db"),
    ///     FileRole::Main,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// let wal_file = vfs.open(
    ///     Path::new("db.wal"),
    ///     FileRole::Wal,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// let wal = Wal::create(wal_file, [7; 16]).unwrap();
    /// let pager = Pager::open(main, wal, 1).unwrap();
    /// let snapshot = pager.snapshot();
    ///
    /// assert_eq!(snapshot.lsn(), Lsn::new(0));
    /// assert_eq!(pager.snapshot_lsn(), Lsn::new(0));
    /// drop(snapshot);
    /// assert_eq!(pager.snapshot_lsn(), Lsn::new(1));
    /// ```
    pub fn snapshot(&self) -> SnapshotGuard<'_, M, W> {
        let (id, lsn) = {
            let mut state = self.state.borrow_mut();
            let id = state.next_snapshot_id;
            state.next_snapshot_id = state.next_snapshot_id.saturating_add(1);

            let lsn = state.committed_lsn;
            state.snapshots.push((id, lsn));

            (id, lsn)
        };

        SnapshotGuard {
            pager: self,
            id,
            lsn,
        }
    }

    /// Pin the newest committed image of `page_id` visible at `lsn`.
    ///
    /// `lsn` must be registered by a live [`SnapshotGuard`]. The returned pin
    /// shares `&Self` with any other pins. Cache hits increment
    /// [`PagerStats::hits`]. An admitted image increments [`PagerStats::misses`].
    /// A main-file admit also increments [`PagerStats::read_io`]. A rejected
    /// read, checksum, or full pool does not.
    ///
    /// # Errors
    ///
    /// Returns [`PagerError::SnapshotNotFound`] when `lsn` is not registered,
    /// or when the main-file image is newer than `lsn`. Returns
    /// [`PagerError::PoolExhausted`] when no frame can be replaced. Returns
    /// [`PagerError::ChecksumMismatch`], [`PagerError::Corrupt`], or
    /// [`PagerError::Io`] when the image cannot be admitted.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::page::{
    ///     HEADER_SIZE, Lsn, PageHeader, PageId, PageKind, PAGE_SIZE,
    /// };
    /// use pizzlydb_storage::pager::Pager;
    /// use pizzlydb_storage::vfs::{FileRole, MemoryVfs, OpenOptions, Vfs, VfsFile};
    /// use pizzlydb_storage::wal::Wal;
    ///
    /// let vfs = MemoryVfs::new();
    /// let mut main = vfs .open(
    ///     Path::new("db"),
    ///     FileRole::Main,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// let mut image = [0x11; PAGE_SIZE];
    /// PageHeader::new(PageKind::BTreeLeaf, PageId::new(0), Lsn::new(0))
    ///     .encode(&mut image)
    ///     .unwrap();
    ///
    /// main.write_all(0, &image).unwrap();
    ///
    /// let wal_file = vfs.open(
    ///     Path::new("db.wal"),
    ///     FileRole::Wal,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// let wal = Wal::create(wal_file, [7; 16]).unwrap();
    /// let pager = Pager::open(main, wal, 1).unwrap();
    /// let snapshot = pager.snapshot();
    /// let pin = pager.pin(PageId::new(0), snapshot.lsn()).unwrap();
    ///
    /// assert_eq!(pin.page_id(), PageId::new(0));
    /// assert_eq!(pin.lsn(), Lsn::new(0));
    /// assert_eq!(pin.bytes()[HEADER_SIZE], 0x11);
    /// ```
    pub fn pin(&self, page_id: PageId, lsn: Lsn) -> Result<Pin<'_, M, W>, PagerError> {
        let frames = &self.frames;
        let index = {
            let mut state = self.state.borrow_mut();
            if !state.registered(lsn) {
                return Err(PagerError::SnapshotNotFound);
            }

            if let Some(index) = state.find_cached(page_id, lsn) {
                note_hit(&mut state, index)?;
                index
            } else {
                let loaded = state.load_image(page_id, lsn)?;
                let index = state.choose_victim()?;
                install(&mut state, frames, index, &loaded);
                bump(&mut state.stats.misses);

                if loaded.durable_main {
                    bump(&mut state.stats.read_io);
                }

                index
            }
        };

        Ok(Pin { pager: self, index })
    }

    /// Pin the dirty frame for `page_id`.
    ///
    /// This is the writer path. It does not consult snapshot registrations and
    /// does not change [`PagerStats`]. The image may be unpublished.
    ///
    /// # Errors
    ///
    /// Returns [`PagerError::NotDirty`] when no dirty frame holds `page_id`.
    /// Returns [`PagerError::Corrupt`] when the pin count cannot increase.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::page::{Lsn, PageHeader, PageId, PageKind, PAGE_SIZE};
    /// use pizzlydb_storage::pager::Pager;
    /// use pizzlydb_storage::vfs::{FileRole, MemoryVfs, OpenOptions, Vfs, VfsFile};
    /// use pizzlydb_storage::wal::Wal;
    ///
    /// let vfs = MemoryVfs::new();
    /// let mut main = vfs.open(
    ///     Path::new("db"),
    ///     FileRole::Main,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// let mut image = [0x11; PAGE_SIZE];
    ///
    /// PageHeader::new(PageKind::BTreeLeaf, PageId::new(0), Lsn::new(0))
    ///     .encode(&mut image)
    ///     .unwrap();
    ///
    /// main.write_all(0, &image).unwrap();
    ///
    /// let wal_file = vfs.open(
    ///     Path::new("db.wal"),
    ///     FileRole::Wal,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// let wal = Wal::create(wal_file, [7; 16]).unwrap();
    /// let pager = Pager::open(main, wal, 1).unwrap();
    /// let snapshot = pager.snapshot();
    /// let pin = pager.pin(PageId::new(0), snapshot.lsn()).unwrap();
    /// pin.mark_dirty();
    /// drop(pin);
    ///
    /// let dirty = pager.pin_dirty(PageId::new(0)).unwrap();
    /// assert_eq!(dirty.page_id(), PageId::new(0));
    /// ```
    pub fn pin_dirty(&self, page_id: PageId) -> Result<Pin<'_, M, W>, PagerError> {
        let index = {
            let mut state = self.state.borrow_mut();

            let index = state
                .meta
                .iter()
                .position(|frame| frame.occupied && frame.dirty && frame.page_id == page_id)
                .ok_or(PagerError::NotDirty)?;

            let count = state.meta[index]
                .pin_count
                .checked_add(1)
                .ok_or(PagerError::Corrupt)?;

            state.meta[index].pin_count = count;
            state.meta[index].referenced = true;
            index
        };

        Ok(Pin { pager: self, index })
    }

    /// Oldest registered snapshot LSN.
    ///
    /// With no guards, the result is one past the committed LSN. At
    /// [`u64::MAX`] the result stays [`u64::MAX`].
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::page::Lsn;
    /// use pizzlydb_storage::pager::Pager;
    /// use pizzlydb_storage::vfs::{FileRole, MemoryVfs, OpenOptions, Vfs};
    /// use pizzlydb_storage::wal::Wal;
    ///
    /// let vfs = MemoryVfs::new();
    /// let main = vfs.open(
    ///     Path::new("db"),
    ///     FileRole::Main,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// let wal_file = vfs.open(
    ///     Path::new("db.wal"),
    ///     FileRole::Wal,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// let wal = Wal::create(wal_file, [7; 16]).unwrap();
    /// let pager = Pager::open(main, wal, 1).unwrap();
    ///
    /// assert_eq!(pager.snapshot_lsn(), Lsn::new(1));
    /// let snapshot = pager.snapshot();
    /// assert_eq!(pager.snapshot_lsn(), snapshot.lsn());
    /// ```
    #[must_use]
    pub fn snapshot_lsn(&self) -> Lsn {
        let state = self.state.borrow();
        let oldest = state.snapshots.iter().map(|(_, lsn)| *lsn).min();
        oldest.unwrap_or_else(|| next_lsn(state.committed_lsn))
    }

    /// Copy of the cache counters.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::pager::Pager;
    /// use pizzlydb_storage::vfs::{FileRole, MemoryVfs, OpenOptions, Vfs};
    /// use pizzlydb_storage::wal::Wal;
    ///
    /// let vfs = MemoryVfs::new();
    /// let main = vfs.open(
    ///     Path::new("db"),
    ///     FileRole::Main,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// let wal_file = vfs.open(
    ///     Path::new("db.wal"),
    ///     FileRole::Wal,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// let wal = Wal::create(wal_file, [7; 16]).unwrap();
    /// let pager = Pager::open(main, wal, 1).unwrap();
    ///
    /// assert_eq!(pager.stats().hits(), 0);
    /// assert_eq!(pager.stats().write_io(), 0);
    /// ```
    #[must_use]
    pub fn stats(&self) -> PagerStats {
        self.state.borrow().stats
    }

    /// Publish `pin`'s dirty image.
    ///
    /// The frame must have a pin count of one. The method assigns the next
    /// LSN, seals the image, appends it, and syncs the write-ahead-log commit.
    /// It clears the dirty bit only after that sync returns. A clean frame
    /// returns its current LSN and writes nothing.
    ///
    /// # Errors
    ///
    /// Returns [`PagerError::Shared`] when another pin holds the frame or a
    /// [`Bytes`] guard borrows it. Returns [`PagerError::Corrupt`] when the
    /// LSN cannot advance or the image cannot be sealed. Returns
    /// [`PagerError::Wal`] when the append or sync fails. The frame stays
    /// dirty in that case.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::page::{Lsn, PageHeader, PageId, PageKind, PAGE_SIZE};
    /// use pizzlydb_storage::pager::Pager;
    /// use pizzlydb_storage::vfs::{FileRole, MemoryVfs, OpenOptions, Vfs, VfsFile};
    /// use pizzlydb_storage::wal::Wal;
    ///
    /// let vfs = MemoryVfs::new();
    /// let mut main = vfs.open(
    ///     Path::new("db"),
    ///     FileRole::Main,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// let mut image = [0x11; PAGE_SIZE];
    /// PageHeader::new(PageKind::BTreeLeaf, PageId::new(0), Lsn::new(0))
    ///     .encode(&mut image)
    ///     .unwrap();
    ///
    /// main.write_all(0, &image).unwrap();
    ///
    /// let wal_file = vfs.open(
    ///     Path::new("db.wal"),
    ///     FileRole::Wal,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// let wal = Wal::create(wal_file, [7; 16]).unwrap();
    /// let pager = Pager::open(main, wal, 1).unwrap();
    /// let snapshot = pager.snapshot();
    /// let pin = pager.pin(PageId::new(0), snapshot.lsn()).unwrap();
    /// pin.mark_dirty();
    ///
    /// assert_eq!(pager.commit_frame(&pin).unwrap(), Lsn::new(1));
    /// assert_eq!(pin.lsn(), Lsn::new(1));
    /// ```
    pub fn commit_frame(&self, pin: &Pin<'_, M, W>) -> Result<Lsn, PagerError> {
        let frames = &self.frames;
        let mut state = self.state.borrow_mut();
        let (page_id, current_lsn, dirty) = {
            let frame = &state.meta[pin.index];
            (frame.page_id, frame.lsn, frame.dirty)
        };

        if state.meta[pin.index].pin_count != 1 {
            return Err(PagerError::Shared);
        }

        if !dirty {
            return Ok(current_lsn);
        }

        let new_lsn = next_lsn(state.committed_lsn);
        if new_lsn.as_u64() == state.committed_lsn.as_u64() {
            return Err(PagerError::Corrupt);
        }

        let mut image = {
            let slot = frames[pin.index]
                .try_borrow_mut()
                .map_err(|_| PagerError::Shared)?;
            *slot
        };

        put_u64(&mut image, LSN_OFFSET, new_lsn.as_u64()).map_err(|_| PagerError::Corrupt)?;
        seal(&mut image).map_err(|_| PagerError::Corrupt)?;

        let txn_id = state.next_txn_id;
        state
            .wal
            .append_page_image(txn_id, page_id, new_lsn, &image)?;

        state.wal.commit(Durability::Full, txn_id, new_lsn, 1)?;
        *frames[pin.index].borrow_mut() = image;

        state.meta[pin.index].lsn = new_lsn;
        state.meta[pin.index].dirty = false;
        state.meta[pin.index].durable_wal = true;
        state.meta[pin.index].durable_main = false;
        state.meta[pin.index].referenced = true;
        state.meta[pin.index].occupied = true;
        state.committed_lsn = new_lsn;
        state.next_txn_id = state.next_txn_id.saturating_add(1);
        Ok(new_lsn)
    }

    /// Copy `pin`'s image into the main file at its page offset.
    ///
    /// Increments [`PagerStats::write_io`] and marks the frame durable in the
    /// main file. Does not sync the file, does not append to the write-ahead
    /// log, and does not clear the dirty bit.
    ///
    /// # Errors
    ///
    /// Returns [`PagerError::Corrupt`] when the page offset overflows and
    /// [`PagerError::Io`] when the write fails. A failed write leaves the
    /// durability flag unchanged.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::page::{Lsn, PageHeader, PageId, PageKind, PAGE_SIZE};
    /// use pizzlydb_storage::pager::Pager;
    /// use pizzlydb_storage::vfs::{FileRole, MemoryVfs, OpenOptions, Vfs, VfsFile};
    /// use pizzlydb_storage::wal::Wal;
    ///
    /// let vfs = MemoryVfs::new();
    /// let mut main = vfs.open(
    ///     Path::new("db"),
    ///     FileRole::Main,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// let mut image = [0x11; PAGE_SIZE];
    /// PageHeader::new(PageKind::BTreeLeaf, PageId::new(0), Lsn::new(0))
    ///     .encode(&mut image)
    ///     .unwrap();
    ///
    /// main.write_all(0, &image).unwrap();
    ///
    /// let wal_file = vfs.open(
    ///     Path::new("db.wal"),
    ///     FileRole::Wal,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// let wal = Wal::create(wal_file, [7; 16]).unwrap();
    /// let pager = Pager::open(main, wal, 1).unwrap();
    /// let snapshot = pager.snapshot();
    /// let pin = pager.pin(PageId::new(0), snapshot.lsn()).unwrap();
    /// pager.write_main(&pin).unwrap();
    ///
    /// assert_eq!(pager.stats().write_io(), 1);
    /// ```
    pub fn write_main(&self, pin: &Pin<'_, M, W>) -> Result<(), PagerError> {
        let frames = &self.frames;
        let (page_id, image) = {
            let state = self.state.borrow();
            let page_id = state.meta[pin.index].page_id;
            let image = *frames[pin.index].borrow();
            (page_id, image)
        };

        let offset = page_offset(page_id)?;
        let mut state = self.state.borrow_mut();
        state.main.write_all(offset, &image)?;
        bump(&mut state.stats.write_io);
        state.meta[pin.index].durable_main = true;

        Ok(())
    }

    /// Last published commit sequence.
    #[must_use]
    pub(crate) fn committed_lsn(&self) -> Lsn {
        self.state.borrow().committed_lsn
    }

    /// Bytes of `page_id` visible at `snapshot`, without checking the header.
    ///
    /// A log image wins over the main file. The caller registers `snapshot`.
    ///
    /// # Errors
    ///
    /// Returns [`PagerError::SnapshotNotFound`] when `snapshot` is not
    /// registered, and [`PagerError::Io`] when the main file cannot be read.
    pub(crate) fn read_image(
        &self,
        page_id: PageId,
        snapshot: Lsn,
    ) -> Result<[u8; PAGE_SIZE], PagerError> {
        let from_log = {
            let state = self.state.borrow();
            if !state.registered(snapshot) {
                return Err(PagerError::SnapshotNotFound);
            }
            state.wal_image(page_id, snapshot)?
        };
        if let Some(bytes) = from_log {
            return Ok(bytes);
        }

        let offset = page_offset(page_id)?;
        let mut bytes = [0u8; PAGE_SIZE];
        self.state
            .borrow_mut()
            .main
            .read_exact(offset, &mut bytes)?;
        Ok(bytes)
    }

    /// Length of the main file.
    ///
    /// # Errors
    ///
    /// Returns [`PagerError::Io`] when the length cannot be read.
    pub(crate) fn main_len(&self) -> Result<u64, PagerError> {
        Ok(self.state.borrow().main.len()?)
    }

    /// Oldest registered snapshot, if any guard is alive.
    #[must_use]
    pub(crate) fn oldest_snapshot(&self) -> Option<Lsn> {
        self.state
            .borrow()
            .snapshots
            .iter()
            .map(|(_, lsn)| *lsn)
            .min()
    }

    /// Page ids that have a published log image at or before `snapshot`.
    #[must_use]
    pub(crate) fn wal_page_ids(&self, snapshot: Lsn) -> Vec<PageId> {
        let state = self.state.borrow();
        let mut ids = Vec::new();
        for page in state.wal.pages() {
            if page.lsn() <= snapshot && !ids.contains(&page.page_id()) {
                ids.push(page.page_id());
            }
        }
        ids
    }

    /// Publish every image in one log transaction.
    ///
    /// `pages` must be non-empty. Each image is already sealed at the next
    /// commit sequence. [`Durability::Full`] syncs before that sequence is
    /// recorded. [`Durability::None`] records it without syncing. A failed
    /// append discards the unsynced tail. A failed sync does the same inside
    /// [`Wal::commit`](crate::wal::Wal::commit). The recorded sequence stays
    /// unchanged on either failure.
    ///
    /// # Errors
    ///
    /// Returns [`PagerError::Corrupt`] when `pages` is empty or the sequence
    /// cannot advance, and [`PagerError::Wal`] when the log rejects the batch.
    pub(crate) fn commit_pages(
        &self,
        pages: &[(PageId, [u8; PAGE_SIZE])],
        durability: Durability,
    ) -> Result<Lsn, PagerError> {
        if pages.is_empty() {
            return Err(PagerError::Corrupt);
        }

        let mut state = self.state.borrow_mut();
        let new_lsn = next_lsn(state.committed_lsn);
        if new_lsn.as_u64() == state.committed_lsn.as_u64() {
            return Err(PagerError::Corrupt);
        }

        let txn_id = state.next_txn_id;
        if txn_id == 0 {
            return Err(PagerError::Corrupt);
        }

        let page_count = u64::try_from(pages.len()).map_err(|_| PagerError::Corrupt)?;
        for (page_id, image) in pages {
            if let Err(err) = state
                .wal
                .append_page_image(txn_id, *page_id, new_lsn, image)
            {
                state.wal.abandon_unsynced();
                return Err(err.into());
            }
        }

        state.wal.commit(durability, txn_id, new_lsn, page_count)?;
        state.committed_lsn = new_lsn;
        state.next_txn_id = state.next_txn_id.saturating_add(1);
        Ok(new_lsn)
    }

    /// Sync the main file as it stands.
    ///
    /// # Errors
    ///
    /// Returns [`PagerError::Io`] when the handle's sync fails.
    pub(crate) fn sync_main(&self) -> Result<(), PagerError> {
        self.state.borrow_mut().main.sync()?;
        Ok(())
    }

    /// Sync the log without publishing a commit.
    ///
    /// # Errors
    ///
    /// Returns [`PagerError::Wal`] when the log sync fails. The recorded
    /// sequence is unchanged.
    pub(crate) fn sync_wal(&self) -> Result<(), PagerError> {
        self.state.borrow_mut().wal.sync()?;
        Ok(())
    }

    /// Write `image` to the main file at `page_id` and drop an unpinned cache frame.
    ///
    /// This does not sync the file and does not append a log record.
    ///
    /// # Errors
    ///
    /// Returns [`PagerError::Corrupt`] when `image` is not one page or the
    /// offset overflows, and [`PagerError::Io`] when the write fails.
    pub(crate) fn write_page(&self, page_id: PageId, image: &[u8]) -> Result<(), PagerError> {
        if image.len() != PAGE_SIZE {
            return Err(PagerError::Corrupt);
        }

        let offset = page_offset(page_id)?;
        let mut state = self.state.borrow_mut();
        state.main.write_all(offset, image)?;
        bump(&mut state.stats.write_io);

        for frame in &mut state.meta {
            if frame.occupied && frame.page_id == page_id && frame.pin_count == 0 {
                *frame = FrameMeta::empty();
            }
        }

        Ok(())
    }

    /// Apply the superblock checkpoint sequence when the log has no commits.
    ///
    /// A log whose recorded sequence is below `checkpoint` is [`PagerError::Corrupt`].
    pub(crate) fn recover_floor(&self, checkpoint: Lsn) -> Result<(), PagerError> {
        let mut state = self.state.borrow_mut();
        let durable = state.wal.durable_lsn();
        if durable.as_u64() == 0 {
            state.wal.adopt_floor(checkpoint);
            if checkpoint.as_u64() > state.committed_lsn.as_u64() {
                state.committed_lsn = checkpoint;
            }
            return Ok(());
        }

        if durable < checkpoint {
            return Err(PagerError::Corrupt);
        }

        Ok(())
    }

    /// Remove log images at or below `horizon`.
    ///
    /// # Errors
    ///
    /// Returns [`PagerError::Wal`] when the rewrite fails.
    pub(crate) fn prune_wal(&self, horizon: Lsn) -> Result<(), PagerError> {
        self.state.borrow_mut().wal.prune(horizon)?;
        Ok(())
    }
}

/// A pinned frame.
///
/// The pin count decreases by one when this value drops. [`Pin`] is not
/// [`Copy`], [`Clone`], or [`Sync`]. [`Self::bytes`] borrows this value, so
/// the pin cannot drop while that view is alive.
///
/// # Examples
///
/// ```ignore
/// use std::path::Path;
///
/// use pizzlydb_storage::page::{Lsn, PageHeader, PageId, PageKind, PAGE_SIZE};
/// use pizzlydb_storage::pager::Pager;
/// use pizzlydb_storage::vfs::{FileRole, MemoryVfs, OpenOptions, Vfs, VfsFile};
/// use pizzlydb_storage::wal::Wal;
///
/// let vfs = MemoryVfs::new();
/// let mut main = vfs.open(
///     Path::new("db"),
///     FileRole::Main,
///     &OpenOptions::new().create(true),
/// ).unwrap();
///
/// let mut image = [0x11; PAGE_SIZE];
/// PageHeader::new(PageKind::BTreeLeaf, PageId::new(0), Lsn::new(0))
///     .encode(&mut image)
///     .unwrap();
///
/// main.write_all(0, &image).unwrap();
///
/// let wal_file = vfs.open(
///     Path::new("db.wal"),
///     FileRole::Wal,
///     &OpenOptions::new().create(true),
/// ).unwrap();
///
/// let wal = Wal::create(wal_file, [7; 16]).unwrap();
/// let pager = Pager::open(main, wal, 1).unwrap();
/// let snapshot = pager.snapshot();
/// let pin = pager.pin(PageId::new(0), snapshot.lsn()).unwrap();
///
/// assert_eq!(pin.page_id(), PageId::new(0));
/// assert_eq!(pin.lsn(), Lsn::new(0));
/// ```
#[must_use = "dropping the pin unpins the frame"]
pub struct Pin<'a, M: VfsFile, W: VfsFile> {
    pager: &'a Pager<M, W>,
    index: usize,
}

impl<M: VfsFile, W: VfsFile> Pin<'_, M, W> {
    /// Borrow the cached image.
    ///
    /// The view dereferences to the page bytes and borrows this pin.
    /// [`Pager::commit_frame`] and [`Self::replace`] return
    /// [`PagerError::Shared`] while the view is alive.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::page::{
    ///     HEADER_SIZE, Lsn, PageHeader, PageId, PageKind, PAGE_SIZE,
    /// };
    /// use pizzlydb_storage::pager::Pager;
    /// use pizzlydb_storage::vfs::{FileRole, MemoryVfs, OpenOptions, Vfs, VfsFile};
    /// use pizzlydb_storage::wal::Wal;
    ///
    /// let vfs = MemoryVfs::new();
    /// let mut main = vfs.open(
    ///     Path::new("db"),
    ///     FileRole::Main,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// let mut image = [0x11; PAGE_SIZE];
    /// PageHeader::new(PageKind::BTreeLeaf, PageId::new(0), Lsn::new(0))
    ///     .encode(&mut image)
    ///     .unwrap();
    ///
    /// main.write_all(0, &image).unwrap();
    ///
    /// let wal_file = vfs.open(
    ///     Path::new("db.wal"),
    ///     FileRole::Wal,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// let wal = Wal::create(wal_file, [7; 16]).unwrap();
    /// let pager = Pager::open(main, wal, 1).unwrap();
    /// let snapshot = pager.snapshot();
    /// let pin = pager.pin(PageId::new(0), snapshot.lsn()).unwrap();
    ///
    /// assert_eq!(pin.bytes()[HEADER_SIZE], 0x11);
    /// ```
    #[must_use = "dropping the view releases the byte borrow"]
    pub fn bytes(&self) -> Bytes<'_> {
        let borrowed = self.pager.frames[self.index].borrow();
        Bytes {
            inner: Ref::map(borrowed, |bytes| &bytes[..]),
        }
    }

    /// Mark the frame dirty and not durable.
    ///
    /// Eviction then skips the frame until [`Pager::commit_frame`] syncs a
    /// write-ahead-log commit. Snapshot resolution also skips the frame.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::page::{Lsn, PageHeader, PageId, PageKind, PAGE_SIZE};
    /// use pizzlydb_storage::pager::{Pager, PagerError};
    /// use pizzlydb_storage::vfs::{FileRole, MemoryVfs, OpenOptions, Vfs, VfsFile};
    /// use pizzlydb_storage::wal::Wal;
    ///
    /// let vfs = MemoryVfs::new();
    /// let mut main = vfs.open(
    ///     Path::new("db"),
    ///     FileRole::Main,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// let mut first = [0x11; PAGE_SIZE];
    /// PageHeader::new(PageKind::BTreeLeaf, PageId::new(0), Lsn::new(0))
    ///     .encode(&mut first)
    ///     .unwrap();
    ///
    /// main.write_all(0, &first).unwrap();
    ///
    /// let mut second = [0x22; PAGE_SIZE];
    /// PageHeader::new(PageKind::BTreeLeaf, PageId::new(1), Lsn::new(0))
    ///     .encode(&mut second)
    ///     .unwrap();
    ///
    /// main.write_all(u64::try_from(PAGE_SIZE).unwrap(), &second)
    ///     .unwrap();
    ///
    /// let wal_file = vfs.open(
    ///     Path::new("db.wal"),
    ///     FileRole::Wal,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// let wal = Wal::create(wal_file, [7; 16]).unwrap();
    /// let pager = Pager::open(main, wal, 1).unwrap();
    /// let snapshot = pager.snapshot();
    /// let pin = pager.pin(PageId::new(0), snapshot.lsn()).unwrap();
    /// pin.mark_dirty();
    /// drop(pin);
    ///
    /// let err = pager.pin(PageId::new(1), snapshot.lsn()).unwrap_err();
    /// assert!(matches!(err, PagerError::PoolExhausted));
    /// ```
    pub fn mark_dirty(&self) {
        let mut state = self.pager.state.borrow_mut();
        let frame = &mut state.meta[self.index];
        frame.dirty = true;
        frame.durable_main = false;
        frame.durable_wal = false;
    }

    /// Replace the frame image and mark it dirty.
    ///
    /// `image` must be one sealed page for this frame's page id. The pin count
    /// must be one. [`Pager::commit_frame`] assigns the commit LSN later and
    /// reseals the image.
    ///
    /// # Errors
    ///
    /// Returns [`PagerError::Shared`] when another pin or a [`Bytes`] guard
    /// holds the frame. Returns [`PagerError::ChecksumMismatch`] or
    /// [`PagerError::Corrupt`] when `image` is not a sealed page for this page
    /// id.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::page::{
    ///     HEADER_SIZE, Lsn, PageHeader, PageId, PageKind, PAGE_SIZE,
    /// };
    /// use pizzlydb_storage::pager::Pager;
    /// use pizzlydb_storage::vfs::{FileRole, MemoryVfs, OpenOptions, Vfs, VfsFile};
    /// use pizzlydb_storage::wal::Wal;
    ///
    /// let vfs = MemoryVfs::new();
    /// let mut main = vfs.open(
    ///     Path::new("db"),
    ///     FileRole::Main,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// let mut image = [0x11; PAGE_SIZE];
    /// PageHeader::new(PageKind::BTreeLeaf, PageId::new(0), Lsn::new(0))
    ///     .encode(&mut image)
    ///     .unwrap();
    ///
    /// main.write_all(0, &image).unwrap();
    ///
    /// let wal_file = vfs.open(
    ///     Path::new("db.wal"),
    ///     FileRole::Wal,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// let wal = Wal::create(wal_file, [7; 16]).unwrap();
    /// let pager = Pager::open(main, wal, 1).unwrap();
    /// let snapshot = pager.snapshot();
    /// let pin = pager.pin(PageId::new(0), snapshot.lsn()).unwrap();
    ///
    /// let mut replacement = [0x22; PAGE_SIZE];
    /// PageHeader::new(PageKind::BTreeLeaf, PageId::new(0), Lsn::new(0))
    ///     .encode(&mut replacement)
    ///     .unwrap();
    ///
    /// pin.replace(&replacement).unwrap();
    ///
    /// assert_eq!(pin.bytes()[HEADER_SIZE], 0x22);
    /// ```
    pub fn replace(&self, image: &[u8]) -> Result<(), PagerError> {
        let bytes = copy_page(image)?;
        let page_id = {
            let state = self.pager.state.borrow();
            let frame = &state.meta[self.index];
            if frame.pin_count != 1 {
                return Err(PagerError::Shared);
            }
            frame.page_id
        };

        let header = decode_page(&bytes, page_id)?;
        {
            let mut slot = self.pager.frames[self.index]
                .try_borrow_mut()
                .map_err(|_| PagerError::Shared)?;
            *slot = bytes;
        }

        let mut state = self.pager.state.borrow_mut();
        let frame = &mut state.meta[self.index];
        frame.lsn = header.lsn();
        frame.dirty = true;
        frame.durable_main = false;
        frame.durable_wal = false;
        frame.occupied = true;
        frame.referenced = true;
        Ok(())
    }

    /// Page id of the cached image.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::page::{Lsn, PageHeader, PageId, PageKind, PAGE_SIZE};
    /// use pizzlydb_storage::pager::Pager;
    /// use pizzlydb_storage::vfs::{FileRole, MemoryVfs, OpenOptions, Vfs, VfsFile};
    /// use pizzlydb_storage::wal::Wal;
    ///
    /// let vfs = MemoryVfs::new();
    /// let mut main = vfs.open(
    ///     Path::new("db"),
    ///     FileRole::Main,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// let mut image = [0x11; PAGE_SIZE];
    /// PageHeader::new(PageKind::BTreeLeaf, PageId::new(0), Lsn::new(0))
    ///     .encode(&mut image)
    ///     .unwrap();
    ///
    /// main.write_all(0, &image).unwrap();
    ///
    /// let wal_file = vfs.open(
    ///     Path::new("db.wal"),
    ///     FileRole::Wal,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// let wal = Wal::create(wal_file, [7; 16]).unwrap();
    /// let pager = Pager::open(main, wal, 1).unwrap();
    /// let snapshot = pager.snapshot();
    /// let pin = pager.pin(PageId::new(0), snapshot.lsn()).unwrap();
    ///
    /// assert_eq!(pin.page_id(), PageId::new(0));
    /// ```
    #[must_use]
    pub fn page_id(&self) -> PageId {
        self.pager.state.borrow().meta[self.index].page_id
    }

    /// LSN of the cached image.
    ///
    /// After [`Pager::commit_frame`], this is the LSN that commit recorded.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::page::{Lsn, PageHeader, PageId, PageKind, PAGE_SIZE};
    /// use pizzlydb_storage::pager::Pager;
    /// use pizzlydb_storage::vfs::{FileRole, MemoryVfs, OpenOptions, Vfs, VfsFile};
    /// use pizzlydb_storage::wal::Wal;
    ///
    /// let vfs = MemoryVfs::new();
    /// let mut main = vfs.open(
    ///     Path::new("db"),
    ///     FileRole::Main,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// let mut image = [0x11; PAGE_SIZE];
    /// PageHeader::new(PageKind::BTreeLeaf, PageId::new(0), Lsn::new(0))
    ///     .encode(&mut image)
    ///     .unwrap();
    ///
    /// main.write_all(0, &image).unwrap();
    ///
    /// let wal_file = vfs.open(
    ///     Path::new("db.wal"),
    ///     FileRole::Wal,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// let wal = Wal::create(wal_file, [7; 16]).unwrap();
    /// let pager = Pager::open(main, wal, 1).unwrap();
    /// let snapshot = pager.snapshot();
    /// let pin = pager.pin(PageId::new(0), snapshot.lsn()).unwrap();
    ///
    /// assert_eq!(pin.lsn(), Lsn::new(0));
    /// ```
    #[must_use]
    pub fn lsn(&self) -> Lsn {
        self.pager.state.borrow().meta[self.index].lsn
    }
}

impl<M: VfsFile, W: VfsFile> Drop for Pin<'_, M, W> {
    fn drop(&mut self) {
        let mut state = self.pager.state.borrow_mut();
        let frame = &mut state.meta[self.index];
        debug_assert!(frame.pin_count > 0);
        frame.pin_count = frame.pin_count.saturating_sub(1);
    }
}

/// Page bytes borrowed from a [`Pin`].
///
/// Dereferences to the cached image. The borrow ends when this value drops.
///
/// # Examples
///
/// ```ignore
/// use std::path::Path;
///
/// use pizzlydb_storage::page::{
///     HEADER_SIZE, Lsn, PageHeader, PageId, PageKind, PAGE_SIZE,
/// };
/// use pizzlydb_storage::pager::Pager;
/// use pizzlydb_storage::vfs::{FileRole, MemoryVfs, OpenOptions, Vfs, VfsFile};
/// use pizzlydb_storage::wal::Wal;
///
/// let vfs = MemoryVfs::new();
/// let mut main = vfs.open(
///     Path::new("db"),
///     FileRole::Main,
///     &OpenOptions::new().create(true),
/// ).unwrap();
///
/// let mut image = [0x11; PAGE_SIZE];
/// PageHeader::new(PageKind::BTreeLeaf, PageId::new(0), Lsn::new(0))
///     .encode(&mut image)
///     .unwrap();
///
/// main.write_all(0, &image).unwrap();
///
/// let wal_file = vfs.open(
///     Path::new("db.wal"),
///     FileRole::Wal,
///     &OpenOptions::new().create(true),
/// ).unwrap();
///
/// let wal = Wal::create(wal_file, [7; 16]).unwrap();
/// let pager = Pager::open(main, wal, 1).unwrap();
/// let snapshot = pager.snapshot();
/// let pin = pager.pin(PageId::new(0), snapshot.lsn()).unwrap();
/// let bytes = pin.bytes();
///
/// assert_eq!(bytes[HEADER_SIZE], 0x11);
/// ```
#[must_use = "the guard borrows the pinned image"]
pub struct Bytes<'a> {
    inner: Ref<'a, [u8]>,
}

impl Deref for Bytes<'_> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

/// Registration of one committed LSN.
///
/// [`Pager::snapshot_lsn`] is the oldest live registration. With no guards it
/// is one past the committed LSN. Drop removes this registration.
///
/// # Examples
///
/// ```ignore
/// use std::path::Path;
///
/// use pizzlydb_storage::page::Lsn;
/// use pizzlydb_storage::pager::Pager;
/// use pizzlydb_storage::vfs::{FileRole, MemoryVfs, OpenOptions, Vfs};
/// use pizzlydb_storage::wal::Wal;
///
/// let vfs = MemoryVfs::new();
/// let main = vfs.open(
///     Path::new("db"),
///     FileRole::Main,
///     &OpenOptions::new().create(true),
/// ).unwrap();
///
/// let wal_file = vfs.open(
///     Path::new("db.wal"),
///     FileRole::Wal,
///     &OpenOptions::new().create(true),
/// ).unwrap();
///
/// let wal = Wal::create(wal_file, [7; 16]).unwrap();
/// let pager = Pager::open(main, wal, 1).unwrap();
/// let snapshot = pager.snapshot();
///
/// assert_eq!(snapshot.lsn(), Lsn::new(0));
/// ```
#[must_use = "dropping the guard unregisters the snapshot"]
pub struct SnapshotGuard<'a, M: VfsFile, W: VfsFile> {
    pager: &'a Pager<M, W>,
    id: u64,
    lsn: Lsn,
}

impl<M: VfsFile, W: VfsFile> SnapshotGuard<'_, M, W> {
    /// LSN registered for this guard.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::page::Lsn;
    /// use pizzlydb_storage::pager::Pager;
    /// use pizzlydb_storage::vfs::{FileRole, MemoryVfs, OpenOptions, Vfs};
    /// use pizzlydb_storage::wal::Wal;
    ///
    /// let vfs = MemoryVfs::new();
    /// let main = vfs.open(
    ///     Path::new("db"),
    ///     FileRole::Main,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// let wal_file = vfs.open(
    ///     Path::new("db.wal"),
    ///     FileRole::Wal,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// let wal = Wal::create(wal_file, [7; 16]).unwrap();
    /// let pager = Pager::open(main, wal, 1).unwrap();
    ///
    /// assert_eq!(pager.snapshot().lsn(), Lsn::new(0));
    /// ```
    #[must_use]
    pub const fn lsn(&self) -> Lsn {
        self.lsn
    }
}

impl<M: VfsFile, W: VfsFile> Drop for SnapshotGuard<'_, M, W> {
    fn drop(&mut self) {
        self.pager
            .state
            .borrow_mut()
            .snapshots
            .retain(|(id, _)| *id != self.id);
    }
}

struct State<M: VfsFile, W: VfsFile> {
    main: M,
    wal: Wal<W>,
    meta: Vec<FrameMeta>,
    clock_hand: usize,
    snapshots: Vec<(u64, Lsn)>,
    next_snapshot_id: u64,
    next_txn_id: u64,
    committed_lsn: Lsn,
    stats: PagerStats,
}

impl<M: VfsFile, W: VfsFile> State<M, W> {
    fn registered(&self, lsn: Lsn) -> bool {
        self.snapshots
            .iter()
            .any(|(_, registered)| *registered == lsn)
    }

    fn find_cached(&self, page_id: PageId, snapshot: Lsn) -> Option<usize> {
        let floor = self.newest_wal_lsn(page_id, snapshot);
        let mut best: Option<(usize, Lsn)> = None;
        for (index, frame) in self.meta.iter().enumerate() {
            if !frame.occupied || frame.dirty || frame.page_id != page_id || frame.lsn > snapshot {
                continue;
            }

            if let Some(floor) = floor
                && frame.lsn < floor
            {
                continue;
            }

            let take = match best {
                Some((_, lsn)) => frame.lsn >= lsn,
                None => true,
            };

            if take {
                best = Some((index, frame.lsn));
            }
        }

        best.map(|(index, _)| index)
    }

    fn newest_wal_lsn(&self, page_id: PageId, snapshot: Lsn) -> Option<Lsn> {
        self.wal
            .pages()
            .iter()
            .filter(|page| page.page_id() == page_id && page.lsn() <= snapshot)
            .map(CommittedPage::lsn)
            .max()
    }

    fn wal_image(
        &self,
        page_id: PageId,
        snapshot: Lsn,
    ) -> Result<Option<[u8; PAGE_SIZE]>, PagerError> {
        let Some(page) = self
            .wal
            .pages()
            .iter()
            .filter(|page| page.page_id() == page_id && page.lsn() <= snapshot)
            .max_by_key(|page| page.lsn())
        else {
            return Ok(None);
        };

        Ok(Some(copy_page(page.image())?))
    }

    fn load_image(&mut self, page_id: PageId, snapshot: Lsn) -> Result<Loaded, PagerError> {
        if let Some(bytes) = self.wal_image(page_id, snapshot)? {
            let header = decode_page(&bytes, page_id)?;
            if header.lsn() > snapshot {
                return Err(PagerError::SnapshotNotFound);
            }

            return Ok(Loaded {
                bytes,
                header,
                durable_main: false,
                durable_wal: true,
            });
        }

        let offset = page_offset(page_id)?;
        let mut bytes = [0u8; PAGE_SIZE];
        self.main.read_exact(offset, &mut bytes)?;

        let header = decode_page(&bytes, page_id)?;
        if header.lsn() > snapshot {
            return Err(PagerError::SnapshotNotFound);
        }

        Ok(Loaded {
            bytes,
            header,
            durable_main: true,
            durable_wal: false,
        })
    }

    fn choose_victim(&mut self) -> Result<usize, PagerError> {
        let len = self.meta.len();
        if len == 0 {
            return Err(PagerError::PoolExhausted);
        }

        let steps = len.saturating_mul(2);
        for _ in 0..steps {
            let index = self.clock_hand;
            self.clock_hand = (self.clock_hand + 1) % len;
            let frame = &mut self.meta[index];

            if frame.pin_count > 0 {
                continue;
            }

            if frame.occupied && frame.dirty {
                continue;
            }

            if frame.occupied && !frame.durable_main && !frame.durable_wal {
                continue;
            }

            if frame.occupied && frame.referenced {
                frame.referenced = false;
                continue;
            }

            return Ok(index);
        }

        Err(PagerError::PoolExhausted)
    }
}

#[derive(Clone, Copy)]
struct FrameMeta {
    page_id: PageId,
    lsn: Lsn,
    pin_count: u32,
    referenced: bool,
    occupied: bool,
    dirty: bool,
    durable_main: bool,
    durable_wal: bool,
}

impl FrameMeta {
    const fn empty() -> Self {
        Self {
            page_id: PageId::new(0),
            lsn: Lsn::new(0),
            pin_count: 0,
            referenced: false,
            occupied: false,
            dirty: false,
            durable_main: false,
            durable_wal: false,
        }
    }
}

struct Loaded {
    bytes: [u8; PAGE_SIZE],
    header: PageHeader,
    durable_main: bool,
    durable_wal: bool,
}

fn install<M: VfsFile, W: VfsFile>(
    state: &mut State<M, W>,
    frames: &[RefCell<[u8; PAGE_SIZE]>],
    index: usize,
    loaded: &Loaded,
) {
    if state.meta[index].occupied {
        bump(&mut state.stats.evictions);
    }

    *frames[index].borrow_mut() = loaded.bytes;
    state.meta[index] = FrameMeta {
        page_id: loaded.header.page_id(),
        lsn: loaded.header.lsn(),
        pin_count: 1,
        referenced: true,
        occupied: true,
        dirty: false,
        durable_main: loaded.durable_main,
        durable_wal: loaded.durable_wal,
    };
}

fn note_hit<M: VfsFile, W: VfsFile>(
    state: &mut State<M, W>,
    index: usize,
) -> Result<(), PagerError> {
    let count = state.meta[index]
        .pin_count
        .checked_add(1)
        .ok_or(PagerError::Corrupt)?;

    state.meta[index].pin_count = count;
    state.meta[index].referenced = true;
    bump(&mut state.stats.hits);
    Ok(())
}

fn bump(counter: &mut u64) {
    *counter = counter.saturating_add(1);
}

fn next_lsn(lsn: Lsn) -> Lsn {
    Lsn::new(lsn.as_u64().saturating_add(1))
}

fn page_offset(page_id: PageId) -> Result<u64, PagerError> {
    let page_size = u64::try_from(PAGE_SIZE).map_err(|_| PagerError::Corrupt)?;
    page_id
        .as_u64()
        .checked_mul(page_size)
        .ok_or(PagerError::Corrupt)
}

fn decode_page(bytes: &[u8], page_id: PageId) -> Result<PageHeader, PagerError> {
    let header = match PageHeader::decode(bytes) {
        Ok(header) => header,
        Err(CodecError::ChecksumMismatch) => return Err(PagerError::ChecksumMismatch),
        Err(_) => return Err(PagerError::Corrupt),
    };

    if header.page_id() != page_id {
        return Err(PagerError::Corrupt);
    }

    Ok(header)
}

fn copy_page(bytes: &[u8]) -> Result<[u8; PAGE_SIZE], PagerError> {
    <[u8; PAGE_SIZE]>::try_from(bytes).map_err(|_| PagerError::Corrupt)
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::path::Path;
    use std::rc::Rc;

    use super::{Pager, PagerError};
    use crate::page::{HEADER_SIZE, Lsn, PAGE_SIZE, PageHeader, PageId, PageKind};
    use crate::vfs::{FileRole, OpenOptions, Vfs, VfsError, VfsFile, io_error};
    use crate::vfs_memory::{MemoryFile, MemoryVfs};
    use crate::wal::{Durability, Wal};

    const FILE_ID: [u8; 16] = [7; 16];

    struct RecordedWrite {
        offset: u64,
        bytes: Vec<u8>,
    }

    type WriteLog = Rc<RefCell<Vec<RecordedWrite>>>;

    struct Controls {
        fail_reads: Rc<Cell<u32>>,
        fail_syncs: Rc<Cell<u32>>,
        writes: WriteLog,
    }

    struct TestFile<F> {
        inner: F,
        fail_reads: Rc<Cell<u32>>,
        fail_syncs: Rc<Cell<u32>>,
        writes: WriteLog,
    }

    impl<F> TestFile<F> {
        fn new(inner: F) -> (Self, Controls) {
            let fail_reads = Rc::new(Cell::new(0));
            let fail_syncs = Rc::new(Cell::new(0));
            let writes = Rc::new(RefCell::new(Vec::new()));
            (
                Self {
                    inner,
                    fail_reads: Rc::clone(&fail_reads),
                    fail_syncs: Rc::clone(&fail_syncs),
                    writes: Rc::clone(&writes),
                },
                Controls {
                    fail_reads,
                    fail_syncs,
                    writes,
                },
            )
        }
    }

    impl<F: VfsFile> VfsFile for TestFile<F> {
        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, VfsError> {
            let left = self.fail_reads.get();
            if left > 0 {
                self.fail_reads.set(left - 1);
                return Err(io_error("injected read fault"));
            }
            self.inner.read_at(offset, buf)
        }

        fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<usize, VfsError> {
            let written = self.inner.write_at(offset, data)?;
            self.writes.borrow_mut().push(RecordedWrite {
                offset,
                bytes: data[..written].to_vec(),
            });
            Ok(written)
        }

        fn sync(&mut self) -> Result<(), VfsError> {
            let left = self.fail_syncs.get();
            if left > 0 {
                self.fail_syncs.set(left - 1);
                return Err(io_error("injected sync fault"));
            }
            self.inner.sync()
        }

        fn len(&self) -> Result<u64, VfsError> {
            self.inner.len()
        }

        fn close(self) -> Result<(), VfsError> {
            self.inner.close()
        }

        fn role(&self) -> FileRole {
            self.inner.role()
        }
    }

    fn sealed(page_id: u64, lsn: u64, fill: u8) -> [u8; PAGE_SIZE] {
        let mut image = [fill; PAGE_SIZE];
        PageHeader::new(PageKind::BTreeLeaf, PageId::new(page_id), Lsn::new(lsn))
            .encode(&mut image)
            .expect("encode page");
        image
    }

    fn body(bytes: &[u8]) -> u8 {
        bytes[HEADER_SIZE]
    }

    fn open_main(vfs: &MemoryVfs) -> MemoryFile {
        vfs.open(
            Path::new("db"),
            FileRole::Main,
            &OpenOptions::new().create(true),
        )
        .expect("open main")
    }

    fn empty_wal(vfs: &MemoryVfs) -> Wal<MemoryFile> {
        let file = vfs
            .open(
                Path::new("db.wal"),
                FileRole::Wal,
                &OpenOptions::new().create(true),
            )
            .expect("open wal");
        Wal::create(file, FILE_ID).expect("create wal")
    }

    fn write_pages(main: &mut MemoryFile, pages: &[(u64, u64, u8)]) {
        for &(id, lsn, fill) in pages {
            let image = sealed(id, lsn, fill);
            let offset = id * u64::try_from(PAGE_SIZE).expect("page size");
            main.write_all(offset, &image).expect("write page");
        }
    }

    fn pager_with(frames: usize, pages: &[(u64, u64, u8)]) -> Pager<MemoryFile, MemoryFile> {
        let vfs = MemoryVfs::new();
        let mut main = open_main(&vfs);
        write_pages(&mut main, pages);
        let wal = empty_wal(&vfs);
        Pager::open(main, wal, frames).expect("open pager")
    }

    fn must_fail() -> Result<(), PagerError> {
        Err(PagerError::Corrupt)
    }

    fn pin_then_fail(pager: &Pager<MemoryFile, MemoryFile>, lsn: Lsn) -> Result<(), PagerError> {
        let pin = pager.pin(PageId::new(0), lsn)?;
        let _ = pin.page_id();
        must_fail()?;
        Ok(())
    }

    #[test]
    fn zero_frames_is_pool_exhausted() {
        let vfs = MemoryVfs::new();
        let main = open_main(&vfs);
        let wal = empty_wal(&vfs);
        assert!(matches!(
            Pager::open(main, wal, 0),
            Err(PagerError::PoolExhausted)
        ));
    }

    #[test]
    fn pin_counts_hits_and_misses() {
        let pager = pager_with(2, &[(0, 0, 0x11), (1, 0, 0x22)]);
        let snapshot = pager.snapshot();
        let lsn = snapshot.lsn();

        let first = pager.pin(PageId::new(0), lsn).expect("miss");
        assert_eq!(body(&first.bytes()), 0x11);
        drop(first);

        let again = pager.pin(PageId::new(0), lsn).expect("hit");
        assert_eq!(pager.stats().hits(), 1);
        assert_eq!(pager.stats().misses(), 1);
        assert_eq!(pager.stats().read_io(), 1);
        drop(again);

        let second = pager.pin(PageId::new(1), lsn).expect("second miss");
        assert_eq!(body(&second.bytes()), 0x22);
        assert_eq!(pager.stats().misses(), 2);
        assert_eq!(pager.stats().read_io(), 2);
        assert_eq!(pager.stats().evictions(), 0);
        assert_eq!(
            pager.commit_frame(&second).expect("clean frame"),
            second.lsn()
        );
    }

    #[test]
    fn one_frame_pool_rejects_a_second_page_until_the_pin_drops() {
        let pager = pager_with(1, &[(0, 0, 0x11), (1, 0, 0x22)]);
        let snapshot = pager.snapshot();
        let lsn = snapshot.lsn();
        let held = pager.pin(PageId::new(0), lsn).expect("pin");

        assert!(matches!(
            pager.pin(PageId::new(1), lsn),
            Err(PagerError::PoolExhausted)
        ));
        assert_eq!(pager.stats().misses(), 1);
        assert_eq!(body(&held.bytes()), 0x11);
        drop(held);

        let next = pager.pin(PageId::new(1), lsn).expect("evict");
        assert_eq!(body(&next.bytes()), 0x22);
        assert!(pager.stats().evictions() >= 1);
    }

    #[test]
    fn all_frames_pinned_returns_pool_exhausted() {
        let pager = pager_with(2, &[(0, 0, 0x11), (1, 0, 0x22), (2, 0, 0x33)]);
        let snapshot = pager.snapshot();
        let lsn = snapshot.lsn();
        let first = pager.pin(PageId::new(0), lsn).expect("page 0");
        let second = pager.pin(PageId::new(1), lsn).expect("page 1");

        assert!(matches!(
            pager.pin(PageId::new(2), lsn),
            Err(PagerError::PoolExhausted)
        ));
        assert_eq!(body(&first.bytes()), 0x11);
        assert_eq!(body(&second.bytes()), 0x22);
    }

    #[test]
    fn error_return_unpins() {
        let pager = pager_with(1, &[(0, 0, 0x11), (1, 0, 0x22)]);
        let snapshot = pager.snapshot();
        pin_then_fail(&pager, snapshot.lsn()).expect_err("forced error");

        let pin = pager
            .pin(PageId::new(1), snapshot.lsn())
            .expect("frame was released");
        assert_eq!(body(&pin.bytes()), 0x22);
    }

    #[test]
    fn dirty_unpinned_frame_is_not_evicted() {
        let pager = pager_with(1, &[(0, 0, 0x11), (1, 0, 0x22)]);
        let snapshot = pager.snapshot();
        let lsn = snapshot.lsn();
        let pin = pager.pin(PageId::new(0), lsn).expect("pin");
        pin.mark_dirty();
        drop(pin);

        assert!(matches!(
            pager.pin(PageId::new(0), lsn),
            Err(PagerError::PoolExhausted)
        ));
        assert!(matches!(
            pager.pin(PageId::new(1), lsn),
            Err(PagerError::PoolExhausted)
        ));

        let pin = pager.pin_dirty(PageId::new(0)).expect("dirty frame");
        assert_eq!(pager.commit_frame(&pin).expect("commit"), Lsn::new(1));
        drop(pin);

        let next = pager.pin(PageId::new(1), lsn).expect("evict after commit");
        assert_eq!(body(&next.bytes()), 0x22);
        assert!(pager.stats().evictions() >= 1);
    }

    #[test]
    fn sync_failure_keeps_the_dirty_bit() {
        let vfs = MemoryVfs::new();
        let mut inner = open_main(&vfs);
        write_pages(&mut inner, &[(0, 0, 0x11), (1, 0, 0x22)]);
        let (wal_inner, controls) = TestFile::new(
            vfs.open(
                Path::new("db.wal"),
                FileRole::Wal,
                &OpenOptions::new().create(true),
            )
            .expect("open wal"),
        );
        let wal = Wal::create(wal_inner, FILE_ID).expect("create wal");
        controls.fail_syncs.set(1);
        let pager = Pager::open(inner, wal, 1).expect("open pager");
        let snapshot = pager.snapshot();
        let lsn = snapshot.lsn();
        let pin = pager.pin(PageId::new(0), lsn).expect("pin");
        pin.mark_dirty();

        let err = pager.commit_frame(&pin).expect_err("sync fails");
        assert!(matches!(err, PagerError::Wal(_)));
        assert_eq!(pin.lsn(), Lsn::new(0));
        assert_eq!(body(&pin.bytes()), 0x11);
        drop(pin);

        let later = pager.snapshot();
        assert_eq!(later.lsn(), Lsn::new(0));
        drop(later);
        assert!(matches!(
            pager.pin(PageId::new(1), lsn),
            Err(PagerError::PoolExhausted)
        ));

        let pin = pager.pin_dirty(PageId::new(0)).expect("still dirty");
        assert_eq!(pager.commit_frame(&pin).expect("sync"), Lsn::new(1));
        drop(pin);
        let next = pager.pin(PageId::new(1), lsn).expect("evict");
        assert_eq!(body(&next.bytes()), 0x22);
    }

    #[test]
    fn snapshot_does_not_observe_a_later_image() {
        let vfs = MemoryVfs::new();
        let mut main = open_main(&vfs);
        let image_a = sealed(4, 1, 0x11);
        let image_b = sealed(4, 1, 0x22);
        let offset = 4 * u64::try_from(PAGE_SIZE).expect("page size");
        main.write_all(offset, &image_a).expect("write main");

        let mut wal = empty_wal(&vfs);
        wal.append_page_image(1, PageId::new(4), Lsn::new(1), &image_a)
            .expect("append");
        wal.commit(Durability::Full, 1, Lsn::new(1), 1)
            .expect("commit");

        let pager = Pager::open(main, wal, 2).expect("open pager");
        assert_eq!(pager.snapshot_lsn(), Lsn::new(2));
        let old = pager.snapshot();
        assert_eq!(old.lsn(), Lsn::new(1));
        assert_eq!(pager.snapshot_lsn(), Lsn::new(1));

        let writer = pager.pin(PageId::new(4), old.lsn()).expect("writer");
        assert_eq!(body(&writer.bytes()), 0x11);
        assert_eq!(pager.stats().read_io(), 0);
        writer.replace(&image_b).expect("replace");
        assert_eq!(body(&writer.bytes()), 0x22);
        assert_eq!(pager.commit_frame(&writer).expect("publish"), Lsn::new(2));
        assert_eq!(writer.lsn(), Lsn::new(2));
        assert_eq!(body(&writer.bytes()), 0x22);
        drop(writer);

        let newer = pager.snapshot();
        assert_eq!(newer.lsn(), Lsn::new(2));
        assert_eq!(pager.snapshot_lsn(), Lsn::new(1));

        let seen_old = pager.pin(PageId::new(4), old.lsn()).expect("old image");
        let seen_new = pager.pin(PageId::new(4), newer.lsn()).expect("new image");
        assert_eq!(body(&seen_old.bytes()), 0x11);
        assert_eq!(body(&seen_new.bytes()), 0x22);
        assert_eq!(seen_old.lsn(), Lsn::new(1));
        assert_eq!(seen_new.lsn(), Lsn::new(2));
        assert_eq!(pager.stats().read_io(), 0);

        drop(old);
        assert_eq!(pager.snapshot_lsn(), Lsn::new(2));
        drop(newer);
        assert_eq!(pager.snapshot_lsn(), Lsn::new(3));
        assert!(matches!(
            pager.pin(PageId::new(4), Lsn::new(1)),
            Err(PagerError::SnapshotNotFound)
        ));
    }

    #[test]
    fn snapshot_pin_skips_a_dirty_frame() {
        let pager = pager_with(2, &[(0, 0, 0x11)]);
        let snapshot = pager.snapshot();
        let lsn = snapshot.lsn();
        let writer = pager.pin(PageId::new(0), lsn).expect("writer");
        writer.replace(&sealed(0, 0, 0x22)).expect("replace");

        let reader = pager.pin(PageId::new(0), lsn).expect("reader");
        assert_eq!(body(&writer.bytes()), 0x22);
        assert_eq!(body(&reader.bytes()), 0x11);
        assert_eq!(reader.lsn(), Lsn::new(0));
    }

    #[test]
    fn checksum_mismatch_is_not_cached() {
        let vfs = MemoryVfs::new();
        let mut main = open_main(&vfs);
        let mut bad = sealed(0, 0, 0x11);
        bad[HEADER_SIZE] ^= 0x01;
        main.write_all(0, &bad).expect("write bad page");
        let good = sealed(1, 0, 0x22);
        let offset = u64::try_from(PAGE_SIZE).expect("page size");
        main.write_all(offset, &good).expect("write good page");
        let pager = Pager::open(main, empty_wal(&vfs), 1).expect("open");
        let snapshot = pager.snapshot();
        let lsn = snapshot.lsn();

        assert!(matches!(
            pager.pin(PageId::new(0), lsn),
            Err(PagerError::ChecksumMismatch)
        ));
        assert!(matches!(
            pager.pin(PageId::new(0), lsn),
            Err(PagerError::ChecksumMismatch)
        ));
        assert_eq!(pager.stats().read_io(), 0);
        assert_eq!(pager.stats().misses(), 0);
        assert_eq!(pager.stats().hits(), 0);

        let pin = pager.pin(PageId::new(1), lsn).expect("good page");
        assert_eq!(body(&pin.bytes()), 0x22);
        assert_eq!(pager.stats().misses(), 1);
        assert_eq!(pager.stats().read_io(), 1);
    }

    #[test]
    fn read_failure_leaves_no_cache_entry() {
        let vfs = MemoryVfs::new();
        let mut inner = open_main(&vfs);
        let image = sealed(0, 0, 0x11);
        inner.write_all(0, &image).expect("write");
        let (main, controls) = TestFile::new(inner);
        controls.fail_reads.set(1);
        let pager = Pager::open(main, empty_wal(&vfs), 1).expect("open");
        let snapshot = pager.snapshot();

        assert!(matches!(
            pager.pin(PageId::new(0), snapshot.lsn()),
            Err(PagerError::Io(_))
        ));

        let pin = pager.pin(PageId::new(0), snapshot.lsn()).expect("retry");
        assert_eq!(body(&pin.bytes()), 0x11);
        assert_eq!(pager.stats().hits(), 0);
        assert_eq!(pager.stats().misses(), 1);
        assert_eq!(pager.stats().read_io(), 1);
    }

    #[test]
    fn write_main_records_the_image_and_leaves_a_dirty_frame() {
        let vfs = MemoryVfs::new();
        let mut inner = open_main(&vfs);
        write_pages(&mut inner, &[(0, 0, 0x11), (1, 0, 0x22)]);
        let (main, controls) = TestFile::new(inner);
        let pager = Pager::open(main, empty_wal(&vfs), 1).expect("open");
        let snapshot = pager.snapshot();
        let pin = pager.pin(PageId::new(0), snapshot.lsn()).expect("pin");
        pager.write_main(&pin).expect("write main");

        assert_eq!(pager.stats().write_io(), 1);
        let recorded = controls.writes.borrow()[0].bytes.clone();
        assert_eq!(controls.writes.borrow()[0].offset, 0);
        assert_eq!(recorded.as_slice(), &pin.bytes()[..]);
        pin.mark_dirty();
        drop(pin);

        assert!(matches!(
            pager.pin(PageId::new(1), snapshot.lsn()),
            Err(PagerError::PoolExhausted)
        ));
    }

    #[test]
    fn shared_frame_rejects_mutation() {
        let pager = pager_with(1, &[(0, 0, 0x11)]);
        let snapshot = pager.snapshot();
        let lsn = snapshot.lsn();
        let first = pager.pin(PageId::new(0), lsn).expect("first");
        let second = pager.pin(PageId::new(0), lsn).expect("second");

        assert!(matches!(
            first.replace(&sealed(0, 0, 0x22)),
            Err(PagerError::Shared)
        ));
        assert!(matches!(
            pager.commit_frame(&first),
            Err(PagerError::Shared)
        ));
        assert_eq!(body(&first.bytes()), 0x11);
        assert_eq!(body(&second.bytes()), 0x11);
    }

    #[test]
    fn unregistered_lsn_is_snapshot_not_found() {
        let pager = pager_with(1, &[(0, 0, 0x11)]);
        assert_eq!(pager.snapshot_lsn(), Lsn::new(1));
        assert!(matches!(
            pager.pin(PageId::new(0), Lsn::new(0)),
            Err(PagerError::SnapshotNotFound)
        ));
    }

    #[test]
    fn main_image_newer_than_snapshot_is_not_admitted() {
        let pager = pager_with(1, &[(0, 5, 0x11)]);
        let snapshot = pager.snapshot();
        assert_eq!(snapshot.lsn(), Lsn::new(0));
        assert!(matches!(
            pager.pin(PageId::new(0), snapshot.lsn()),
            Err(PagerError::SnapshotNotFound)
        ));
        assert_eq!(pager.stats().read_io(), 0);
        assert_eq!(pager.stats().misses(), 0);
        assert!(matches!(
            pager.pin(PageId::new(0), snapshot.lsn()),
            Err(PagerError::SnapshotNotFound)
        ));
    }

    #[test]
    fn page_offset_overflow_is_corrupt() {
        let pager = pager_with(1, &[]);
        let snapshot = pager.snapshot();
        assert!(matches!(
            pager.pin(PageId::new(u64::MAX), snapshot.lsn()),
            Err(PagerError::Corrupt)
        ));
    }

    #[test]
    fn header_page_id_must_match_the_request() {
        let vfs = MemoryVfs::new();
        let mut main = open_main(&vfs);
        main.write_all(0, &sealed(1, 0, 0x11)).expect("write");
        let pager = Pager::open(main, empty_wal(&vfs), 1).expect("open");
        let snapshot = pager.snapshot();
        assert!(matches!(
            pager.pin(PageId::new(0), snapshot.lsn()),
            Err(PagerError::Corrupt)
        ));
        assert_eq!(pager.stats().misses(), 0);
    }

    #[test]
    fn pin_dirty_requires_a_dirty_frame() {
        let pager = pager_with(1, &[(0, 0, 0x11)]);
        assert!(matches!(
            pager.pin_dirty(PageId::new(0)),
            Err(PagerError::NotDirty)
        ));
    }
}
