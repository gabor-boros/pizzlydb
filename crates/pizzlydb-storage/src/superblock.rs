//! Dual checksummed superblocks at page ids 0 and 1.
//!
//! Each copy is a sealed [`PageKind::Superblock`] page. The page-header LSN is
//! the checkpoint LSN. [`Superblock::open`] reads both copies and
//! [`Superblock::select`] keeps the higher valid generation. Equal generations
//! keep page 0. One valid copy is enough. Zero valid copies is an error.
//!
//! The body starts at [`HEADER_SIZE`](crate::page::HEADER_SIZE). Version
//! [`FORMAT_MAJOR`] / [`FORMAT_MINOR`]:
//!
//! - file id, 16 bytes
//! - major `u16`, minor `u16`
//! - page size `u32`
//! - generation `u64`
//! - checkpoint LSN `u64`
//! - logical page count `u64`
//! - allocator page id `u64`
//! - free-space map page id `u64`
//! - B-tree root page id `u64`
//!
//! Bytes after [`BODY_END`] are zero. The body has no node or edge counters
//! and no database lineage field. The file id identifies this main file and
//! its log only. A later copy draws a new file id; lineage is deferred to the
//! backup manifest. See `docs/architecture/file-identity.md`.
//! A major other than [`FORMAT_MAJOR`], a minor other than [`FORMAT_MINOR`], or
//! a page size other than [`PAGE_SIZE`](crate::page::PAGE_SIZE) is
//! [`SuperblockError::UnsupportedVersion`].
//!
//! `logical_page_count` is at least 2, so both copies fit in the file. A page
//! id of zero means that slot is unset. Any other id is at least 2 and below
//! `logical_page_count`.
//!
//! [`Superblock::write`] replaces one copy and leaves the other bytes untouched.
//! It does not sync. The caller syncs the main file after the new copy is written.
//! [`SuperblockError`] is this record's own error.

use thiserror::Error;

use crate::codec::{CodecError, get_u16, get_u32, get_u64, put_u16, put_u32, put_u64};
use crate::page::{HEADER_SIZE, Lsn, PAGE_SIZE, PageHeader, PageId, PageKind};
use crate::vfs::{VfsError, VfsFile};

/// First superblock copy. It occupies page 0.
pub const COPY_A: PageId = PageId::new(0);

/// Second superblock copy. It occupies page 1.
pub const COPY_B: PageId = PageId::new(1);

/// Superblock major version written by [`Superblock::encode`].
pub const FORMAT_MAJOR: u16 = 1;

/// Superblock minor version written by [`Superblock::encode`].
pub const FORMAT_MINOR: u16 = 0;

const FILE_ID_LEN: usize = 16;
const FILE_ID_OFFSET: usize = HEADER_SIZE;
const MAJOR_OFFSET: usize = FILE_ID_OFFSET + FILE_ID_LEN;
const MINOR_OFFSET: usize = MAJOR_OFFSET + 2;
const PAGE_SIZE_OFFSET: usize = MINOR_OFFSET + 2;
const GENERATION_OFFSET: usize = PAGE_SIZE_OFFSET + 4;
const CHECKPOINT_LSN_OFFSET: usize = GENERATION_OFFSET + 8;
const LOGICAL_PAGE_COUNT_OFFSET: usize = CHECKPOINT_LSN_OFFSET + 8;
const ALLOCATOR_PAGE_OFFSET: usize = LOGICAL_PAGE_COUNT_OFFSET + 8;
const FREE_SPACE_MAP_OFFSET: usize = ALLOCATOR_PAGE_OFFSET + 8;
const BTREE_ROOT_OFFSET: usize = FREE_SPACE_MAP_OFFSET + 8;

/// First byte after the superblock body. Later bytes in the page are zero.
pub const BODY_END: usize = BTREE_ROOT_OFFSET + 8;

const _: () = assert!(BODY_END == 100);
const _: () = assert!(BODY_END < PAGE_SIZE);

