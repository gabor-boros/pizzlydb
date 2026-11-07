//! Extent bitmap, deferred retire list, and free-space hints.
//!
//! The bitmap is the authority for which page ids are allocated. A hint never
//! sets a bit. [`Space::reclaim`] takes the horizon as an argument and does
//! not read snapshot registrations. The caller passes the oldest snapshot
//! LSN, or one past the durable LSN when no snapshot is registered.
//!
//! Pages 0 and 1 are the superblock copies. [`ALLOCATOR_PAGE_ID`] is the
//! allocator header. [`BITMAP_PAGE_ID`] is the allocation bitmap.
//! [`FIRST_DATA_PAGE_ID`] is the first id [`Space::allocate`] returns. Those
//! ids stay allocated for the life of the file.
//!
//! A new file covers [`INITIAL_PAGES`] ids. When no clear bit remains below
//! the high-water mark, the mark advances by [`EXTENT_PAGES`]. It does not
//! advance when another extent would pass [`MAX_BITMAP_PAGES`].
//!
//! [`Space::retire`] records a page id and an LSN. The bit stays set.
//! [`Space::reclaim`] clears that bit only when the retire LSN is strictly
//! below the horizon. The header holds [`MAX_RETIRE`] entries. Further
//! entries spill to [`PageKind::RetireSpill`] pages. `retire` and `reclaim`
//! publish the header, the bitmap, and the spill chain together. A failure
//! leaves the previous chain in place.
//!
//! A hint stores `floor(log2(free_bytes))`, or class 0 when `free_bytes` is 0.
//! A full hint table replaces the first entry. [`Space::page_with_space`]
//! returns a hint only when its bit is set, the page is not retired, the page
//! is not a spill page, and the caller's free-byte count is at least the
//! request.
//!
//! The caller serializes [`Space::allocate`], [`Space::retire`],
//! [`Space::reclaim`], and [`Space::record_free_space`].
//!
//! Allocator header body, little-endian, after [`HEADER_SIZE`]:
//!
//! - high-water mark `u64`
//! - bitmap page id `u64`
//! - retire count `u16`
//! - hint count `u16`
//! - 4 zero bytes
//! - [`MAX_RETIRE`] entries of page id `u64` and LSN `u64`
//! - hint entries of page id `u64`, class `u8`, and 7 zero bytes
//! - zero bytes through the spill reserve
//! - spill-chain head `u64` at [`PAGE_SIZE`] - 16, or zero when the chain is empty
//! - 8 zero bytes
//!
//! Bitmap body: one bit per page id. Bit 0 is the low bit of the first body
//! byte. Bits at and past the high-water mark are zero. Bits 0 through 3 are
//! set.
//!
//! Spill body, after [`HEADER_SIZE`]:
//!
//! - next page id `u64`, or zero at the end of the chain
//! - entry count `u16`
//! - 6 zero bytes
//! - entries of page id `u64` and LSN `u64`
//!
//! Unused entry slots are zero. The page-header LSN of these images is zero.
//! The retire LSN is the body field.

use std::mem;

use thiserror::Error;

use crate::codec::{get_u16, get_u64, put_u16, put_u64};
use crate::page::{
    ALLOCATOR_PAGE_ID, BITMAP_PAGE_ID, FIRST_DATA_PAGE_ID, HEADER_SIZE, Lsn, PAGE_SIZE, PageHeader,
    PageId, PageKind,
};

/// Page ids covered by a new bitmap.
pub const INITIAL_PAGES: u64 = 64;

/// Page ids added when the high-water mark advances.
pub const EXTENT_PAGES: u64 = 64;

/// Retire entries stored in the allocator header.
pub const MAX_RETIRE: usize = 64;

const BITMAP_BYTES: usize = PAGE_SIZE - HEADER_SIZE;
const BITMAP_BITS: usize = BITMAP_BYTES * 8;

/// Page ids addressable by the bitmap page.
pub const MAX_BITMAP_PAGES: u64 = 65_312;

const HIGH_WATER_OFFSET: usize = HEADER_SIZE;
const BITMAP_ID_OFFSET: usize = HIGH_WATER_OFFSET + 8;
const RETIRE_COUNT_OFFSET: usize = BITMAP_ID_OFFSET + 8;
const HINT_COUNT_OFFSET: usize = RETIRE_COUNT_OFFSET + 2;
const HEADER_RESERVED_OFFSET: usize = HINT_COUNT_OFFSET + 2;
const HEADER_RESERVED_LEN: usize = 4;
const RETIRE_LIST_OFFSET: usize = HEADER_RESERVED_OFFSET + HEADER_RESERVED_LEN;
const RETIRE_ENTRY_LEN: usize = 16;
const HINT_LIST_OFFSET: usize = RETIRE_LIST_OFFSET + MAX_RETIRE * RETIRE_ENTRY_LEN;
const HINT_ENTRY_LEN: usize = 16;
const SPILL_RESERVE: usize = 16;
const SPILL_HEAD_OFFSET: usize = PAGE_SIZE - SPILL_RESERVE;

const MAX_HINTS: usize = (SPILL_HEAD_OFFSET - HINT_LIST_OFFSET) / HINT_ENTRY_LEN;

const SPILL_NEXT_OFFSET: usize = HEADER_SIZE;
const SPILL_COUNT_OFFSET: usize = SPILL_NEXT_OFFSET + 8;
const SPILL_RESERVED_OFFSET: usize = SPILL_COUNT_OFFSET + 2;
const SPILL_RESERVED_LEN: usize = 6;
const SPILL_LIST_OFFSET: usize = SPILL_RESERVED_OFFSET + SPILL_RESERVED_LEN;
const SPILL_ENTRY_LEN: usize = 16;
const SPILL_CAPACITY: usize = (PAGE_SIZE - SPILL_LIST_OFFSET) / SPILL_ENTRY_LEN;

const MAX_FREE_CLASS: u8 = 13;

const _: () = assert!(BITMAP_BITS == 65_312);
const _: () = assert!(PAGE_SIZE.ilog2() == 13);
const _: () = assert!(INITIAL_PAGES >= FIRST_DATA_PAGE_ID.as_u64());
const _: () = assert!(INITIAL_PAGES.is_multiple_of(EXTENT_PAGES));
const _: () = assert!(MAX_RETIRE > 0);
const _: () = assert!(MAX_RETIRE <= u16::MAX as usize);
const _: () = assert!(SPILL_CAPACITY > 0);
const _: () = assert!(SPILL_CAPACITY <= u16::MAX as usize);
const _: () = assert!(MAX_HINTS > 0);
const _: () = assert!(MAX_HINTS <= u16::MAX as usize);
const _: () = assert!(HINT_LIST_OFFSET + MAX_HINTS * HINT_ENTRY_LEN <= SPILL_HEAD_OFFSET);
const _: () = assert!(SPILL_LIST_OFFSET + SPILL_CAPACITY * SPILL_ENTRY_LEN <= PAGE_SIZE);
const _: () = assert!(SPILL_HEAD_OFFSET + 8 == PAGE_SIZE - 8);

/// Failure while allocating, retiring, reclaiming, or loading allocator pages.
///
/// # Examples
///
/// ```ignore
/// use pizzlydb_storage::page::HEADER_SIZE;
/// use pizzlydb_storage::space::{Space, SpaceError};
///
/// let mut encoded = Space::create().encode();
/// encoded.header[HEADER_SIZE] ^= 0xff;
///
/// let err = Space::load(&encoded.header, &encoded.bitmap, &encoded.spills).unwrap_err();
/// assert_eq!(err, SpaceError::Corrupt);
/// ```
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum SpaceError {
    /// No clear page id remains, and the high-water mark cannot advance.
    #[error("no free page")]
    Full,
    /// A page image or allocator field breaks an invariant.
    ///
    /// This includes a checksum mismatch, a reserved id, a duplicate retire
    /// entry, and a request to set a bit that is already set.
    #[error("corrupt allocator")]
    Corrupt,
    /// The bitmap bit is already clear.
    #[error("page is already free")]
    DoubleFree,
}