/// Failure while reading or writing a superblock page.
///
/// # Examples
///
/// ```ignore
/// use pizzlydb_storage::page::PAGE_SIZE;
/// use pizzlydb_storage::superblock::{Superblock, SuperblockError};
///
/// let err = Superblock::select(&[0; PAGE_SIZE], &[0; PAGE_SIZE]).unwrap_err();
/// assert!(matches!(err, SuperblockError::BadMagic));
/// ```
#[derive(Debug, Error)]
pub enum SuperblockError {
    /// The page image is shorter than one page.
    #[error("torn superblock")]
    Torn,
    /// The page checksum does not match the image.
    #[error("superblock checksum mismatch")]
    ChecksumMismatch,
    /// The major, minor, or page size is not the one this crate writes.
    #[error("unsupported superblock version")]
    UnsupportedVersion,
    /// The page does not start with the page magic.
    #[error("bad superblock magic")]
    BadMagic,
    /// A field breaks a superblock invariant, or the page is not a superblock.
    #[error("corrupt superblock")]
    Corrupt,
    /// The file handle returned an error.
    #[error("superblock i/o failed")]
    Io(#[source] VfsError),
}

impl From<VfsError> for SuperblockError {
    fn from(err: VfsError) -> Self {
        match err {
            VfsError::Truncated => Self::Torn,
            other => Self::Io(other),
        }
    }
}

/// Engine facts stored in one superblock copy.
///
/// # Examples
///
/// ```ignore
/// use pizzlydb_storage::page::{Lsn, PageId};
/// use pizzlydb_storage::superblock::{Superblock, COPY_A, FORMAT_MAJOR};
///
/// let record = Superblock::new(
///     [1; 16],
///     1,
///     Lsn::new(0),
///     2,
///     PageId::new(0),
///     PageId::new(0),
///     PageId::new(0),
/// )
/// .unwrap();
///
/// let page = record.encode(COPY_A).unwrap();
/// let decoded = Superblock::decode(&page, COPY_A).unwrap();
///
/// assert_eq!(decoded.generation(), 1);
/// assert_eq!(decoded.file_id(), [1; 16]);
/// assert_eq!(FORMAT_MAJOR, 1);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Superblock {
    file_id: [u8; FILE_ID_LEN],
    generation: u64,
    checkpoint_lsn: Lsn,
    logical_page_count: u64,
    allocator_page_id: PageId,
    free_space_map_page_id: PageId,
    btree_root_page_id: PageId,
}

impl Superblock {
    /// Build a record whose fields satisfy the version-1 invariants.
    ///
    /// # Errors
    ///
    /// Returns [`SuperblockError::Corrupt`] when `logical_page_count` is below
    /// 2 or a non-zero page id is page 0, page 1, or not below
    /// `logical_page_count`.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use pizzlydb_storage::page::{Lsn, PageId};
    /// use pizzlydb_storage::superblock::{Superblock, SuperblockError};
    ///
    /// let err = Superblock::new(
    ///     [0; 16],
    ///     0,
    ///     Lsn::new(0),
    ///     1,
    ///     PageId::new(0),
    ///     PageId::new(0),
    ///     PageId::new(0),
    /// );
    ///
    /// assert!(matches!(err, Err(SuperblockError::Corrupt)));
    /// ```
    pub fn new(
        file_id: [u8; FILE_ID_LEN],
        generation: u64,
        checkpoint_lsn: Lsn,
        logical_page_count: u64,
        allocator_page_id: PageId,
        free_space_map_page_id: PageId,
        btree_root_page_id: PageId,
    ) -> Result<Self, SuperblockError> {
        let record = Self {
            file_id,
            generation,
            checkpoint_lsn,
            logical_page_count,
            allocator_page_id,
            free_space_map_page_id,
            btree_root_page_id,
        };

        record.check()?;
        Ok(record)
    }

    /// File id stored in the body.
    #[must_use]
    pub const fn file_id(self) -> [u8; FILE_ID_LEN] {
        self.file_id
    }

    /// Generation of this copy. A higher valid generation wins [`Self::select`].
    #[must_use]
    pub const fn generation(self) -> u64 {
        self.generation
    }

    /// Checkpoint LSN published with this generation.
    #[must_use]
    pub const fn checkpoint_lsn(self) -> Lsn {
        self.checkpoint_lsn
    }

    /// Number of pages the checkpoint covers, including the two superblock pages.
    #[must_use]
    pub const fn logical_page_count(self) -> u64 {
        self.logical_page_count
    }

    /// Allocator page, or zero when the slot is unset.
    #[must_use]
    pub const fn allocator_page_id(self) -> PageId {
        self.allocator_page_id
    }

    /// Free-space map page, or zero when the slot is unset.
    #[must_use]
    pub const fn free_space_map_page_id(self) -> PageId {
        self.free_space_map_page_id
    }

    /// B-tree root page, or zero when the slot is unset.
    #[must_use]
    pub const fn btree_root_page_id(self) -> PageId {
        self.btree_root_page_id
    }

    /// Seal this record into the page image for `page_id`.
    ///
    /// `page_id` is [`COPY_A`] or [`COPY_B`]. The other copy is not part of the
    /// returned image.
    ///
    /// # Errors
    ///
    /// Returns [`SuperblockError::Corrupt`] when `page_id` is not 0 or 1 or the
    /// record breaks an invariant.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use pizzlydb_storage::page::{Lsn, PageId};
    /// use pizzlydb_storage::superblock::{Superblock, COPY_A, COPY_B};
    ///
    /// let record = Superblock::new(
    ///     [1; 16],
    ///     3,
    ///     Lsn::new(9),
    ///     4,
    ///     PageId::new(2),
    ///     PageId::new(0),
    ///     PageId::new(0),
    /// ).unwrap();
    ///
    /// let page = record.encode(COPY_A).unwrap();
    ///
    /// assert_eq!(Superblock::decode(&page, COPY_A).unwrap(), record);
    /// assert!(Superblock::decode(&page, COPY_B).is_err());
    /// ```
    pub fn encode(&self, page_id: PageId) -> Result<[u8; PAGE_SIZE], SuperblockError> {
        if page_id != COPY_A && page_id != COPY_B {
            return Err(SuperblockError::Corrupt);
        }

        self.check()?;

        let page_size = u32::try_from(PAGE_SIZE).map_err(|_| SuperblockError::Corrupt)?;
        let mut page = [0u8; PAGE_SIZE];
        page[FILE_ID_OFFSET..FILE_ID_OFFSET + FILE_ID_LEN].copy_from_slice(&self.file_id);
        put_u16(&mut page, MAJOR_OFFSET, FORMAT_MAJOR).map_err(map_codec)?;
        put_u16(&mut page, MINOR_OFFSET, FORMAT_MINOR).map_err(map_codec)?;
        put_u32(&mut page, PAGE_SIZE_OFFSET, page_size).map_err(map_codec)?;
        put_u64(&mut page, GENERATION_OFFSET, self.generation).map_err(map_codec)?;
        put_u64(
            &mut page,
            CHECKPOINT_LSN_OFFSET,
            self.checkpoint_lsn.as_u64(),
        )
        .map_err(map_codec)?;
        put_u64(
            &mut page,
            LOGICAL_PAGE_COUNT_OFFSET,
            self.logical_page_count,
        )
        .map_err(map_codec)?;
        put_u64(
            &mut page,
            ALLOCATOR_PAGE_OFFSET,
            self.allocator_page_id.as_u64(),
        )
        .map_err(map_codec)?;
        put_u64(
            &mut page,
            FREE_SPACE_MAP_OFFSET,
            self.free_space_map_page_id.as_u64(),
        )
        .map_err(map_codec)?;
        put_u64(
            &mut page,
            BTREE_ROOT_OFFSET,
            self.btree_root_page_id.as_u64(),
        )
        .map_err(map_codec)?;

        PageHeader::new(PageKind::Superblock, page_id, self.checkpoint_lsn)
            .encode(&mut page)
            .map_err(map_codec)?;

        Ok(page)
    }