/// Allocator header, bitmap, and spill images.
///
/// # Examples
///
/// ```ignore
/// use pizzlydb_storage::page::{Lsn, PageKind};
/// use pizzlydb_storage::space::Space;
///
/// let mut space = Space::create();
/// let page = space.allocate(PageKind::BTreeLeaf).unwrap();
/// space.retire(page, Lsn::new(4)).unwrap();
///
/// let encoded = space.encode();
/// let loaded = Space::load(&encoded.header, &encoded.bitmap, &encoded.spills).unwrap();
///
/// assert!(loaded.is_allocated(page));
/// assert!(loaded.is_retired(page));
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedSpace {
    /// Sealed allocator header page.
    pub header: [u8; PAGE_SIZE],
    /// Sealed bitmap page.
    pub bitmap: [u8; PAGE_SIZE],
    /// Spill pages in chain order, head first.
    pub spills: Vec<SpillImage>,
}

/// One sealed retire-spill page.
///
/// # Examples
///
/// ```ignore
/// use pizzlydb_storage::page::PageKind;
/// use pizzlydb_storage::space::Space;
///
/// let mut space = Space::create();
/// for _ in 0..=64 {
///     let page = space.allocate(PageKind::BTreeLeaf).unwrap();
///     space.retire(page, pizzlydb_storage::page::Lsn::new(1)).unwrap();
/// }
///
/// let encoded = space.encode();
/// assert_eq!(encoded.spills.len(), 1);
/// assert_eq!(encoded.spills[0].page_id, space.spill_head().unwrap());
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpillImage {
    /// Page id stored in the image header.
    pub page_id: PageId,
    /// Sealed spill page.
    pub bytes: [u8; PAGE_SIZE],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RetireEntry {
    page_id: PageId,
    lsn: Lsn,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Hint {
    page_id: PageId,
    class: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SpillPage {
    page_id: PageId,
    next: Option<PageId>,
    entries: Vec<RetireEntry>,
}

/// Bitmap, high-water mark, retire list, and free-space hints.
///
/// # Examples
///
/// ```ignore
/// use pizzlydb_storage::page::{PageId, PageKind};
/// use pizzlydb_storage::space::{Space, INITIAL_PAGES};
///
/// let mut space = Space::create();
/// let page = space.allocate(PageKind::BTreeLeaf).unwrap();
///
/// assert_eq!(page, PageId::new(4));
/// assert_eq!(space.high_water(), INITIAL_PAGES);
/// assert!(space.is_allocated(page));
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Space {
    high_water: u64,
    bitmap: Vec<u8>,
    retires: Vec<RetireEntry>,
    spills: Vec<SpillPage>,
    hints: Vec<Hint>,
}

impl Space {
    /// Bitmap with [`INITIAL_PAGES`] ids and the reserved pages allocated.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use pizzlydb_storage::page::{
    ///     ALLOCATOR_PAGE_ID, BITMAP_PAGE_ID, PageId,
    /// };
    /// use pizzlydb_storage::space::Space;
    ///
    /// let space = Space::create();
    ///
    /// assert!(space.is_allocated(PageId::new(0)));
    /// assert!(space.is_allocated(PageId::new(1)));
    /// assert!(space.is_allocated(ALLOCATOR_PAGE_ID));
    /// assert!(space.is_allocated(BITMAP_PAGE_ID));
    /// ```
    #[must_use]
    pub fn create() -> Self {
        let mut space = Self {
            high_water: INITIAL_PAGES,
            bitmap: vec![0; BITMAP_BYTES],
            retires: Vec::new(),
            spills: Vec::new(),
            hints: Vec::new(),
        };

        for id in 0..FIRST_DATA_PAGE_ID.as_u64() {
            space
                .write_bit(id, true)
                .expect("reserved id fits in the bitmap");
        }

        space
    }

    /// High-water mark. Ids below it have a bitmap bit.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use pizzlydb_storage::space::{Space, INITIAL_PAGES};
    ///
    /// assert_eq!(Space::create().high_water(), INITIAL_PAGES);
    /// ```
    #[must_use]
    pub const fn high_water(&self) -> u64 {
        self.high_water
    }

    /// Whether the bitmap bit for `page_id` is set.
    ///
    /// An id that does not fit in the bitmap is not allocated.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use pizzlydb_storage::page::{PageId, PageKind};
    /// use pizzlydb_storage::space::Space;
    ///
    /// let mut space = Space::create();
    /// let page = space.allocate(PageKind::BTreeLeaf).unwrap();
    ///
    /// assert!(space.is_allocated(page));
    /// assert!(!space.is_allocated(PageId::new(5)));
    /// ```
    #[must_use]
    pub fn is_allocated(&self, page_id: PageId) -> bool {
        self.bit(page_id.as_u64()).unwrap_or(false)
    }

    /// Whether `page_id` is on the header retire list or a spill page.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use pizzlydb_storage::page::{Lsn, PageKind};
    /// use pizzlydb_storage::space::Space;
    ///
    /// let mut space = Space::create();
    /// let page = space.allocate(PageKind::BTreeLeaf).unwrap();
    /// space.retire(page, Lsn::new(2)).unwrap();
    ///
    /// assert!(space.is_retired(page));
    /// assert!(space.is_allocated(page));
    /// ```
    #[must_use]
    pub fn is_retired(&self, page_id: PageId) -> bool {
        self.retires.iter().any(|entry| entry.page_id == page_id)
            || self
                .spills
                .iter()
                .any(|spill| spill.entries.iter().any(|entry| entry.page_id == page_id))
    }

    /// Retire entries stored in the header, not on spill pages.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use pizzlydb_storage::page::{Lsn, PageKind};
    /// use pizzlydb_storage::space::Space;
    ///
    /// let mut space = Space::create();
    /// let page = space.allocate(PageKind::BTreeLeaf).unwrap();
    /// space.retire(page, Lsn::new(2)).unwrap();
    ///
    /// assert_eq!(space.header_retire_count(), 1);
    /// ```
    #[must_use]
    pub fn header_retire_count(&self) -> usize {
        self.retires.len()
    }

    /// First spill page, or [`None`] when every retire entry fits in the header.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use pizzlydb_storage::page::{Lsn, PageKind};
    /// use pizzlydb_storage::space::Space;
    ///
    /// let mut space = Space::create();
    /// let page = space.allocate(PageKind::BTreeLeaf).unwrap();
    /// space.retire(page, Lsn::new(2)).unwrap();
    ///
    /// assert_eq!(space.spill_head(), None);
    /// ```
    #[must_use]
    pub fn spill_head(&self) -> Option<PageId> {
        self.spills.first().map(|spill| spill.page_id)
    }

    /// Set the first clear data bit and return its page id.
    ///
    /// `Superblock`, `Allocator`, and `Bitmap` are reserved kinds. The check
    /// and the bit update happen in this call. A set bit is not returned.
    /// `kind` is not written to the bitmap. [`PageKind::RetireSpill`] reserves
    /// an ordinary id; spill pages that hold retire entries are created by
    /// [`Self::retire`].
    ///
    /// # Errors
    ///
    /// - [`SpaceError::Corrupt`] for a reserved kind, or when the first clear
    ///   id in a new extent is already set.
    /// - [`SpaceError::Full`] when no clear id remains and the high-water mark
    ///   cannot advance by [`EXTENT_PAGES`].
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use pizzlydb_storage::page::{PageId, PageKind};
    /// use pizzlydb_storage::space::{Space, SpaceError};
    ///
    /// let mut space = Space::create();
    /// let first = space.allocate(PageKind::BTreeLeaf).unwrap();
    /// let second = space.allocate(PageKind::BTreeLeaf).unwrap();
    ///
    /// assert_eq!(first, PageId::new(4));
    /// assert_ne!(first, second);
    /// assert_eq!(
    ///     space.allocate(PageKind::Allocator).unwrap_err(),
    ///     SpaceError::Corrupt,
    /// );
    /// ```
    pub fn allocate(&mut self, kind: PageKind) -> Result<PageId, SpaceError> {
        if matches!(
            kind,
            PageKind::Superblock | PageKind::Allocator | PageKind::Bitmap
        ) {
            return Err(SpaceError::Corrupt);
        }

        Ok(PageId::new(self.take_free()?))
    }

    /// Queue `page_id` for reuse after a horizon strictly above `lsn`.
    ///
    /// The bitmap bit stays set. The update is published only after the
    /// header entry, or the spill page it requires, is complete.
    ///
    /// # Errors
    ///
    /// - [`SpaceError::DoubleFree`] when the bit is clear.
    /// - [`SpaceError::Corrupt`] for a reserved id, an id at or past the
    ///   high-water mark, a spill page, `lsn == 0`, or a page already retired.
    /// - [`SpaceError::Full`] when the header is full and no page id is left
    ///   for a spill page. The published header and bitmap stay as they were.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use pizzlydb_storage::page::{Lsn, PageKind};
    /// use pizzlydb_storage::space::Space;
    ///
    /// let mut space = Space::create();
    /// let page = space.allocate(PageKind::BTreeLeaf).unwrap();
    /// space.retire(page, Lsn::new(6)).unwrap();
    ///
    /// assert!(space.is_allocated(page));
    /// assert!(space.is_retired(page));
    /// ```
    pub fn retire(&mut self, page_id: PageId, lsn: Lsn) -> Result<(), SpaceError> {
        let mut updated = self.clone();
        updated.retire_in_place(page_id, lsn)?;
        *self = updated;
        Ok(())
    }

    /// Clear retire bits whose LSN is strictly below `horizon`.
    ///
    /// Entries at or above `horizon` stay queued, in their previous order.
    /// An empty spill page is unlinked and its bit is cleared. Reserved bits
    /// are left set. A failure publishes none of the cleared bits.
    ///
    /// # Errors
    ///
    /// Returns [`SpaceError::DoubleFree`] when a queued page bit is already
    /// clear, and [`SpaceError::Corrupt`] when a queued id is reserved.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use pizzlydb_storage::page::{Lsn, PageKind};
    /// use pizzlydb_storage::space::Space;
    ///
    /// let mut space = Space::create();
    /// let page = space.allocate(PageKind::BTreeLeaf).unwrap();
    /// space.retire(page, Lsn::new(11)).unwrap();
    /// space.reclaim(Lsn::new(11)).unwrap();
    ///
    /// assert!(space.is_allocated(page));
    ///
    /// space.reclaim(Lsn::new(12)).unwrap();
    /// let reused = space.allocate(PageKind::BTreeLeaf).unwrap();
    ///
    /// assert_eq!(reused, page);
    /// ```
    pub fn reclaim(&mut self, horizon: Lsn) -> Result<(), SpaceError> {
        let mut updated = self.clone();
        updated.reclaim_in_place(horizon)?;
        *self = updated;
        Ok(())
    }

    /// Record or replace the hint for `page_id`.
    ///
    /// `free_bytes == 0` stores class 0. Any other accepted count stores
    /// `floor(log2(free_bytes))`. When the table is full and `page_id` is new,
    /// the first entry is replaced. This does not change the bitmap.
    ///
    /// # Errors
    ///
    /// Returns [`SpaceError::Corrupt`] when `page_id` is reserved, at or past
    /// the high-water mark, or a spill page, and when `free_bytes` is greater
    /// than [`PAGE_SIZE`].
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use pizzlydb_storage::page::PageKind;
    /// use pizzlydb_storage::space::Space;
    ///
    /// let mut space = Space::create();
    /// let page = space.allocate(PageKind::BTreeLeaf).unwrap();
    /// space.record_free_space(page, 128).unwrap();
    ///
    /// assert_eq!(space.page_with_space(64, |_| 128), Some(page));
    /// ```
    pub fn record_free_space(
        &mut self,
        page_id: PageId,
        free_bytes: usize,
    ) -> Result<(), SpaceError> {
        if !self.hint_id_ok(page_id) || free_bytes > PAGE_SIZE {
            return Err(SpaceError::Corrupt);
        }

        let class = free_class(free_bytes);

        if let Some(hint) = self.hints.iter_mut().find(|hint| hint.page_id == page_id) {
            hint.class = class;
            return Ok(());
        }

        if self.hints.len() >= MAX_HINTS {
            self.hints[0] = Hint { page_id, class };
            return Ok(());
        }

        self.hints.push(Hint { page_id, class });
        Ok(())
    }

    /// First hint that can hold `min_free` bytes.
    ///
    /// `free_bytes` is the page's real free-byte count. A hint is skipped when
    /// the bit is clear, the page is retired, the page is a spill page, the
    /// stored class is below the class of `min_free`, or `free_bytes` reports
    /// less than `min_free`. The bitmap is not changed.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use pizzlydb_storage::page::PageKind;
    /// use pizzlydb_storage::space::Space;
    ///
    /// let mut space = Space::create();
    /// let page = space.allocate(PageKind::BTreeLeaf).unwrap();
    /// space.record_free_space(page, 100).unwrap();
    ///
    /// assert_eq!(space.page_with_space(50, |_| 0), None);
    /// let other = space.allocate(PageKind::BTreeLeaf).unwrap();
    ///
    /// assert_ne!(other, page);
    /// assert!(space.is_allocated(page));
    /// ```
    #[must_use]
    pub fn page_with_space(
        &self,
        min_free: usize,
        free_bytes: impl Fn(PageId) -> usize,
    ) -> Option<PageId> {
        if min_free > PAGE_SIZE {
            return None;
        }

        let need = free_class(min_free);
        for hint in &self.hints {
            if hint.class < need
                || !self.is_allocated(hint.page_id)
                || self.is_retired(hint.page_id)
                || self.is_spill(hint.page_id)
            {
                continue;
            }

            if free_bytes(hint.page_id) < min_free {
                continue;
            }

            return Some(hint.page_id);
        }

        None
    }

    /// Sealed header, bitmap, and spill images for [`Self::load`].
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use pizzlydb_storage::page::{PageId, PageKind};
    /// use pizzlydb_storage::space::Space;
    ///
    /// let mut space = Space::create();
    /// let page = space.allocate(PageKind::BTreeLeaf).unwrap();
    /// let encoded = space.encode();
    /// let loaded = Space::load(&encoded.header, &encoded.bitmap, &encoded.spills).unwrap();
    ///
    /// assert_eq!(loaded.allocate(PageKind::BTreeLeaf).unwrap(), PageId::new(5));
    /// assert!(loaded.is_allocated(page));
    /// ```
    #[must_use]
    pub fn encode(&self) -> EncodedSpace {
        assert_eq!(
            self.bitmap.len(),
            BITMAP_BYTES,
            "bitmap covers one page body"
        );

        assert!(
            self.retires.len() <= MAX_RETIRE,
            "header retire list fits in the page"
        );

        EncodedSpace {
            header: self.encode_header(),
            bitmap: self.encode_bitmap(),
            spills: self.encode_spills(),
        }
    }

    /// Rebuild allocator state from sealed pages.
    ///
    /// The spill slice may be in any order. The header's spill head selects
    /// the chain. After a successful load, [`Self::allocate`] uses the loaded
    /// bits.
    ///
    /// # Errors
    ///
    /// Returns [`SpaceError::Corrupt`] when a checksum, kind, page id, count,
    /// reserved byte, retire entry, hint, spill link, or reserved bit is
    /// invalid.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use pizzlydb_storage::page::PageKind;
    /// use pizzlydb_storage::space::Space;
    ///
    /// let mut space = Space::create();
    /// let page = space.allocate(PageKind::BTreeLeaf).unwrap();
    /// let encoded = space.encode();
    /// let mut loaded = Space::load(&encoded.header, &encoded.bitmap, &[]).unwrap();
    ///
    /// assert_ne!(loaded.allocate(PageKind::BTreeLeaf).unwrap(), page);
    /// ```
    pub fn load(header: &[u8], bitmap: &[u8], spills: &[SpillImage]) -> Result<Self, SpaceError> {
        let decoded = decode_header(header)?;
        let bits = decode_bitmap(bitmap)?;
        let spill_pages = decode_spill_chain(decoded.spill_head, spills, decoded.high_water)?;
        let space = Self {
            high_water: decoded.high_water,
            bitmap: bits,
            retires: decoded.retires,
            spills: spill_pages,
            hints: decoded.hints,
        };

        space.validate_state()?;
        Ok(space)
    }

    fn take_free(&mut self) -> Result<u64, SpaceError> {
        let start = FIRST_DATA_PAGE_ID.as_u64();
        if let Some(id) = self.first_clear(start, self.high_water)? {
            self.set_allocated(id)?;
            return Ok(id);
        }

        let next = self
            .high_water
            .checked_add(EXTENT_PAGES)
            .ok_or(SpaceError::Full)?;
        if next > MAX_BITMAP_PAGES {
            return Err(SpaceError::Full);
        }

        let grown = self.high_water;
        if self.first_clear(grown, next)? != Some(grown) {
            return Err(SpaceError::Corrupt);
        }

        self.write_bit(grown, true)?;
        self.high_water = next;
        Ok(grown)
    }

    fn retire_in_place(&mut self, page_id: PageId, lsn: Lsn) -> Result<(), SpaceError> {
        self.validate_new_retire(page_id, lsn)?;

        let entry = RetireEntry { page_id, lsn };
        if self.retires.len() < MAX_RETIRE {
            self.retires.push(entry);
            return Ok(());
        }

        self.spill_append(entry)
    }

    fn validate_new_retire(&self, page_id: PageId, lsn: Lsn) -> Result<(), SpaceError> {
        if lsn.as_u64() == 0 || !self.data_id(page_id.as_u64()) || self.is_spill(page_id) {
            return Err(SpaceError::Corrupt);
        }

        if !self.bit(page_id.as_u64())? {
            return Err(SpaceError::DoubleFree);
        }

        if self.is_retired(page_id) {
            return Err(SpaceError::Corrupt);
        }

        Ok(())
    }

    fn spill_append(&mut self, entry: RetireEntry) -> Result<(), SpaceError> {
        if self.spills.is_empty() {
            let page_id = PageId::new(self.take_free()?);
            self.spills.push(SpillPage {
                page_id,
                next: None,
                entries: vec![entry],
            });
            return Ok(());
        }

        let last = self.spills.len() - 1;
        if self.spills[last].entries.len() < SPILL_CAPACITY {
            self.spills[last].entries.push(entry);
            return Ok(());
        }

        let page_id = PageId::new(self.take_free()?);
        self.spills.push(SpillPage {
            page_id,
            next: None,
            entries: vec![entry],
        });

        self.spills[last].next = Some(page_id);
        Ok(())
    }

    fn reclaim_in_place(&mut self, horizon: Lsn) -> Result<(), SpaceError> {
        let pending = mem::take(&mut self.retires);
        let mut kept = Vec::with_capacity(pending.len());
        for entry in pending {
            if entry.lsn < horizon {
                self.release(entry.page_id)?;
            } else {
                kept.push(entry);
            }
        }

        self.retires = kept;

        let pending_spills = mem::take(&mut self.spills);
        let mut kept_spills = Vec::with_capacity(pending_spills.len());
        for mut spill in pending_spills {
            let entries = mem::take(&mut spill.entries);
            for entry in entries {
                if entry.lsn < horizon {
                    self.release(entry.page_id)?;
                } else {
                    spill.entries.push(entry);
                }
            }

            if spill.entries.is_empty() {
                self.release(spill.page_id)?;
                continue;
            }

            kept_spills.push(spill);
        }

        relink(&mut kept_spills);
        self.spills = kept_spills;

        Ok(())
    }

    fn release(&mut self, page_id: PageId) -> Result<(), SpaceError> {
        self.clear_allocated(page_id.as_u64())
    }

    fn set_allocated(&mut self, id: u64) -> Result<(), SpaceError> {
        if !self.data_id(id) {
            return Err(SpaceError::Corrupt);
        }

        if self.bit(id)? {
            return Err(SpaceError::Corrupt);
        }

        self.write_bit(id, true)
    }

    fn clear_allocated(&mut self, id: u64) -> Result<(), SpaceError> {
        if id < FIRST_DATA_PAGE_ID.as_u64() {
            return Err(SpaceError::Corrupt);
        }

        if !self.bit(id)? {
            return Err(SpaceError::DoubleFree);
        }

        self.write_bit(id, false)
    }

    fn grow(&mut self) -> Result<(), SpaceError> {
        let next = self
            .high_water
            .checked_add(EXTENT_PAGES)
            .ok_or(SpaceError::Full)?;

        if next > MAX_BITMAP_PAGES {
            return Err(SpaceError::Full);
        }

        self.high_water = next;
        Ok(())
    }

    fn data_id(&self, id: u64) -> bool {
        id >= FIRST_DATA_PAGE_ID.as_u64() && id < self.high_water
    }

    fn hint_id_ok(&self, page_id: PageId) -> bool {
        self.data_id(page_id.as_u64()) && !self.is_spill(page_id)
    }

    fn is_spill(&self, page_id: PageId) -> bool {
        self.spills.iter().any(|spill| spill.page_id == page_id)
    }

    fn first_clear(&self, start: u64, end: u64) -> Result<Option<u64>, SpaceError> {
        let mut id = start;
        while id < end {
            if !self.bit(id)? {
                return Ok(Some(id));
            }
            id += 1;
        }

        Ok(None)
    }

    fn bit(&self, id: u64) -> Result<bool, SpaceError> {
        let (byte, bit) = bit_index(id)?;
        let slot = self.bitmap.get(byte).copied().ok_or(SpaceError::Corrupt)?;
        Ok((slot & (1u8 << bit)) != 0)
    }

    fn write_bit(&mut self, id: u64, allocated: bool) -> Result<(), SpaceError> {
        let (byte, bit) = bit_index(id)?;
        let slot = self.bitmap.get_mut(byte).ok_or(SpaceError::Corrupt)?;
        let mask = 1u8 << bit;

        if allocated {
            *slot |= mask;
        } else {
            *slot &= !mask;
        }

        Ok(())
    }

    fn encode_header(&self) -> [u8; PAGE_SIZE] {
        let mut page = [0u8; PAGE_SIZE];
        write_u64(&mut page, HIGH_WATER_OFFSET, self.high_water);
        write_u64(&mut page, BITMAP_ID_OFFSET, BITMAP_PAGE_ID.as_u64());
        write_u16(
            &mut page,
            RETIRE_COUNT_OFFSET,
            count_u16(self.retires.len()),
        );
        write_u16(&mut page, HINT_COUNT_OFFSET, count_u16(self.hints.len()));

        for (index, entry) in self.retires.iter().enumerate() {
            write_retire(
                &mut page,
                RETIRE_LIST_OFFSET + index * RETIRE_ENTRY_LEN,
                entry,
            );
        }

        for (index, hint) in self.hints.iter().enumerate() {
            let offset = HINT_LIST_OFFSET + index * HINT_ENTRY_LEN;
            write_u64(&mut page, offset, hint.page_id.as_u64());
            page[offset + 8] = hint.class;
        }

        let head = self.spill_head().map_or(0, PageId::as_u64);
        write_u64(&mut page, SPILL_HEAD_OFFSET, head);
        seal_page(&mut page, PageKind::Allocator, ALLOCATOR_PAGE_ID);

        page
    }

    fn encode_bitmap(&self) -> [u8; PAGE_SIZE] {
        let mut page = [0u8; PAGE_SIZE];
        page[HEADER_SIZE..].copy_from_slice(&self.bitmap);
        seal_page(&mut page, PageKind::Bitmap, BITMAP_PAGE_ID);
        page
    }

    fn encode_spills(&self) -> Vec<SpillImage> {
        self.spills
            .iter()
            .map(|spill| SpillImage {
                page_id: spill.page_id,
                bytes: encode_spill(spill),
            })
            .collect()
    }

    fn validate_state(&self) -> Result<(), SpaceError> {
        if !valid_high_water(self.high_water) || self.bitmap.len() != BITMAP_BYTES {
            return Err(SpaceError::Corrupt);
        }

        if !tail_is_clear(&self.bitmap, self.high_water)? {
            return Err(SpaceError::Corrupt);
        }

        for id in 0..FIRST_DATA_PAGE_ID.as_u64() {
            if !self.bit(id)? {
                return Err(SpaceError::Corrupt);
            }
        }

        let mut seen = Vec::new();
        for spill in &self.spills {
            if !self.data_id(spill.page_id.as_u64()) || !self.bit(spill.page_id.as_u64())? {
                return Err(SpaceError::Corrupt);
            }
            note_unique(&mut seen, spill.page_id)?;
            if spill.entries.is_empty() || spill.entries.len() > SPILL_CAPACITY {
                return Err(SpaceError::Corrupt);
            }
        }

        for entry in self.retire_entries() {
            if entry.lsn.as_u64() == 0 || !self.data_id(entry.page_id.as_u64()) {
                return Err(SpaceError::Corrupt);
            }
            if !self.bit(entry.page_id.as_u64())? {
                return Err(SpaceError::Corrupt);
            }
            note_unique(&mut seen, entry.page_id)?;
        }

        for hint in &self.hints {
            if !self.data_id(hint.page_id.as_u64()) || hint.class > MAX_FREE_CLASS {
                return Err(SpaceError::Corrupt);
            }
        }

        let mut hint_ids = Vec::new();
        for hint in &self.hints {
            note_unique(&mut hint_ids, hint.page_id)?;
        }

        Ok(())
    }

    fn retire_entries(&self) -> impl Iterator<Item = RetireEntry> + '_ {
        self.retires.iter().copied().chain(
            self.spills
                .iter()
                .flat_map(|spill| spill.entries.iter().copied()),
        )
    }
}

struct DecodedHeader {
    high_water: u64,
    spill_head: u64,
    retires: Vec<RetireEntry>,
    hints: Vec<Hint>,
}

#[allow(private_interfaces)]
pub(crate) fn decode_header(page: &[u8]) -> Result<DecodedHeader, SpaceError> {
    let header = PageHeader::decode(page).map_err(|_| SpaceError::Corrupt)?;
    if header.kind() != PageKind::Allocator || header.page_id() != ALLOCATOR_PAGE_ID {
        return Err(SpaceError::Corrupt);
    }

    let high_water = read_u64(page, HIGH_WATER_OFFSET)?;
    if !valid_high_water(high_water) || read_u64(page, BITMAP_ID_OFFSET)? != BITMAP_PAGE_ID.as_u64()
    {
        return Err(SpaceError::Corrupt);
    }

    if !is_zero(&page[HEADER_RESERVED_OFFSET..RETIRE_LIST_OFFSET])
        || !is_zero(&page[PAGE_SIZE - 8..])
    {
        return Err(SpaceError::Corrupt);
    }

    let retire_count = usize::from(read_u16(page, RETIRE_COUNT_OFFSET)?);
    let hint_count = usize::from(read_u16(page, HINT_COUNT_OFFSET)?);
    if retire_count > MAX_RETIRE || hint_count > MAX_HINTS {
        return Err(SpaceError::Corrupt);
    }

    let retire_used = RETIRE_LIST_OFFSET + retire_count * RETIRE_ENTRY_LEN;
    let retire_end = RETIRE_LIST_OFFSET + MAX_RETIRE * RETIRE_ENTRY_LEN;
    if !is_zero(&page[retire_used..retire_end]) {
        return Err(SpaceError::Corrupt);
    }

    let hint_used = HINT_LIST_OFFSET + hint_count * HINT_ENTRY_LEN;
    if !is_zero(&page[hint_used..SPILL_HEAD_OFFSET]) {
        return Err(SpaceError::Corrupt);
    }

    let mut retires = Vec::with_capacity(retire_count);
    for index in 0..retire_count {
        retires.push(read_retire(
            page,
            RETIRE_LIST_OFFSET + index * RETIRE_ENTRY_LEN,
        )?);
    }

    let mut hints = Vec::with_capacity(hint_count);
    for index in 0..hint_count {
        hints.push(read_hint(page, HINT_LIST_OFFSET + index * HINT_ENTRY_LEN)?);
    }

    let spill_head = read_u64(page, SPILL_HEAD_OFFSET)?;
    if spill_head != 0 && !data_id_in(spill_head, high_water) {
        return Err(SpaceError::Corrupt);
    }

    Ok(DecodedHeader {
        high_water,
        spill_head,
        retires,
        hints,
    })
}

pub(crate) fn decode_bitmap(page: &[u8]) -> Result<Vec<u8>, SpaceError> {
    let header = PageHeader::decode(page).map_err(|_| SpaceError::Corrupt)?;
    if header.kind() != PageKind::Bitmap || header.page_id() != BITMAP_PAGE_ID {
        return Err(SpaceError::Corrupt);
    }
    Ok(page[HEADER_SIZE..].to_vec())
}

struct ParsedSpill {
    page_id: PageId,
    next: u64,
    entries: Vec<RetireEntry>,
}

#[allow(private_interfaces)]
pub(crate) fn decode_spill_chain(
    head: u64,
    images: &[SpillImage],
    high_water: u64,
) -> Result<Vec<SpillPage>, SpaceError> {
    let mut pending = Vec::with_capacity(images.len());
    for image in images {
        pending.push(decode_spill_image(image, high_water)?);
    }

    let mut ordered = Vec::with_capacity(pending.len());
    let mut current = head;
    let mut hops = 0usize;
    while current != 0 {
        hops += 1;
        if hops > images.len() {
            return Err(SpaceError::Corrupt);
        }
        let parsed = take_spill(&mut pending, current)?;
        let next = parsed.next;
        ordered.push(SpillPage {
            page_id: parsed.page_id,
            next: (next != 0).then(|| PageId::new(next)),
            entries: parsed.entries,
        });
        current = next;
    }

    if !pending.is_empty() {
        return Err(SpaceError::Corrupt);
    }

    Ok(ordered)
}

#[cfg(feature = "fuzzing")]
pub(crate) fn header_ok(page: &[u8]) -> bool {
    decode_header(page).is_ok()
}

#[cfg(feature = "fuzzing")]
pub(crate) fn spill_chain_ok(head: u64, images: &[SpillImage], high_water: u64) -> bool {
    decode_spill_chain(head, images, high_water).is_ok()
}

#[cfg(feature = "fuzzing")]
pub(crate) fn spill_image_ok(image: &SpillImage, high_water: u64) -> bool {
    decode_spill_image(image, high_water).is_ok()
}

#[allow(private_interfaces)]
pub(crate) fn decode_spill_image(
    image: &SpillImage,
    high_water: u64,
) -> Result<ParsedSpill, SpaceError> {
    let header = PageHeader::decode(&image.bytes).map_err(|_| SpaceError::Corrupt)?;
    if header.kind() != PageKind::RetireSpill || header.page_id() != image.page_id {
        return Err(SpaceError::Corrupt);
    }

    if !data_id_in(image.page_id.as_u64(), high_water) {
        return Err(SpaceError::Corrupt);
    }

    let next = read_u64(&image.bytes, SPILL_NEXT_OFFSET)?;
    if next != 0 && !data_id_in(next, high_water) {
        return Err(SpaceError::Corrupt);
    }

    let count = usize::from(read_u16(&image.bytes, SPILL_COUNT_OFFSET)?);
    if count == 0 || count > SPILL_CAPACITY {
        return Err(SpaceError::Corrupt);
    }

    if !is_zero(&image.bytes[SPILL_RESERVED_OFFSET..SPILL_LIST_OFFSET]) {
        return Err(SpaceError::Corrupt);
    }

    let mut entries = Vec::with_capacity(count);
    for index in 0..count {
        entries.push(read_retire(
            &image.bytes,
            SPILL_LIST_OFFSET + index * SPILL_ENTRY_LEN,
        )?);
    }

    let used_end = SPILL_LIST_OFFSET + count * SPILL_ENTRY_LEN;
    if !is_zero(&image.bytes[used_end..]) {
        return Err(SpaceError::Corrupt);
    }

    Ok(ParsedSpill {
        page_id: image.page_id,
        next,
        entries,
    })
}

fn take_spill(pending: &mut Vec<ParsedSpill>, id: u64) -> Result<ParsedSpill, SpaceError> {
    let index = pending
        .iter()
        .position(|spill| spill.page_id.as_u64() == id)
        .ok_or(SpaceError::Corrupt)?;

    Ok(pending.swap_remove(index))
}

fn encode_spill(spill: &SpillPage) -> [u8; PAGE_SIZE] {
    assert!(
        !spill.entries.is_empty() && spill.entries.len() <= SPILL_CAPACITY,
        "spill page stores one or more entries"
    );

    let mut page = [0u8; PAGE_SIZE];
    write_u64(
        &mut page,
        SPILL_NEXT_OFFSET,
        spill.next.map_or(0, PageId::as_u64),
    );
    write_u16(
        &mut page,
        SPILL_COUNT_OFFSET,
        count_u16(spill.entries.len()),
    );

    for (index, entry) in spill.entries.iter().enumerate() {
        write_retire(
            &mut page,
            SPILL_LIST_OFFSET + index * SPILL_ENTRY_LEN,
            entry,
        );
    }

    seal_page(&mut page, PageKind::RetireSpill, spill.page_id);
    page
}

fn relink(spills: &mut [SpillPage]) {
    if spills.is_empty() {
        return;
    }

    let last = spills.len() - 1;
    for index in 0..last {
        let next = spills[index + 1].page_id;
        spills[index].next = Some(next);
    }

    spills[last].next = None;
}

fn valid_high_water(high_water: u64) -> bool {
    (INITIAL_PAGES..=MAX_BITMAP_PAGES).contains(&high_water)
        && (high_water - INITIAL_PAGES).is_multiple_of(EXTENT_PAGES)
}

fn data_id_in(id: u64, high_water: u64) -> bool {
    id >= FIRST_DATA_PAGE_ID.as_u64() && id < high_water
}

fn note_unique(seen: &mut Vec<PageId>, page_id: PageId) -> Result<(), SpaceError> {
    if seen.contains(&page_id) {
        return Err(SpaceError::Corrupt);
    }
    seen.push(page_id);
    Ok(())
}

fn tail_is_clear(bitmap: &[u8], start: u64) -> Result<bool, SpaceError> {
    let start = usize::try_from(start).map_err(|_| SpaceError::Corrupt)?;
    let byte_index = start / 8;
    if byte_index >= bitmap.len() {
        return Ok(true);
    }

    let bit = start % 8;
    if bit != 0 {
        let mask = u8::MAX << bit;
        if bitmap[byte_index] & mask != 0 {
            return Ok(false);
        }
        return Ok(bitmap[byte_index + 1..].iter().all(|byte| *byte == 0));
    }

    Ok(bitmap[byte_index..].iter().all(|byte| *byte == 0))
}

fn bit_index(id: u64) -> Result<(usize, u8), SpaceError> {
    if id >= MAX_BITMAP_PAGES {
        return Err(SpaceError::Corrupt);
    }

    let id = usize::try_from(id).map_err(|_| SpaceError::Corrupt)?;
    let bit = u8::try_from(id % 8).map_err(|_| SpaceError::Corrupt)?;

    Ok((id / 8, bit))
}

fn free_class(free_bytes: usize) -> u8 {
    if free_bytes == 0 {
        return 0;
    }
    u8::try_from(free_bytes.ilog2()).expect("log2 of a byte count fits in u8")
}

fn count_u16(len: usize) -> u16 {
    u16::try_from(len).expect("count fits in u16")
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, SpaceError> {
    get_u16(bytes, offset).map_err(|_| SpaceError::Corrupt)
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, SpaceError> {
    get_u64(bytes, offset).map_err(|_| SpaceError::Corrupt)
}

fn write_u16(bytes: &mut [u8], offset: usize, value: u16) {
    put_u16(bytes, offset, value).expect("offset fits in the page");
}

fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
    put_u64(bytes, offset, value).expect("offset fits in the page");
}

fn read_retire(bytes: &[u8], offset: usize) -> Result<RetireEntry, SpaceError> {
    Ok(RetireEntry {
        page_id: PageId::new(read_u64(bytes, offset)?),
        lsn: Lsn::new(read_u64(bytes, offset + 8)?),
    })
}

fn write_retire(bytes: &mut [u8], offset: usize, entry: &RetireEntry) {
    write_u64(bytes, offset, entry.page_id.as_u64());
    write_u64(bytes, offset + 8, entry.lsn.as_u64());
}

fn read_hint(bytes: &[u8], offset: usize) -> Result<Hint, SpaceError> {
    if !is_zero(&bytes[offset + 9..offset + HINT_ENTRY_LEN]) {
        return Err(SpaceError::Corrupt);
    }

    Ok(Hint {
        page_id: PageId::new(read_u64(bytes, offset)?),
        class: bytes[offset + 8],
    })
}

fn is_zero(bytes: &[u8]) -> bool {
    bytes.iter().all(|byte| *byte == 0)
}

fn seal_page(page: &mut [u8; PAGE_SIZE], kind: PageKind, page_id: PageId) {
    PageHeader::new(kind, page_id, Lsn::new(0))
        .encode(page)
        .expect("page image is one page");
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        EXTENT_PAGES, INITIAL_PAGES, MAX_BITMAP_PAGES, MAX_HINTS, MAX_RETIRE, RETIRE_ENTRY_LEN,
        RETIRE_LIST_OFFSET, SPILL_CAPACITY, SPILL_NEXT_OFFSET, Space, SpaceError,
    };
    use crate::codec::{get_u64, put_u64};
    use crate::page::{
        ALLOCATOR_PAGE_ID, BITMAP_PAGE_ID, FIRST_DATA_PAGE_ID, HEADER_SIZE, Lsn, PAGE_SIZE,
        PageHeader, PageId, PageKind, seal,
    };
    use crate::pager::Pager;
    use crate::vfs::{FileRole, OpenOptions, Vfs, VfsFile};
    use crate::vfs_memory::{MemoryFile, MemoryVfs};
    use crate::wal::Wal;

    fn leaf() -> PageKind {
        PageKind::BTreeLeaf
    }

    fn grow_to_limit(space: &mut Space) {
        let start = space.high_water();
        while space.grow().is_ok() {}
        assert!(space.high_water() > start);
        assert_eq!((space.high_water() - INITIAL_PAGES) % EXTENT_PAGES, 0);
        assert!(space.high_water() + EXTENT_PAGES > MAX_BITMAP_PAGES);
    }

    fn allocate_every_data_page(space: &mut Space) {
        let mut id = FIRST_DATA_PAGE_ID.as_u64();
        while id < space.high_water() {
            space.set_allocated(id).expect("data page is free");
            id += 1;
        }
    }

    fn open_pager() -> Pager<MemoryFile, MemoryFile> {
        let vfs = MemoryVfs::new();
        let mut main = vfs
            .open(
                Path::new("db"),
                FileRole::Main,
                &OpenOptions::new().create(true),
            )
            .expect("open main");
        let mut image = [0u8; PAGE_SIZE];
        PageHeader::new(PageKind::BTreeLeaf, PageId::new(0), Lsn::new(0))
            .encode(&mut image)
            .expect("encode page");
        main.write_all(0, &image).expect("write page");
        let wal_file = vfs
            .open(
                Path::new("db.wal"),
                FileRole::Wal,
                &OpenOptions::new().create(true),
            )
            .expect("open wal");
        let wal = Wal::create(wal_file, [9; 16]).expect("create wal");
        Pager::open(main, wal, 1).expect("open pager")
    }

    #[test]
    fn allocate_skips_reserved_pages_and_sets_each_bit_once() {
        let mut space = Space::create();
        assert!(space.is_allocated(PageId::new(0)));
        assert!(space.is_allocated(PageId::new(1)));
        assert!(space.is_allocated(ALLOCATOR_PAGE_ID));
        assert!(space.is_allocated(BITMAP_PAGE_ID));
        assert_eq!(
            space.allocate(PageKind::Superblock),
            Err(SpaceError::Corrupt)
        );
        assert_eq!(
            space.allocate(PageKind::Allocator),
            Err(SpaceError::Corrupt)
        );
        assert_eq!(space.allocate(PageKind::Bitmap), Err(SpaceError::Corrupt));

        let mut seen = Vec::new();
        for _ in 0..10 {
            let id = space.allocate(leaf()).expect("allocate");
            assert!(id.as_u64() >= FIRST_DATA_PAGE_ID.as_u64());
            assert!(!seen.contains(&id));
            assert!(space.is_allocated(id));
            seen.push(id);
        }
        assert_eq!(seen[0], FIRST_DATA_PAGE_ID);
        assert_eq!(space.high_water(), INITIAL_PAGES);
    }

    #[test]
    fn double_allocation_is_corrupt() {
        let mut space = Space::create();
        let page = space.allocate(leaf()).expect("allocate");
        let before = space.clone();
        assert_eq!(space.set_allocated(page.as_u64()), Err(SpaceError::Corrupt));
        assert_eq!(space, before);
    }

    #[test]
    fn double_free_is_an_error() {
        let mut space = Space::create();
        let before = space.clone();
        assert_eq!(
            space.retire(PageId::new(10), Lsn::new(1)),
            Err(SpaceError::DoubleFree)
        );
        assert_eq!(space.clear_allocated(10), Err(SpaceError::DoubleFree));
        assert_eq!(space.clear_allocated(0), Err(SpaceError::Corrupt));
        assert_eq!(
            space.retire(PageId::new(1), Lsn::new(1)),
            Err(SpaceError::Corrupt)
        );
        assert_eq!(
            space.retire(PageId::new(space.high_water()), Lsn::new(1)),
            Err(SpaceError::Corrupt)
        );
        assert_eq!(space, before);

        let page = space.allocate(leaf()).expect("page");
        assert_eq!(space.retire(page, Lsn::new(0)), Err(SpaceError::Corrupt));
        space.retire(page, Lsn::new(4)).expect("retire");
        let queued = space.clone();
        assert_eq!(space.retire(page, Lsn::new(5)), Err(SpaceError::Corrupt));
        assert_eq!(space, queued);
        space.clear_allocated(page.as_u64()).expect("clear bit");
        let cleared = space.clone();
        assert_eq!(space.reclaim(Lsn::new(5)), Err(SpaceError::DoubleFree));
        assert_eq!(space, cleared);
    }

    #[test]
    fn horizon_blocks_reuse_until_it_passes_the_retire_lsn() {
        let mut space = Space::create();
        let page = space.allocate(leaf()).expect("allocate");
        space.retire(page, Lsn::new(11)).expect("retire");
        space.reclaim(Lsn::new(11)).expect("reclaim at horizon");
        assert!(space.is_allocated(page));
        assert!(space.is_retired(page));
        let other = space.allocate(leaf()).expect("other page");
        assert_ne!(other, page);

        space.reclaim(Lsn::new(12)).expect("reclaim past horizon");
        assert!(!space.is_allocated(page));
        assert!(!space.is_retired(page));
        let reused = space.allocate(leaf()).expect("reuse");
        assert_eq!(reused, page);
        let third = space.allocate(leaf()).expect("third");
        assert_ne!(third, page);
    }

    #[test]
    fn hint_for_a_full_page_does_not_add_an_owner() {
        let mut space = Space::create();
        let page = space.allocate(leaf()).expect("allocate");
        space.record_free_space(page, 100).expect("hint");
        let hinted = space.encode();
        let loaded =
            Space::load(&hinted.header, &hinted.bitmap, &hinted.spills).expect("load hint");
        assert_eq!(loaded.page_with_space(50, |_| 100), Some(page));
        assert_eq!(space.page_with_space(50, |_| 0), None);
        assert_eq!(space.encode().bitmap, hinted.bitmap);
        assert_eq!(space.page_with_space(50, |_| 100), Some(page));

        let other = space.allocate(leaf()).expect("other");
        assert_ne!(other, page);
        assert!(space.is_allocated(page));
        assert!(space.is_allocated(other));

        space.retire(page, Lsn::new(3)).expect("retire");
        assert_eq!(space.page_with_space(50, |_| 100), None);
        assert!(space.is_allocated(page));
        assert_eq!(
            space.record_free_space(ALLOCATOR_PAGE_ID, 10),
            Err(SpaceError::Corrupt)
        );
    }

    #[test]
    fn a_full_hint_table_replaces_the_first_entry() {
        let mut space = Space::create();
        let first = space.allocate(leaf()).expect("first");
        space.record_free_space(first, 64).expect("first hint");
        let mut kept = None;
        for index in 1..MAX_HINTS {
            let page = space.allocate(leaf()).expect("hint page");
            space.record_free_space(page, 32).expect("hint");
            if index == 1 {
                kept = Some(page);
            }
        }
        let kept = kept.expect("second hint");
        let extra = space.allocate(leaf()).expect("extra");
        space.record_free_space(extra, 16).expect("replace first");
        assert_eq!(space.hints[0].page_id, extra);
        assert_eq!(space.hints[1].page_id, kept);
        space.record_free_space(kept, 128).expect("update in place");
        assert_eq!(space.hints[1].page_id, kept);
        assert_eq!(space.page_with_space(64, |_| 128), Some(kept));
        assert_ne!(space.page_with_space(16, |_| 16), Some(first));
        assert!(space.is_allocated(first));
    }

    #[test]
    fn a_hint_that_names_a_later_spill_page_still_loads() {
        let mut space = Space::create();
        let hinted = space.allocate(leaf()).expect("hinted");
        space.record_free_space(hinted, 100).expect("hint");
        for _ in 0..MAX_RETIRE {
            let page = space.allocate(leaf()).expect("header page");
            space.retire(page, Lsn::new(5)).expect("fill header");
        }
        let extra = space.allocate(leaf()).expect("extra");
        space
            .retire(hinted, Lsn::new(1))
            .expect("queue hinted page");
        space.reclaim(Lsn::new(2)).expect("free hinted page");
        assert!(!space.is_allocated(hinted));
        assert_eq!(space.header_retire_count(), MAX_RETIRE);
        assert_eq!(space.spill_head(), None);

        space
            .retire(extra, Lsn::new(6))
            .expect("spill onto hinted id");
        assert_eq!(space.spill_head(), Some(hinted));
        assert_eq!(
            space.record_free_space(hinted, 32),
            Err(SpaceError::Corrupt)
        );
        let encoded = space.encode();
        let loaded = Space::load(&encoded.header, &encoded.bitmap, &encoded.spills)
            .expect("stale hint still loads");
        assert_eq!(loaded, space);
        assert_ne!(loaded.page_with_space(50, |_| 100), Some(hinted));
    }

    #[test]
    fn retire_spills_when_the_header_is_full_and_reloads() {
        let mut space = Space::create();
        let mut ids = Vec::new();
        for _ in 0..=MAX_RETIRE {
            ids.push(space.allocate(leaf()).expect("allocate"));
        }
        for id in &ids[..MAX_RETIRE] {
            space.retire(*id, Lsn::new(1)).expect("retire into header");
        }
        assert_eq!(space.header_retire_count(), MAX_RETIRE);
        assert_eq!(space.spill_head(), None);

        space.retire(ids[MAX_RETIRE], Lsn::new(2)).expect("spill");
        let head = space.spill_head().expect("spill head");
        assert!(space.is_allocated(head));
        assert!(space.is_allocated(ids[MAX_RETIRE]));
        assert!(space.is_retired(ids[MAX_RETIRE]));
        assert_eq!(space.header_retire_count(), MAX_RETIRE);

        let encoded = space.encode();
        assert_eq!(encoded.spills.len(), 1);
        assert_eq!(encoded.spills[0].page_id, head);
        let mut loaded =
            Space::load(&encoded.header, &encoded.bitmap, &encoded.spills).expect("load spill");
        assert_eq!(loaded, space);

        loaded.reclaim(Lsn::new(2)).expect("reclaim header only");
        assert_eq!(loaded.header_retire_count(), 0);
        assert!(loaded.is_retired(ids[MAX_RETIRE]));
        assert!(loaded.is_allocated(head));
        loaded.reclaim(Lsn::new(3)).expect("reclaim spill");
        assert_eq!(loaded.spill_head(), None);
        assert!(!loaded.is_allocated(ids[MAX_RETIRE]));
        assert!(!loaded.is_allocated(head));
        for id in 0..FIRST_DATA_PAGE_ID.as_u64() {
            assert!(loaded.is_allocated(PageId::new(id)));
        }
    }

    #[test]
    fn retire_spill_links_a_second_page_when_the_first_is_full() {
        let mut space = Space::create();
        let count = MAX_RETIRE + SPILL_CAPACITY + 1;
        let mut ids = Vec::with_capacity(count);
        for _ in 0..count {
            ids.push(space.allocate(leaf()).expect("allocate"));
        }
        for id in &ids[..count - 1] {
            space.retire(*id, Lsn::new(1)).expect("fill first spill");
        }
        assert_eq!(space.spills.len(), 1);
        space
            .retire(ids[count - 1], Lsn::new(2))
            .expect("second spill");
        assert_eq!(space.spills.len(), 2);

        let encoded = space.encode();
        let next = get_u64(&encoded.spills[0].bytes, SPILL_NEXT_OFFSET).expect("next");
        assert_eq!(next, encoded.spills[1].page_id.as_u64());
        assert_eq!(
            get_u64(&encoded.spills[1].bytes, SPILL_NEXT_OFFSET).expect("tail"),
            0
        );
        let reversed = vec![encoded.spills[1].clone(), encoded.spills[0].clone()];
        let loaded =
            Space::load(&encoded.header, &encoded.bitmap, &reversed).expect("load reversed");
        assert_eq!(loaded, space);
    }

    #[test]
    fn failed_spill_leaves_the_header_unchanged() {
        let mut space = Space::create();
        grow_to_limit(&mut space);
        allocate_every_data_page(&mut space);
        let high_water = space.high_water();
        for id in FIRST_DATA_PAGE_ID.as_u64()..FIRST_DATA_PAGE_ID.as_u64() + MAX_RETIRE as u64 {
            space
                .retire(PageId::new(id), Lsn::new(1))
                .expect("fill header");
        }
        let published = space.encode();
        let next = PageId::new(FIRST_DATA_PAGE_ID.as_u64() + MAX_RETIRE as u64);
        assert_eq!(space.retire(next, Lsn::new(2)), Err(SpaceError::Full));
        assert_eq!(space.encode(), published);
        assert_eq!(space.high_water(), high_water);
        assert_eq!(space.spill_head(), None);
        assert_eq!(space.allocate(leaf()), Err(SpaceError::Full));
        assert_eq!(space.high_water(), high_water);
    }

    #[test]
    fn load_rejects_corrupt_images() {
        let mut cleared = Space::create().encode();
        cleared.bitmap[HEADER_SIZE] &= !1;
        seal(&mut cleared.bitmap).expect("reseal bitmap");
        assert_eq!(
            Space::load(&cleared.header, &cleared.bitmap, &cleared.spills),
            Err(SpaceError::Corrupt)
        );

        let mut checksum = Space::create().encode();
        checksum.header[HEADER_SIZE] ^= 0xff;
        assert_eq!(
            Space::load(&checksum.header, &checksum.bitmap, &checksum.spills),
            Err(SpaceError::Corrupt)
        );

        let mut space = Space::create();
        let first = space.allocate(leaf()).expect("first");
        let second = space.allocate(leaf()).expect("second");
        space.retire(first, Lsn::new(1)).expect("retire first");
        space.retire(second, Lsn::new(2)).expect("retire second");
        let mut duplicate = space.encode();
        put_u64(
            &mut duplicate.header,
            RETIRE_LIST_OFFSET + RETIRE_ENTRY_LEN,
            first.as_u64(),
        )
        .expect("patch retire id");
        seal(&mut duplicate.header).expect("reseal header");
        assert_eq!(
            Space::load(&duplicate.header, &duplicate.bitmap, &duplicate.spills),
            Err(SpaceError::Corrupt)
        );
    }

    #[test]
    fn growth_adds_one_extent_and_keeps_reserved_bits() {
        let mut space = Space::create();
        let free = INITIAL_PAGES - FIRST_DATA_PAGE_ID.as_u64();
        let mut last = PageId::new(0);
        for _ in 0..free {
            last = space.allocate(leaf()).expect("fill extent");
        }
        assert_eq!(space.high_water(), INITIAL_PAGES);
        assert_eq!(last.as_u64(), INITIAL_PAGES - 1);
        let grown = space.allocate(leaf()).expect("grow");
        assert_eq!(space.high_water(), INITIAL_PAGES + EXTENT_PAGES);
        assert_eq!(grown.as_u64(), INITIAL_PAGES);
        for id in 0..FIRST_DATA_PAGE_ID.as_u64() {
            assert!(space.is_allocated(PageId::new(id)));
        }
    }

    #[test]
    fn loaded_bitmap_is_used_for_the_next_allocation() {
        let mut space = Space::create();
        let page = space.allocate(leaf()).expect("allocate");
        let encoded = space.encode();
        let mut loaded =
            Space::load(&encoded.header, &encoded.bitmap, &encoded.spills).expect("load");
        assert!(loaded.is_allocated(page));
        assert_ne!(loaded.allocate(leaf()).expect("next"), page);
    }

    #[test]
    fn snapshot_guard_blocks_reuse_until_it_drops() {
        let pager = open_pager();
        let setup = pager.snapshot();
        let pin = pager.pin(PageId::new(0), setup.lsn()).expect("pin page 0");
        pin.mark_dirty();
        pager.commit_frame(&pin).expect("commit");
        drop(pin);
        drop(setup);

        let guard = pager.snapshot();
        let retire_lsn = guard.lsn();
        assert_ne!(retire_lsn, Lsn::new(0));
        assert_eq!(pager.snapshot_lsn(), retire_lsn);

        let mut space = Space::create();
        let page = space.allocate(leaf()).expect("allocate");
        space.retire(page, retire_lsn).expect("retire");
        space
            .reclaim(pager.snapshot_lsn())
            .expect("reclaim at snapshot");
        assert!(space.is_allocated(page));
        let other = space.allocate(leaf()).expect("other");
        assert_ne!(other, page);

        drop(guard);
        let horizon = pager.snapshot_lsn();
        assert!(horizon > retire_lsn);
        space.reclaim(horizon).expect("reclaim after drop");
        assert!(!space.is_allocated(page));
        let reused = space.allocate(leaf()).expect("reuse");
        assert_eq!(reused, page);
        let third = space.allocate(leaf()).expect("third");
        assert_ne!(third, page);
        assert_ne!(third, other);
    }
}