    /// Decode one sealed copy.
    ///
    /// `expected_id` is the page id the caller read. It must be [`COPY_A`] or
    /// [`COPY_B`] and it must match the page header.
    ///
    /// # Errors
    ///
    /// Returns [`SuperblockError::Torn`] when `page` is not one page,
    /// [`SuperblockError::ChecksumMismatch`] when the page checksum fails,
    /// [`SuperblockError::BadMagic`] when the magic is wrong,
    /// [`SuperblockError::UnsupportedVersion`] when the page header version or
    /// the superblock major, minor, or page size is not accepted, and
    /// [`SuperblockError::Corrupt`] when the kind, page id, tail, or root slots
    /// are wrong.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use pizzlydb_storage::page::PAGE_SIZE;
    /// use pizzlydb_storage::superblock::{Superblock, SuperblockError, COPY_A};
    ///
    /// assert!(matches!(
    ///     Superblock::decode(&[0; 4], COPY_A),
    ///     Err(SuperblockError::Torn)
    /// ));
    ///
    /// assert!(matches!(
    ///     Superblock::decode(&[0; PAGE_SIZE], COPY_A),
    ///     Err(SuperblockError::BadMagic)
    /// ));
    /// ```
    pub fn decode(page: &[u8], expected_id: PageId) -> Result<Self, SuperblockError> {
        if page.len() != PAGE_SIZE {
            return Err(SuperblockError::Torn);
        }

        if expected_id != COPY_A && expected_id != COPY_B {
            return Err(SuperblockError::Corrupt);
        }

        let header = PageHeader::decode(page).map_err(map_codec)?;
        if header.kind() != PageKind::Superblock || header.page_id() != expected_id {
            return Err(SuperblockError::Corrupt);
        }

        let major = get_u16(page, MAJOR_OFFSET).map_err(map_codec)?;
        let minor = get_u16(page, MINOR_OFFSET).map_err(map_codec)?;
        let page_size = get_u32(page, PAGE_SIZE_OFFSET).map_err(map_codec)?;
        let expected = u32::try_from(PAGE_SIZE).map_err(|_| SuperblockError::Corrupt)?;

        if major != FORMAT_MAJOR || minor != FORMAT_MINOR || page_size != expected {
            return Err(SuperblockError::UnsupportedVersion);
        }

        let mut file_id = [0u8; FILE_ID_LEN];
        file_id.copy_from_slice(&page[FILE_ID_OFFSET..FILE_ID_OFFSET + FILE_ID_LEN]);

        let checkpoint_lsn = Lsn::new(get_u64(page, CHECKPOINT_LSN_OFFSET).map_err(map_codec)?);
        if header.lsn() != checkpoint_lsn {
            return Err(SuperblockError::Corrupt);
        }

        if page[BODY_END..].iter().any(|&byte| byte != 0) {
            return Err(SuperblockError::Corrupt);
        }

        let record = Self {
            file_id,
            generation: get_u64(page, GENERATION_OFFSET).map_err(map_codec)?,
            checkpoint_lsn,
            logical_page_count: get_u64(page, LOGICAL_PAGE_COUNT_OFFSET).map_err(map_codec)?,
            allocator_page_id: PageId::new(
                get_u64(page, ALLOCATOR_PAGE_OFFSET).map_err(map_codec)?,
            ),
            free_space_map_page_id: PageId::new(
                get_u64(page, FREE_SPACE_MAP_OFFSET).map_err(map_codec)?,
            ),
            btree_root_page_id: PageId::new(get_u64(page, BTREE_ROOT_OFFSET).map_err(map_codec)?),
        };

        record.check()?;
        Ok(record)
    }

    /// Choose the higher valid generation.
    ///
    /// When both copies are valid and their generations are equal, page 0 wins.
    /// When both fail, the error from page 0 is returned.
    ///
    /// # Errors
    ///
    /// Returns the page-0 error when neither copy decodes.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use pizzlydb_storage::page::{Lsn, PageId};
    /// use pizzlydb_storage::superblock::{Superblock, COPY_A, COPY_B};
    ///
    /// let older = Superblock::new(
    ///     [1; 16],
    ///     1,
    ///     Lsn::new(0),
    ///     2,
    ///     PageId::new(0),
    ///     PageId::new(0),
    ///     PageId::new(0),
    /// ).unwrap();
    ///
    /// let newer = Superblock::new(
    ///     [1; 16],
    ///     2,
    ///     Lsn::new(5),
    ///     2,
    ///     PageId::new(0),
    ///     PageId::new(0),
    ///     PageId::new(0),
    /// ).unwrap();
    ///
    /// let page_a = older.encode(COPY_A).unwrap();
    /// let page_b = newer.encode(COPY_B).unwrap();
    /// let (selected, id) = Superblock::select(&page_a, &page_b).unwrap();
    ///
    /// assert_eq!(id, COPY_B);
    /// assert_eq!(selected.generation(), 2);
    ///
    /// let mut damaged = page_b;
    /// damaged[40] ^= 0xff;
    /// let (selected, id) = Superblock::select(&page_a, &damaged).unwrap();
    ///
    /// assert_eq!(id, COPY_A);
    /// assert_eq!(selected.generation(), 1);
    /// ```
    pub fn select(first: &[u8], second: &[u8]) -> Result<(Self, PageId), SuperblockError> {
        match (Self::decode(first, COPY_A), Self::decode(second, COPY_B)) {
            (Ok(copy_a), Ok(copy_b)) => {
                if copy_b.generation() > copy_a.generation() {
                    Ok((copy_b, COPY_B))
                } else {
                    Ok((copy_a, COPY_A))
                }
            }
            (Ok(copy_a), Err(_)) => Ok((copy_a, COPY_A)),
            (Err(_), Ok(copy_b)) => Ok((copy_b, COPY_B)),
            (Err(err), Err(_)) => Err(err),
        }
    }

    /// Read page 0 and page 1 and select the higher valid generation.
    ///
    /// # Errors
    ///
    /// Returns [`SuperblockError::Torn`] when either page is short, and
    /// [`Self::select`]'s error when neither copy is valid.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::page::{Lsn, PageId};
    /// use pizzlydb_storage::superblock::{Superblock, COPY_A, COPY_B};
    /// use pizzlydb_storage::vfs::{FileRole, MemoryVfs, OpenOptions, Vfs};
    ///
    /// let vfs = MemoryVfs::new();
    /// let mut file = vfs.open(
    ///     Path::new("db"),
    ///     FileRole::Main,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// let older = Superblock::new(
    ///     [1; 16],
    ///     1,
    ///     Lsn::new(0),
    ///     2,
    ///     PageId::new(0),
    ///     PageId::new(0),
    ///     PageId::new(0),
    /// ).unwrap();
    ///
    /// let newer = Superblock::new(
    ///     [1; 16],
    ///     4,
    ///     Lsn::new(8),
    ///     2,
    ///     PageId::new(0),
    ///     PageId::new(0),
    ///     PageId::new(0),
    /// ).unwrap();
    ///
    /// older.write(&mut file, COPY_A).unwrap();
    /// newer.write(&mut file, COPY_B).unwrap();
    /// let (opened, id) = Superblock::open(&mut file).unwrap();
    ///
    /// assert_eq!(id, COPY_B);
    /// assert_eq!(opened.checkpoint_lsn(), Lsn::new(8));
    /// ```
    pub fn open(file: &mut impl VfsFile) -> Result<(Self, PageId), SuperblockError> {
        let mut first = [0u8; PAGE_SIZE];
        let mut second = [0u8; PAGE_SIZE];
        file.read_exact(0, &mut first)?;
        file.read_exact(page_offset(COPY_B)?, &mut second)?;

        Self::select(&first, &second)
    }

    /// Write this record at `page_id` without modifying the other copy.
    ///
    /// The write is not synced.
    ///
    /// # Errors
    ///
    /// Returns [`SuperblockError::Corrupt`] when `page_id` is not 0 or 1, and
    /// [`SuperblockError::Io`] when the write fails.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::page::{Lsn, PageId};
    /// use pizzlydb_storage::superblock::{Superblock, COPY_A, COPY_B};
    /// use pizzlydb_storage::vfs::{FileRole, MemoryVfs, OpenOptions, Vfs, VfsFile};
    ///
    /// let vfs = MemoryVfs::new();
    /// let mut file = vfs.open(
    ///     Path::new("db"),
    ///     FileRole::Main,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// let first = Superblock::new(
    ///     [1; 16],
    ///     1,
    ///     Lsn::new(0),
    ///     2,
    ///     PageId::new(0),
    ///     PageId::new(0),
    ///     PageId::new(0),
    /// ).unwrap();
    ///
    /// let second = Superblock::new(
    ///     [1; 16],
    ///     2,
    ///     Lsn::new(3),
    ///     2,
    ///     PageId::new(0),
    ///     PageId::new(0),
    ///     PageId::new(0),
    /// ).unwrap();
    ///
    /// first.write(&mut file, COPY_A).unwrap();
    /// second.write(&mut file, COPY_B).unwrap();
    /// let mut page = [0; pizzlydb_storage::page::PAGE_SIZE];
    /// file.read_exact(0, &mut page).unwrap();
    ///
    /// assert_eq!(page, first.encode(COPY_A).unwrap());
    /// ```
    pub fn write(&self, file: &mut impl VfsFile, page_id: PageId) -> Result<(), SuperblockError> {
        let image = self.encode(page_id)?;
        file.write_all(page_offset(page_id)?, &image)?;
        Ok(())
    }

    /// The copy that is not `page_id`.
    ///
    /// A checkpoint writes this copy and leaves `page_id` unchanged until the
    /// new copy is sealed.
    ///
    /// # Errors
    ///
    /// Returns [`SuperblockError::Corrupt`] when `page_id` is not 0 or 1.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use pizzlydb_storage::superblock::{Superblock, SuperblockError, COPY_A, COPY_B};
    ///
    /// assert_eq!(Superblock::other_copy(COPY_A).unwrap(), COPY_B);
    /// assert_eq!(Superblock::other_copy(COPY_B).unwrap(), COPY_A);
    /// assert!(matches!(
    ///     Superblock::other_copy(pizzlydb_storage::page::PageId::new(2)),
    ///     Err(SuperblockError::Corrupt)
    /// ));
    /// ```
    pub fn other_copy(page_id: PageId) -> Result<PageId, SuperblockError> {
        match page_id.as_u64() {
            0 => Ok(COPY_B),
            1 => Ok(COPY_A),
            _ => Err(SuperblockError::Corrupt),
        }
    }

    fn check(self) -> Result<(), SuperblockError> {
        if self.logical_page_count < 2 {
            return Err(SuperblockError::Corrupt);
        }

        for id in [
            self.allocator_page_id,
            self.free_space_map_page_id,
            self.btree_root_page_id,
        ] {
            if !slot_ok(id, self.logical_page_count) {
                return Err(SuperblockError::Corrupt);
            }
        }

        Ok(())
    }
}

fn slot_ok(id: PageId, logical_page_count: u64) -> bool {
    let raw = id.as_u64();
    raw == 0 || (raw >= 2 && raw < logical_page_count)
}

fn page_offset(page_id: PageId) -> Result<u64, SuperblockError> {
    let page_size = u64::try_from(PAGE_SIZE).map_err(|_| SuperblockError::Corrupt)?;
    page_id
        .as_u64()
        .checked_mul(page_size)
        .ok_or(SuperblockError::Corrupt)
}

fn map_codec(err: CodecError) -> SuperblockError {
    match err {
        CodecError::Truncated => SuperblockError::Torn,
        CodecError::ChecksumMismatch => SuperblockError::ChecksumMismatch,
        CodecError::UnsupportedVersion => SuperblockError::UnsupportedVersion,
        CodecError::BadMagic => SuperblockError::BadMagic,
        CodecError::BadKind => SuperblockError::Corrupt,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        ALLOCATOR_PAGE_OFFSET, BODY_END, BTREE_ROOT_OFFSET, CHECKPOINT_LSN_OFFSET, COPY_A, COPY_B,
        FORMAT_MAJOR, FORMAT_MINOR, LOGICAL_PAGE_COUNT_OFFSET, MAJOR_OFFSET, MINOR_OFFSET,
        PAGE_SIZE_OFFSET, Superblock, SuperblockError,
    };
    use crate::codec::{put_u16, put_u32, put_u64};
    use crate::page::{Lsn, PAGE_SIZE, PageHeader, PageId, PageKind, seal};
    use crate::vfs::{FileRole, OpenOptions, Vfs, VfsFile};
    use crate::vfs_memory::MemoryVfs;

    fn record(generation: u64, lsn: u64) -> Superblock {
        Superblock::new(
            [1; 16],
            generation,
            Lsn::new(lsn),
            8,
            PageId::new(2),
            PageId::new(3),
            PageId::new(4),
        )
        .unwrap()
    }

    fn reseal(page: &mut [u8]) {
        seal(page).unwrap();
    }

    #[test]
    fn example_roundtrip_and_rejected_page_count() {
        let stored = Superblock::new(
            [1; 16],
            1,
            Lsn::new(0),
            2,
            PageId::new(0),
            PageId::new(0),
            PageId::new(0),
        )
        .unwrap();
        let page = stored.encode(COPY_A).unwrap();
        assert_eq!(Superblock::decode(&page, COPY_A).unwrap(), stored);
        assert_eq!(FORMAT_MAJOR, 1);
        assert_eq!(stored.file_id(), [1; 16]);
        assert_eq!(stored.generation(), 1);

        let err = Superblock::new(
            [0; 16],
            0,
            Lsn::new(0),
            1,
            PageId::new(0),
            PageId::new(0),
            PageId::new(0),
        );
        assert!(matches!(err, Err(SuperblockError::Corrupt)));
    }

    #[test]
    fn example_encode_checks_the_page_id() {
        let stored = Superblock::new(
            [1; 16],
            3,
            Lsn::new(9),
            4,
            PageId::new(2),
            PageId::new(0),
            PageId::new(0),
        )
        .unwrap();
        let page = stored.encode(COPY_A).unwrap();
        assert_eq!(Superblock::decode(&page, COPY_A).unwrap(), stored);
        assert!(Superblock::decode(&page, COPY_B).is_err());
        assert_eq!(stored.allocator_page_id(), PageId::new(2));
        assert_eq!(stored.free_space_map_page_id(), PageId::new(0));
        assert_eq!(stored.btree_root_page_id(), PageId::new(0));
        assert_eq!(stored.logical_page_count(), 4);
        assert_eq!(stored.checkpoint_lsn(), Lsn::new(9));
    }

    #[test]
    fn example_decode_rejects_a_short_page_and_bad_magic() {
        assert!(matches!(
            Superblock::decode(&[0; 4], COPY_A),
            Err(SuperblockError::Torn)
        ));
        assert!(matches!(
            Superblock::decode(&[0; PAGE_SIZE], COPY_A),
            Err(SuperblockError::BadMagic)
        ));
    }

    #[test]
    fn example_select_prefers_the_higher_valid_generation() {
        let older = Superblock::new(
            [1; 16],
            1,
            Lsn::new(0),
            2,
            PageId::new(0),
            PageId::new(0),
            PageId::new(0),
        )
        .unwrap();
        let newer = Superblock::new(
            [1; 16],
            2,
            Lsn::new(5),
            2,
            PageId::new(0),
            PageId::new(0),
            PageId::new(0),
        )
        .unwrap();
        let page_a = older.encode(COPY_A).unwrap();
        let page_b = newer.encode(COPY_B).unwrap();
        let (selected, id) = Superblock::select(&page_a, &page_b).unwrap();
        assert_eq!(id, COPY_B);
        assert_eq!(selected.generation(), 2);

        let mut damaged = page_b;
        damaged[40] ^= 0xff;
        let (selected, id) = Superblock::select(&page_a, &damaged).unwrap();
        assert_eq!(id, COPY_A);
        assert_eq!(selected.generation(), 1);
    }

    #[test]
    fn example_open_reads_both_copies() {
        let vfs = MemoryVfs::new();
        let mut file = vfs
            .open(
                Path::new("db"),
                FileRole::Main,
                &OpenOptions::new().create(true),
            )
            .unwrap();
        let older = Superblock::new(
            [1; 16],
            1,
            Lsn::new(0),
            2,
            PageId::new(0),
            PageId::new(0),
            PageId::new(0),
        )
        .unwrap();
        let newer = Superblock::new(
            [1; 16],
            4,
            Lsn::new(8),
            2,
            PageId::new(0),
            PageId::new(0),
            PageId::new(0),
        )
        .unwrap();
        older.write(&mut file, COPY_A).unwrap();
        newer.write(&mut file, COPY_B).unwrap();
        let (opened, id) = Superblock::open(&mut file).unwrap();
        assert_eq!(id, COPY_B);
        assert_eq!(opened.checkpoint_lsn(), Lsn::new(8));
    }

    #[test]
    fn example_write_leaves_the_other_copy_in_place() {
        let vfs = MemoryVfs::new();
        let mut file = vfs
            .open(
                Path::new("db"),
                FileRole::Main,
                &OpenOptions::new().create(true),
            )
            .unwrap();
        let first = Superblock::new(
            [1; 16],
            1,
            Lsn::new(0),
            2,
            PageId::new(0),
            PageId::new(0),
            PageId::new(0),
        )
        .unwrap();
        let second = Superblock::new(
            [1; 16],
            2,
            Lsn::new(3),
            2,
            PageId::new(0),
            PageId::new(0),
            PageId::new(0),
        )
        .unwrap();
        first.write(&mut file, COPY_A).unwrap();
        second.write(&mut file, COPY_B).unwrap();
        let mut page = [0; PAGE_SIZE];
        file.read_exact(0, &mut page).unwrap();
        assert_eq!(page, first.encode(COPY_A).unwrap());
    }

    #[test]
    fn example_other_copy_flips_the_two_slots() {
        assert_eq!(Superblock::other_copy(COPY_A).unwrap(), COPY_B);
        assert_eq!(Superblock::other_copy(COPY_B).unwrap(), COPY_A);
        assert!(matches!(
            Superblock::other_copy(PageId::new(2)),
            Err(SuperblockError::Corrupt)
        ));
    }

    #[test]
    fn equal_generations_keep_page_zero() {
        let page_a = record(4, 1).encode(COPY_A).unwrap();
        let page_b = record(4, 9).encode(COPY_B).unwrap();
        let (selected, id) = Superblock::select(&page_a, &page_b).unwrap();
        assert_eq!(id, COPY_A);
        assert_eq!(selected.checkpoint_lsn(), Lsn::new(1));
    }

    #[test]
    fn zero_valid_copies_returns_the_page_zero_error() {
        let err = Superblock::select(&[0; PAGE_SIZE], &[0; PAGE_SIZE]).unwrap_err();
        assert!(matches!(err, SuperblockError::BadMagic));
    }

    #[test]
    fn one_damaged_copy_still_opens() {
        let vfs = MemoryVfs::new();
        let mut file = vfs
            .open(
                Path::new("db"),
                FileRole::Main,
                &OpenOptions::new().create(true),
            )
            .unwrap();
        let older = record(1, 0);
        let newer = record(2, 6);
        older.write(&mut file, COPY_A).unwrap();
        newer.write(&mut file, COPY_B).unwrap();
        let mut damaged = newer.encode(COPY_B).unwrap();
        damaged[80] ^= 0xff;
        file.write_all(u64::try_from(PAGE_SIZE).unwrap(), &damaged)
            .unwrap();

        let (opened, id) = Superblock::open(&mut file).unwrap();
        assert_eq!(id, COPY_A);
        assert_eq!(opened.generation(), 1);
    }

    #[test]
    fn a_foreign_major_minor_or_page_size_is_rejected() {
        let mut page = record(1, 0).encode(COPY_A).unwrap();
        put_u16(&mut page, MAJOR_OFFSET, 2).unwrap();
        reseal(&mut page);
        assert!(matches!(
            Superblock::decode(&page, COPY_A),
            Err(SuperblockError::UnsupportedVersion)
        ));

        let mut page = record(1, 0).encode(COPY_A).unwrap();
        put_u16(&mut page, MINOR_OFFSET, FORMAT_MINOR + 1).unwrap();
        reseal(&mut page);
        assert!(matches!(
            Superblock::decode(&page, COPY_A),
            Err(SuperblockError::UnsupportedVersion)
        ));

        let mut page = record(1, 0).encode(COPY_A).unwrap();
        put_u32(&mut page, PAGE_SIZE_OFFSET, 4096).unwrap();
        reseal(&mut page);
        assert!(matches!(
            Superblock::decode(&page, COPY_A),
            Err(SuperblockError::UnsupportedVersion)
        ));
    }

    #[test]
    fn roots_and_tail_bytes_are_checked_after_the_checksum() {
        let mut page = record(1, 4).encode(COPY_A).unwrap();
        put_u64(&mut page, LOGICAL_PAGE_COUNT_OFFSET, 1).unwrap();
        reseal(&mut page);
        assert!(matches!(
            Superblock::decode(&page, COPY_A),
            Err(SuperblockError::Corrupt)
        ));

        let mut page = record(1, 4).encode(COPY_A).unwrap();
        put_u64(&mut page, BTREE_ROOT_OFFSET, 1).unwrap();
        reseal(&mut page);
        assert!(matches!(
            Superblock::decode(&page, COPY_A),
            Err(SuperblockError::Corrupt)
        ));

        let mut page = record(1, 4).encode(COPY_A).unwrap();
        put_u64(&mut page, CHECKPOINT_LSN_OFFSET, 99).unwrap();
        reseal(&mut page);
        assert!(matches!(
            Superblock::decode(&page, COPY_A),
            Err(SuperblockError::Corrupt)
        ));

        let mut page = record(1, 4).encode(COPY_A).unwrap();
        page[BODY_END] = 1;
        reseal(&mut page);
        assert!(matches!(
            Superblock::decode(&page, COPY_A),
            Err(SuperblockError::Corrupt)
        ));

        let mut page = record(1, 4).encode(COPY_A).unwrap();
        page[ALLOCATOR_PAGE_OFFSET] ^= 0x01;
        assert!(matches!(
            Superblock::decode(&page, COPY_A),
            Err(SuperblockError::ChecksumMismatch)
        ));
        assert_eq!(BODY_END, 100);
        assert!(page[BODY_END..].iter().all(|&byte| byte == 0));
    }

    #[test]
    fn a_non_superblock_page_is_corrupt() {
        let mut page = [0u8; PAGE_SIZE];
        PageHeader::new(PageKind::BTreeLeaf, COPY_A, Lsn::new(0))
            .encode(&mut page)
            .unwrap();
        assert!(matches!(
            Superblock::decode(&page, COPY_A),
            Err(SuperblockError::Corrupt)
        ));
    }

    #[test]
    fn body_fields_roundtrip_including_the_free_space_map() {
        let stored = record(7, 11);
        let page = stored.encode(COPY_B).unwrap();
        let decoded = Superblock::decode(&page, COPY_B).unwrap();
        assert_eq!(decoded, stored);
        assert_eq!(decoded.free_space_map_page_id(), PageId::new(3));
        assert!(page[BODY_END..].iter().all(|&byte| byte == 0));
    }
}
