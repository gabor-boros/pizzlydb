//! Page-image write-ahead log.
//!
//! A frame is a page image or a commit record. A page image stays pending until
//! a later commit record with a valid checksum names that transaction and the
//! same number of pages. [`recover`] applies those images and stops at the first
//! frame that is short, fails its checksum, or has an unknown kind. Bytes at and
//! after that frame are not applied.
//!
//! This is the physical log. It does not store graph mutations.
//!
//! ## Header
//!
//! The first [`HEADER_SIZE`] bytes are version [`VERSION`]:
//!
//! - `0..4`: magic [`MAGIC`] (`PZLW`)
//! - `4..6`: version as `u16`
//! - `6..8`: reserved `u16`, written as zero
//! - `8..12`: page size as `u32`, [`PAGE_SIZE`](crate::page::PAGE_SIZE)
//! - `12..28`: file id, [`FILE_ID_LEN`] bytes
//! - `28..32`: CRC-32C of the header, with this field treated as zeros
//! - `32..64`: reserved zeros
//!
//! Multi-byte integers are little-endian. The checksum is [`Checksum`].
//!
//! ## Frame
//!
//! Each frame is a 12-byte header plus a payload:
//!
//! - `0`: kind, `1` for a page image and `2` for a commit
//! - `1..4`: reserved zeros
//! - `4..8`: payload length as `u32`
//! - `8..12`: CRC-32C of the header and the payload, with this field treated as zeros
//!
//! A payload longer than [`MAX_PAYLOAD`], or a length that runs past the buffer,
//! ends the scan. The page bytes are copied only after the frame checksum matches.
//!
//! A page-image payload is `txn_id`, `page_id`, `lsn`, then one sealed page.
//! `txn_id` and `lsn` are non-zero. The page header's page id and LSN match the
//! payload.
//!
//! A commit payload is `txn_id`, `lsn`, and `page_count`. `page_count` is the
//! number of page images that commit covers. It is applied when that many pending
//! images for `txn_id` share `lsn`, use distinct page ids, and `lsn` is greater
//! than the last applied commit. The file's logical page count is stored in the
//! superblock, not in this field.
//!
//! ## Durability
//!
//! [`Durability::Full`] writes the commit frame, syncs the file, and only then
//! records the commit. A failed sync returns the error and leaves
//! [`Wal::durable_lsn`] unchanged. [`Durability::None`] records the commit and
//! does not sync. A crash can drop that commit. A later [`Wal::sync`] makes the
//! bytes written so far durable, including a commit recorded as `None`.
//!
//! [`Wal`] does not synchronize itself. Dropping a [`Wal`] drops the file handle
//! and does not sync. [`WalError`] is the log's own error.

use thiserror::Error;

use crate::checksum::Checksum;
use crate::codec::{CodecError, get_u64, put_u32, put_u64};
use crate::page::{Lsn, PAGE_SIZE, PageHeader, PageId};
use crate::vfs::{VfsError, VfsFile};

/// File bytes `PZLW`, in that order.
pub const MAGIC: [u8; 4] = *b"PZLW";

/// WAL header version written and accepted by this crate.
pub const VERSION: u16 = 1;

/// Length of the WAL header.
pub const HEADER_SIZE: usize = 64;

/// Length of the file id stored in the header.
pub const FILE_ID_LEN: usize = 16;

/// Length of a frame header. The payload starts at this offset.
pub const FRAME_HEADER_SIZE: usize = 12;

/// Largest accepted frame payload, including one page image.
pub const MAX_PAYLOAD: usize = PAGE_SIZE + 64;

const MAGIC_OFFSET: usize = 0;
const VERSION_OFFSET: usize = 4;
const HEADER_RESERVED: usize = 6;
const PAGE_SIZE_OFFSET: usize = 8;
const FILE_ID_OFFSET: usize = 12;
const HEADER_CHECKSUM: usize = 28;
const HEADER_TAIL: usize = 32;
const CRC_LEN: usize = 4;

const FRAME_KIND: usize = 0;
const FRAME_RESERVED: usize = 1;
const FRAME_LENGTH: usize = 4;
const FRAME_CHECKSUM: usize = 8;

const KIND_PAGE_IMAGE: u8 = 1;
const KIND_COMMIT: u8 = 2;

const IMAGE_PREFIX: usize = 24;
const IMAGE_PAYLOAD_LEN: usize = IMAGE_PREFIX + PAGE_SIZE;
const COMMIT_PAYLOAD_LEN: usize = 24;

const _: () = assert!(HEADER_SIZE == 64);
const _: () = assert!(FILE_ID_OFFSET + FILE_ID_LEN == HEADER_CHECKSUM);
const _: () = assert!(HEADER_TAIL == 32);
const _: () = assert!(FRAME_HEADER_SIZE == 12);
const _: () = assert!(IMAGE_PAYLOAD_LEN <= MAX_PAYLOAD);
const _: () = assert!(COMMIT_PAYLOAD_LEN <= MAX_PAYLOAD);

/// Failure while reading or writing a write-ahead log.
///
/// A short header fails [`recover`]. A later torn frame does not: [`recover`]
/// returns the commits that were complete before that frame.
///
/// # Examples
///
/// ```ignore
/// use pizzlydb_storage::wal::{recover, WalError};
///
/// assert!(matches!(recover(&[]), Err(WalError::Torn)));
/// ```
#[derive(Debug, Error)]
pub enum WalError {
    /// The header or a frame ended before a full record.
    #[error("torn write-ahead log record")]
    Torn,
    /// A stored CRC-32C does not match the bytes it covers.
    #[error("write-ahead log checksum mismatch")]
    ChecksumMismatch,
    /// The header version or page size is not the one this crate writes.
    #[error("unsupported write-ahead log version")]
    UnsupportedVersion,
    /// The header does not start with [`MAGIC`].
    #[error("bad write-ahead log magic")]
    BadMagic,
    /// A frame or header field breaks a log invariant.
    #[error("corrupt write-ahead log record")]
    Corrupt,
    /// The header file id is not the id passed to [`Wal::open`].
    #[error("write-ahead log file id mismatch")]
    FileIdMismatch,
    /// The file handle returned an error.
    #[error("write-ahead log i/o failed")]
    Io(#[source] VfsError),
}

impl From<VfsError> for WalError {
    fn from(err: VfsError) -> Self {
        match err {
            VfsError::Truncated => Self::Torn,
            other => Self::Io(other),
        }
    }
}

/// Sync requested for one commit.
///
/// There is no group-commit variant. [`Self::Full`] syncs this commit.
/// [`Self::None`] does not.
///
/// # Examples
///
/// ```ignore
/// use pizzlydb_storage::wal::Durability;
///
/// let durable = Durability::Full;
/// let volatile = Durability::None;
///
/// assert_eq!(durable, Durability::Full);
/// assert_ne!(durable, volatile);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Durability {
    /// Sync the log before recording the commit.
    Full,
    /// Record the commit and leave it unsynced.
    None,
}

/// One decoded frame whose checksum and payload shape have been checked.
///
/// Sequence rules, including which images belong to a commit, are applied by
/// [`recover`] and [`Wal::commit`].
///
/// # Examples
///
/// ```ignore
/// use pizzlydb_storage::page::Lsn;
/// use pizzlydb_storage::wal::WalFrame;
///
/// let frame = WalFrame::Commit {
///     txn_id: 1,
///     lsn: Lsn::new(1),
///     page_count: 1,
/// };
///
/// assert!(matches!(
///     frame,
///     WalFrame::Commit {
///         txn_id: 1,
///         page_count: 1,
///         ..
///     }
/// ));
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WalFrame {
    /// A full page image for `txn_id` at `lsn`.
    PageImage {
        /// Caller-chosen transaction id. Zero is not written.
        txn_id: u64,
        /// Page the image claims to be.
        page_id: PageId,
        /// Commit LSN the image belongs to.
        lsn: Lsn,
        /// Sealed page image of [`PAGE_SIZE`](crate::page::PAGE_SIZE) bytes.
        image: Vec<u8>,
    },
    /// Makes the pending images of `txn_id` visible.
    Commit {
        /// Transaction whose pending images this record covers.
        txn_id: u64,
        /// LSN shared by those images.
        lsn: Lsn,
        /// Number of page images this commit covers.
        page_count: u64,
    },
}

/// A page image that belongs to an applied commit.
///
/// # Examples
///
/// ```ignore
/// use std::path::Path;
///
/// use pizzlydb_storage::page::{Lsn, PageHeader, PageId, PageKind, PAGE_SIZE};
/// use pizzlydb_storage::vfs::{FileRole, MemoryVfs, OpenOptions, Vfs};
/// use pizzlydb_storage::wal::{Durability, Wal};
///
/// let vfs = MemoryVfs::new();
/// let file = vfs.open(
///     Path::new("db.wal"),
///     FileRole::Wal,
///     &OpenOptions::new().create(true),
/// ).unwrap();
///
/// let mut wal = Wal::create(file, [7; 16]).unwrap();
/// let mut image = [0x11; PAGE_SIZE];
///
/// PageHeader::new(PageKind::BTreeLeaf, PageId::new(4), Lsn::new(1))
///     .encode(&mut image)
///     .unwrap();
///
/// wal.append_page_image(1, PageId::new(4), Lsn::new(1), &image)
///     .unwrap();
///
/// wal.commit(Durability::Full, 1, Lsn::new(1), 1).unwrap();
///
/// let page = &wal.pages()[0];
/// assert_eq!(page.txn_id(), 1);
/// assert_eq!(page.page_id(), PageId::new(4));
/// assert_eq!(page.lsn(), Lsn::new(1));
/// assert_eq!(page.image(), &image);
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommittedPage {
    txn_id: u64,
    page_id: PageId,
    lsn: Lsn,
    image: Vec<u8>,
}

impl CommittedPage {
    /// Transaction that committed this image.
    #[must_use]
    pub const fn txn_id(&self) -> u64 {
        self.txn_id
    }

    /// Page id stored in the frame and in the page header.
    #[must_use]
    pub const fn page_id(&self) -> PageId {
        self.page_id
    }

    /// Commit LSN of this image.
    #[must_use]
    pub const fn lsn(&self) -> Lsn {
        self.lsn
    }

    /// Sealed page bytes.
    #[must_use]
    pub fn image(&self) -> &[u8] {
        &self.image
    }
}

/// Commits present in a byte string, and where the next frame would start.
///
/// # Examples
///
/// ```ignore
/// use pizzlydb_storage::wal::{recover, HEADER_SIZE, WalError};
///
/// let err = recover(&[0; HEADER_SIZE - 1]).unwrap_err();
/// assert!(matches!(err, WalError::Torn));
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recovery {
    file_id: [u8; FILE_ID_LEN],
    pages: Vec<CommittedPage>,
    durable_lsn: Lsn,
    valid_end: u64,
}

impl Recovery {
    /// File id stored in the header.
    #[must_use]
    pub const fn file_id(&self) -> [u8; FILE_ID_LEN] {
        self.file_id
    }

    /// Page images from commits that were complete in the byte string.
    #[must_use]
    pub fn pages(&self) -> &[CommittedPage] {
        &self.pages
    }

    /// LSN of the last applied commit, or zero when none was applied.
    #[must_use]
    pub const fn durable_lsn(&self) -> Lsn {
        self.durable_lsn
    }

    /// Offset of the first byte that was not applied.
    ///
    /// A later append starts here. [`recover`] does not return a [`Recovery`]
    /// when the header itself is shorter than [`HEADER_SIZE`].
    #[must_use]
    pub const fn valid_end(&self) -> u64 {
        self.valid_end
    }
}

#[derive(Debug)]
struct PendingPage {
    txn_id: u64,
    page_id: PageId,
    lsn: Lsn,
    image: Vec<u8>,
    offset: u64,
}

/// Append-only page-image log over a caller-owned file handle.
///
/// `create` writes and syncs the header. `open` reads the bytes currently in
/// the file and applies [`recover`]. The handle is not shared. Dropping `Wal`
/// does not sync.
///
/// # Examples
///
/// ```ignore
/// use std::path::Path;
///
/// use pizzlydb_storage::page::Lsn;
/// use pizzlydb_storage::vfs::{FileRole, MemoryVfs, OpenOptions, Vfs};
/// use pizzlydb_storage::wal::Wal;
///
/// let vfs = MemoryVfs::new();
/// let file = vfs.open(
///     Path::new("db.wal"),
///     FileRole::Wal,
///     &OpenOptions::new().create(true),
/// ).unwrap();
///
/// let wal = Wal::create(file, [7; 16]).unwrap();
///
/// assert_eq!(wal.file_id(), [7; 16]);
/// assert_eq!(wal.durable_lsn(), Lsn::new(0));
/// assert!(wal.pages().is_empty());
/// ```
#[derive(Debug)]
pub struct Wal<F: VfsFile> {
    file: F,
    file_id: [u8; FILE_ID_LEN],
    append_offset: u64,
    synced_offset: u64,
    allocated_end: u64,
    durable_lsn: Lsn,
    pages: Vec<CommittedPage>,
    pending: Vec<PendingPage>,
}

impl<F: VfsFile> Wal<F> {
    /// Write a version-[`VERSION`] header and sync it.
    ///
    /// Bytes already stored after the header are overwritten with zeros and
    /// included in that sync. The new log has no commits.
    ///
    /// # Errors
    ///
    /// Returns [`WalError::Io`] when a write or the header sync fails. The
    /// caller does not receive a [`Wal`] in that case.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::page::Lsn;
    /// use pizzlydb_storage::vfs::{FileRole, MemoryVfs, OpenOptions, Vfs};
    /// use pizzlydb_storage::wal::Wal;
    ///
    /// let vfs = MemoryVfs::new();
    /// let file = vfs.open(
    ///     Path::new("db.wal"),
    ///     FileRole::Wal,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// let wal = Wal::create(file, [7; 16]).unwrap();
    ///
    /// assert_eq!(wal.durable_lsn(), Lsn::new(0));
    /// ```
    pub fn create(mut file: F, file_id: [u8; FILE_ID_LEN]) -> Result<Self, WalError> {
        let header = encode_header(&file_id)?;
        file.write_all(0, &header)?;

        let len = file.len()?;
        let header_end = header_end()?;
        if len > header_end {
            zero_between(&mut file, header_end, len)?;
        }

        file.sync()?;

        Ok(Self {
            file,
            file_id,
            append_offset: header_end,
            synced_offset: header_end,
            allocated_end: len.max(header_end),
            durable_lsn: Lsn::new(0),
            pages: Vec::new(),
            pending: Vec::new(),
        })
    }

    /// Read `file` and apply the committed prefix.
    ///
    /// The header must be complete and must match `file_id`. A torn, truncated,
    /// or checksum-invalid frame ends the prefix and does not fail the open.
    /// The next append starts at the end of that prefix. This does not rewrite
    /// the ignored tail.
    ///
    /// # Errors
    ///
    /// Returns [`WalError::Torn`] when the header is short, [`WalError::FileIdMismatch`]
    /// when the stored id differs, and the header errors from [`recover`] when
    /// the header is complete but not a log this crate accepts.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::page::{Lsn, PageHeader, PageId, PageKind, PAGE_SIZE};
    /// use pizzlydb_storage::vfs::{FileRole, MemoryVfs, OpenOptions, Vfs};
    /// use pizzlydb_storage::wal::{Durability, Wal};
    ///
    /// let vfs = MemoryVfs::new();
    /// let path = Path::new("db.wal");
    /// let file = vfs
    ///     .open(path, FileRole::Wal, &OpenOptions::new().create(true))
    ///     .unwrap();
    ///
    /// let mut wal = Wal::create(file, [7; 16]).unwrap();
    /// let mut image = [0x11; PAGE_SIZE];
    ///
    /// PageHeader::new(PageKind::BTreeLeaf, PageId::new(4), Lsn::new(1))
    ///     .encode(&mut image)
    ///     .unwrap();
    ///
    /// wal.append_page_image(1, PageId::new(4), Lsn::new(1), &image)
    ///     .unwrap();
    ///
    /// wal.commit(Durability::Full, 1, Lsn::new(1), 1).unwrap();
    /// drop(wal);
    ///
    /// let file = vfs.open(path, FileRole::Wal, &OpenOptions::new()).unwrap();
    /// let wal = Wal::open(file, [7; 16]).unwrap();
    ///
    /// assert_eq!(wal.durable_lsn(), Lsn::new(1));
    /// assert_eq!(wal.pages()[0].image(), &image);
    /// ```
    pub fn open(mut file: F, file_id: [u8; FILE_ID_LEN]) -> Result<Self, WalError> {
        let bytes = read_file(&mut file)?;

        let recovery = recover(&bytes)?;
        if recovery.file_id != file_id {
            return Err(WalError::FileIdMismatch);
        }

        let allocated_end = u64::try_from(bytes.len()).map_err(|_| WalError::Torn)?;

        Ok(Self {
            file,
            file_id,
            append_offset: recovery.valid_end,
            synced_offset: recovery.valid_end,
            allocated_end,
            durable_lsn: recovery.durable_lsn,
            pages: recovery.pages,
            pending: Vec::new(),
        })
    }

    /// Append a page image. It stays invisible until [`Self::commit`] for `txn_id`.
    ///
    /// `image` must be one sealed page whose header page id is `page_id` and
    /// whose header LSN is `lsn`.
    ///
    /// # Errors
    ///
    /// Returns [`WalError::Corrupt`] when `txn_id` or `lsn` is zero, `lsn` does
    /// not move past the recorded LSN, the page header does not match, or this
    /// transaction already has that page id or a different LSN. Returns
    /// [`WalError::ChecksumMismatch`], [`WalError::BadMagic`],
    /// [`WalError::UnsupportedVersion`], or [`WalError::Torn`] when `image` is
    /// not a sealed page. Returns [`WalError::Io`] when the write fails.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::page::{Lsn, PageHeader, PageId, PageKind, PAGE_SIZE};
    /// use pizzlydb_storage::vfs::{FileRole, MemoryVfs, OpenOptions, Vfs};
    /// use pizzlydb_storage::wal::Wal;
    ///
    /// let vfs = MemoryVfs::new();
    /// let file = vfs.open(
    ///     Path::new("db.wal"),
    ///     FileRole::Wal,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// let mut wal = Wal::create(file, [7; 16]).unwrap();
    /// let mut image = [0x11; PAGE_SIZE];
    ///
    /// PageHeader::new(PageKind::BTreeLeaf, PageId::new(4), Lsn::new(1))
    ///     .encode(&mut image)
    ///     .unwrap();
    ///
    /// wal.append_page_image(1, PageId::new(4), Lsn::new(1), &image)
    ///     .unwrap();
    ///
    /// assert!(wal.pages().is_empty());
    /// assert_eq!(wal.durable_lsn(), Lsn::new(0));
    /// ```
    pub fn append_page_image(
        &mut self,
        txn_id: u64,
        page_id: PageId,
        lsn: Lsn,
        image: &[u8],
    ) -> Result<(), WalError> {
        self.prepare_image(txn_id, page_id, lsn, image)?;

        let payload = image_payload(txn_id, page_id, lsn, image)?;
        let offset = self.write_frame(KIND_PAGE_IMAGE, &payload)?;

        self.pending.push(PendingPage {
            txn_id,
            page_id,
            lsn,
            image: image.to_vec(),
            offset,
        });

        Ok(())
    }

    /// Write a commit record for the pending images of `txn_id`.
    ///
    /// `page_count` is the number of those images. [`Durability::Full`] syncs
    /// before the images are recorded. [`Durability::None`] records them without
    /// syncing. On a failed sync the recorded LSN is unchanged, the append
    /// offset returns to the last synced byte, and the unacknowledged tail is
    /// overwritten with zeros. Those zeros are not synced.
    ///
    /// # Errors
    ///
    /// Returns [`WalError::Corrupt`] when the pending images for `txn_id` are
    /// not exactly `page_count` distinct pages at `lsn`, or when `lsn` does not
    /// move forward. Returns [`WalError::Io`] when the write or, for
    /// [`Durability::Full`], the sync fails.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::page::{Lsn, PageHeader, PageId, PageKind, PAGE_SIZE};
    /// use pizzlydb_storage::vfs::{FileRole, MemoryVfs, OpenOptions, Vfs};
    /// use pizzlydb_storage::wal::{Durability, Wal};
    ///
    /// let vfs = MemoryVfs::new();
    /// let file = vfs.open(
    ///     Path::new("db.wal"),
    ///     FileRole::Wal,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// let mut wal = Wal::create(file, [7; 16]).unwrap();
    /// let mut image = [0x11; PAGE_SIZE];
    ///
    /// PageHeader::new(PageKind::BTreeLeaf, PageId::new(4), Lsn::new(1))
    ///     .encode(&mut image)
    ///     .unwrap();
    ///
    /// wal.append_page_image(1, PageId::new(4), Lsn::new(1), &image)
    ///     .unwrap();
    ///
    /// wal.commit(Durability::Full, 1, Lsn::new(1), 1).unwrap();
    ///
    /// assert_eq!(wal.durable_lsn(), Lsn::new(1));
    /// assert_eq!(wal.pages()[0].image(), &image);
    /// ```
    pub fn commit(
        &mut self,
        durability: Durability,
        txn_id: u64,
        lsn: Lsn,
        page_count: u64,
    ) -> Result<(), WalError> {
        self.prepare_commit(txn_id, lsn, page_count)?;

        let payload = commit_payload(txn_id, lsn, page_count)?;
        self.write_frame(KIND_COMMIT, &payload)?;

        match durability {
            Durability::Full => {
                if let Err(err) = self.sync() {
                    self.discard_unsynced();
                    return Err(err);
                }
            }
            Durability::None => {}
        }

        self.publish(txn_id, lsn);
        Ok(())
    }

    /// Sync the file as it stands.
    ///
    /// This does not write a commit record and does not make pending images
    /// visible. After it returns, a crash keeps the bytes written so far.
    ///
    /// # Errors
    ///
    /// Returns [`WalError::Io`] when the handle's sync fails. The recorded LSN
    /// is unchanged.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::page::{Lsn, PageHeader, PageId, PageKind, PAGE_SIZE};
    /// use pizzlydb_storage::vfs::{FileRole, MemoryVfs, OpenOptions, Vfs};
    /// use pizzlydb_storage::wal::Wal;
    ///
    /// let vfs = MemoryVfs::new();
    /// let file = vfs.open(.open(
    ///     Path::new("db.wal"),
    ///     FileRole::Wal,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// let mut wal = Wal::create(file, [7; 16]).unwrap();
    /// let mut image = [0x11; PAGE_SIZE];
    ///
    /// PageHeader::new(PageKind::BTreeLeaf, PageId::new(4), Lsn::new(1))
    ///     .encode(&mut image)
    ///     .unwrap();
    ///
    /// wal.append_page_image(1, PageId::new(4), Lsn::new(1), &image)
    ///     .unwrap();
    ///
    /// wal.sync().unwrap();
    ///
    /// assert_eq!(wal.durable_lsn(), Lsn::new(0));
    /// assert!(wal.pages().is_empty());
    /// ```
    pub fn sync(&mut self) -> Result<(), WalError> {
        self.file.sync()?;
        self.synced_offset = self.append_offset;
        Ok(())
    }

    /// File id passed to [`Self::create`] or [`Self::open`].
    #[must_use]
    pub const fn file_id(&self) -> [u8; FILE_ID_LEN] {
        self.file_id
    }

    /// Last recorded commit.
    ///
    /// After [`Durability::Full`] this LSN was synced. After [`Durability::None`]
    /// it was recorded in memory and can be lost on crash until a later
    /// [`Self::sync`]. A failed `Full` sync does not change the value.
    #[must_use]
    pub const fn durable_lsn(&self) -> Lsn {
        self.durable_lsn
    }

    /// Images from commits recorded on this handle, in commit order.
    #[must_use]
    pub fn pages(&self) -> &[CommittedPage] {
        &self.pages
    }

    fn prepare_image(
        &self,
        txn_id: u64,
        page_id: PageId,
        lsn: Lsn,
        image: &[u8],
    ) -> Result<(), WalError> {
        if txn_id == 0 || lsn.as_u64() == 0 || lsn.as_u64() <= self.durable_lsn.as_u64() {
            return Err(WalError::Corrupt);
        }

        if self
            .pending
            .iter()
            .any(|page| page.txn_id == txn_id && (page.page_id == page_id || page.lsn != lsn))
        {
            return Err(WalError::Corrupt);
        }

        checked_image(page_id, lsn, image)
    }

    fn prepare_commit(&self, txn_id: u64, lsn: Lsn, page_count: u64) -> Result<(), WalError> {
        if txn_id == 0
            || lsn.as_u64() == 0
            || page_count == 0
            || lsn.as_u64() <= self.durable_lsn.as_u64()
            || !commit_matches(&self.pending, txn_id, lsn, page_count)
        {
            return Err(WalError::Corrupt);
        }

        Ok(())
    }

    fn publish(&mut self, txn_id: u64, lsn: Lsn) {
        let mut kept = Vec::new();

        for page in self.pending.drain(..) {
            if page.txn_id == txn_id {
                self.pages.push(CommittedPage {
                    txn_id: page.txn_id,
                    page_id: page.page_id,
                    lsn: page.lsn,
                    image: page.image,
                });
            } else {
                kept.push(page);
            }
        }

        self.pending = kept;
        self.durable_lsn = lsn;
    }

    fn write_frame(&mut self, kind: u8, payload: &[u8]) -> Result<u64, WalError> {
        let offset = self.append_offset;
        let frame = encode_frame(kind, payload)?;
        self.file.write_all(offset, &frame)?;

        let end = add_len(offset, frame.len())?;
        if self.allocated_end > end {
            self.zero_range(end, self.allocated_end)?;
        } else {
            self.allocated_end = end;
        }
        self.append_offset = end;

        Ok(offset)
    }

    /// Forget bytes written after the last successful sync.
    ///
    /// The append offset returns to that sync. The visible tail is overwritten
    /// with zeros and those zeros are not synced, so a crash keeps the last
    /// synced prefix.
    fn discard_unsynced(&mut self) {
        let end = self.allocated_end.max(self.append_offset);
        let start = self.synced_offset;
        self.append_offset = start;
        self.pending.retain(|page| page.offset < start);
        let _ = self.zero_range(start, end);
    }

    fn zero_range(&mut self, start: u64, end: u64) -> Result<(), WalError> {
        zero_between(&mut self.file, start, end)
    }
}

/// Apply every commit that is fully contained in `bytes`.
///
/// A header shorter than [`HEADER_SIZE`] is [`WalError::Torn`]. A complete header
/// with the wrong magic, version, page size, checksum, or reserved bytes is an
/// error. After a valid header, the first short, checksum-invalid, unknown, or
/// inconsistent frame ends the scan. That frame and everything after it are
/// omitted, and the function returns the commits already applied.
///
/// # Errors
///
/// Returns [`WalError::Torn`], [`WalError::BadMagic`], [`WalError::ChecksumMismatch`],
/// [`WalError::UnsupportedVersion`], or [`WalError::Corrupt`] for the header.
/// A bad frame after a valid header is not an error.
///
/// # Examples
///
/// ```ignore
/// use std::path::Path;
///
/// use pizzlydb_storage::page::{Lsn, PageHeader, PageId, PageKind, PAGE_SIZE};
/// use pizzlydb_storage::vfs::{FileRole, MemoryVfs, OpenOptions, Vfs, VfsFile};
/// use pizzlydb_storage::wal::{recover, Durability, Wal};
///
/// let vfs = MemoryVfs::new();
/// let path = Path::new("db.wal");
/// let file = vfs.open(
///     path,
///     FileRole::Wal,
///     &OpenOptions::new().create(true),
/// ).unwrap();
///
/// let mut wal = Wal::create(file, [7; 16]).unwrap();
/// let mut image = [0x11; PAGE_SIZE];
///
/// PageHeader::new(PageKind::BTreeLeaf, PageId::new(4), Lsn::new(1))
///     .encode(&mut image)
///     .unwrap();
///
/// wal.append_page_image(1, PageId::new(4), Lsn::new(1), &image)
///     .unwrap();
///
/// wal.commit(Durability::None, 1, Lsn::new(1), 1).unwrap();
/// drop(wal);
///
/// let mut file = vfs.open(path, FileRole::Wal, &OpenOptions::new()).unwrap();
/// let len = usize::try_from(file.len().unwrap()).unwrap();
///
/// let mut bytes = vec![0; len];
/// file.read_exact(0, &mut bytes).unwrap();
///
/// assert_eq!(recover(&bytes).unwrap().durable_lsn(), Lsn::new(1));
/// assert!(recover(&bytes[..bytes.len() - 1]).unwrap().pages().is_empty());
/// ```
pub fn recover(bytes: &[u8]) -> Result<Recovery, WalError> {
    let file_id = decode_header(bytes)?;
    let mut pending: Vec<PendingPage> = Vec::new();
    let mut pages = Vec::new();
    let mut durable = 0u64;
    let mut offset = HEADER_SIZE;

    while let Some((frame, len)) = read_frame(&bytes[offset..]) {
        let Some(next) = offset.checked_add(len) else {
            break;
        };

        match frame {
            WalFrame::PageImage {
                txn_id,
                page_id,
                lsn,
                image,
            } => {
                if lsn.as_u64() <= durable
                    || pending
                        .iter()
                        .any(|page| page.txn_id == txn_id && page.page_id == page_id)
                {
                    break;
                }
                pending.push(PendingPage {
                    txn_id,
                    page_id,
                    lsn,
                    image,
                    offset: 0,
                });
                offset = next;
            }
            WalFrame::Commit {
                txn_id,
                lsn,
                page_count,
            } => {
                if lsn.as_u64() <= durable || !commit_matches(&pending, txn_id, lsn, page_count) {
                    break;
                }
                take_commit(&mut pending, &mut pages, txn_id);
                durable = lsn.as_u64();
                offset = next;
            }
        }
    }

    let valid_end = u64::try_from(offset).map_err(|_| WalError::Corrupt)?;
    Ok(Recovery {
        file_id,
        pages,
        durable_lsn: Lsn::new(durable),
        valid_end,
    })
}

fn take_commit(pending: &mut Vec<PendingPage>, pages: &mut Vec<CommittedPage>, txn_id: u64) {
    let mut kept = Vec::new();

    for page in pending.drain(..) {
        if page.txn_id == txn_id {
            pages.push(CommittedPage {
                txn_id: page.txn_id,
                page_id: page.page_id,
                lsn: page.lsn,
                image: page.image,
            });
        } else {
            kept.push(page);
        }
    }

    *pending = kept;
}

fn commit_matches(pending: &[PendingPage], txn_id: u64, lsn: Lsn, page_count: u64) -> bool {
    let mut count = 0u64;
    let mut seen = Vec::new();

    for page in pending {
        if page.txn_id != txn_id {
            continue;
        }
        if page.lsn != lsn || seen.contains(&page.page_id) {
            return false;
        }
        seen.push(page.page_id);
        count = count.saturating_add(1);
    }

    count == page_count
}

fn decode_header(bytes: &[u8]) -> Result<[u8; FILE_ID_LEN], WalError> {
    if bytes.len() < HEADER_SIZE {
        return Err(WalError::Torn);
    }

    let header = &bytes[..HEADER_SIZE];
    if header[MAGIC_OFFSET..MAGIC_OFFSET + MAGIC.len()] != MAGIC {
        return Err(WalError::BadMagic);
    }

    let mut hashed = [0u8; HEADER_SIZE];
    hashed.copy_from_slice(header);

    let stored = read_u32(&hashed, HEADER_CHECKSUM)?;
    hashed[HEADER_CHECKSUM..HEADER_CHECKSUM + CRC_LEN].fill(0);

    let digest = Checksum::hash_skipping(&hashed, HEADER_CHECKSUM..HEADER_CHECKSUM + CRC_LEN)
        .map_err(|_| WalError::Corrupt)?;

    if digest.as_u32() != stored {
        return Err(WalError::ChecksumMismatch);
    }

    let version = u16::from_le_bytes([header[VERSION_OFFSET], header[VERSION_OFFSET + 1]]);
    if version != VERSION {
        return Err(WalError::UnsupportedVersion);
    }

    let page_size = read_u32(header, PAGE_SIZE_OFFSET)?;
    let expected = u32::try_from(PAGE_SIZE).map_err(|_| WalError::Corrupt)?;
    if page_size != expected {
        return Err(WalError::UnsupportedVersion);
    }

    let reserved = u16::from_le_bytes([header[HEADER_RESERVED], header[HEADER_RESERVED + 1]]);
    if reserved != 0 || header[HEADER_TAIL..].iter().any(|&byte| byte != 0) {
        return Err(WalError::Corrupt);
    }

    let mut file_id = [0u8; FILE_ID_LEN];
    file_id.copy_from_slice(&header[FILE_ID_OFFSET..FILE_ID_OFFSET + FILE_ID_LEN]);

    Ok(file_id)
}

fn encode_header(file_id: &[u8; FILE_ID_LEN]) -> Result<[u8; HEADER_SIZE], WalError> {
    let mut header = [0u8; HEADER_SIZE];
    header[MAGIC_OFFSET..MAGIC_OFFSET + MAGIC.len()].copy_from_slice(&MAGIC);
    header[VERSION_OFFSET..VERSION_OFFSET + 2].copy_from_slice(&VERSION.to_le_bytes());

    let page_size = u32::try_from(PAGE_SIZE).map_err(|_| WalError::Corrupt)?;
    header[PAGE_SIZE_OFFSET..PAGE_SIZE_OFFSET + 4].copy_from_slice(&page_size.to_le_bytes());
    header[FILE_ID_OFFSET..FILE_ID_OFFSET + FILE_ID_LEN].copy_from_slice(file_id);

    let digest = Checksum::hash_skipping(&header, HEADER_CHECKSUM..HEADER_CHECKSUM + CRC_LEN)
        .map_err(|_| WalError::Corrupt)?;

    header[HEADER_CHECKSUM..HEADER_CHECKSUM + CRC_LEN]
        .copy_from_slice(&digest.as_u32().to_le_bytes());

    Ok(header)
}

fn read_frame(bytes: &[u8]) -> Option<(WalFrame, usize)> {
    if bytes.len() < FRAME_HEADER_SIZE {
        return None;
    }

    let mut header = [0u8; FRAME_HEADER_SIZE];
    header.copy_from_slice(&bytes[..FRAME_HEADER_SIZE]);
    if header[FRAME_RESERVED..FRAME_LENGTH]
        .iter()
        .any(|&byte| byte != 0)
    {
        return None;
    }

    let payload_len = usize::try_from(read_u32(&header, FRAME_LENGTH).ok()?).ok()?;
    if payload_len == 0 || payload_len > MAX_PAYLOAD {
        return None;
    }

    let frame_len = FRAME_HEADER_SIZE.checked_add(payload_len)?;
    if bytes.len() < frame_len {
        return None;
    }

    let payload = &bytes[FRAME_HEADER_SIZE..frame_len];
    let stored = read_u32(&header, FRAME_CHECKSUM).ok()?;
    header[FRAME_CHECKSUM..FRAME_CHECKSUM + CRC_LEN].fill(0);

    let digest = frame_digest(&header, payload).ok()?;
    if digest.as_u32() != stored {
        return None;
    }

    let frame = match header[FRAME_KIND] {
        KIND_PAGE_IMAGE => page_image_frame(payload)?,
        KIND_COMMIT => commit_frame(payload)?,
        _ => return None,
    };

    Some((frame, frame_len))
}

fn page_image_frame(payload: &[u8]) -> Option<WalFrame> {
    if payload.len() != IMAGE_PAYLOAD_LEN {
        return None;
    }

    let txn_id = get_u64(payload, 0).ok()?;
    let page_id = PageId::new(get_u64(payload, 8).ok()?);
    let lsn = Lsn::new(get_u64(payload, 16).ok()?);
    if txn_id == 0 || lsn.as_u64() == 0 {
        return None;
    }

    let image = &payload[IMAGE_PREFIX..];
    checked_image(page_id, lsn, image).ok()?;

    Some(WalFrame::PageImage {
        txn_id,
        page_id,
        lsn,
        image: image.to_vec(),
    })
}

fn commit_frame(payload: &[u8]) -> Option<WalFrame> {
    if payload.len() != COMMIT_PAYLOAD_LEN {
        return None;
    }

    let txn_id = get_u64(payload, 0).ok()?;
    let lsn = Lsn::new(get_u64(payload, 8).ok()?);
    let page_count = get_u64(payload, 16).ok()?;
    if txn_id == 0 || lsn.as_u64() == 0 || page_count == 0 {
        return None;
    }

    Some(WalFrame::Commit {
        txn_id,
        lsn,
        page_count,
    })
}

fn checked_image(page_id: PageId, lsn: Lsn, image: &[u8]) -> Result<(), WalError> {
    if image.len() != PAGE_SIZE {
        return Err(WalError::Corrupt);
    }

    let header = PageHeader::decode(image).map_err(map_page)?;
    if header.page_id() != page_id || header.lsn() != lsn {
        return Err(WalError::Corrupt);
    }

    Ok(())
}

fn map_page(err: CodecError) -> WalError {
    match err {
        CodecError::Truncated => WalError::Torn,
        CodecError::ChecksumMismatch => WalError::ChecksumMismatch,
        CodecError::UnsupportedVersion => WalError::UnsupportedVersion,
        CodecError::BadMagic => WalError::BadMagic,
        CodecError::BadKind => WalError::Corrupt,
    }
}

fn image_payload(
    txn_id: u64,
    page_id: PageId,
    lsn: Lsn,
    image: &[u8],
) -> Result<Vec<u8>, WalError> {
    let mut payload = vec![0u8; IMAGE_PAYLOAD_LEN];
    put_u64(&mut payload, 0, txn_id).map_err(|_| WalError::Corrupt)?;
    put_u64(&mut payload, 8, page_id.as_u64()).map_err(|_| WalError::Corrupt)?;
    put_u64(&mut payload, 16, lsn.as_u64()).map_err(|_| WalError::Corrupt)?;
    payload[IMAGE_PREFIX..].copy_from_slice(image);
    Ok(payload)
}

fn commit_payload(
    txn_id: u64,
    lsn: Lsn,
    page_count: u64,
) -> Result<[u8; COMMIT_PAYLOAD_LEN], WalError> {
    let mut payload = [0u8; COMMIT_PAYLOAD_LEN];
    put_u64(&mut payload, 0, txn_id).map_err(|_| WalError::Corrupt)?;
    put_u64(&mut payload, 8, lsn.as_u64()).map_err(|_| WalError::Corrupt)?;
    put_u64(&mut payload, 16, page_count).map_err(|_| WalError::Corrupt)?;
    Ok(payload)
}

fn encode_frame(kind: u8, payload: &[u8]) -> Result<Vec<u8>, WalError> {
    if payload.is_empty() || payload.len() > MAX_PAYLOAD {
        return Err(WalError::Corrupt);
    }

    let payload_len = u32::try_from(payload.len()).map_err(|_| WalError::Corrupt)?;

    let mut frame = vec![0u8; FRAME_HEADER_SIZE + payload.len()];
    frame[FRAME_KIND] = kind;
    put_u32(&mut frame, FRAME_LENGTH, payload_len).map_err(|_| WalError::Corrupt)?;
    frame[FRAME_HEADER_SIZE..].copy_from_slice(payload);

    let digest = frame_digest(&frame[..FRAME_HEADER_SIZE], payload)?;
    frame[FRAME_CHECKSUM..FRAME_CHECKSUM + CRC_LEN].copy_from_slice(&digest.as_u32().to_le_bytes());

    Ok(frame)
}

fn frame_digest(header: &[u8], payload: &[u8]) -> Result<Checksum, WalError> {
    let mut bytes = Vec::with_capacity(header.len().saturating_add(payload.len()));
    bytes.extend_from_slice(header);
    bytes.extend_from_slice(payload);
    Checksum::hash_skipping(&bytes, FRAME_CHECKSUM..FRAME_CHECKSUM + CRC_LEN)
        .map_err(|_| WalError::Corrupt)
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, WalError> {
    let end = offset.checked_add(4).ok_or(WalError::Torn)?;
    let raw: [u8; 4] = bytes
        .get(offset..end)
        .ok_or(WalError::Torn)?
        .try_into()
        .map_err(|_| WalError::Torn)?;

    Ok(u32::from_le_bytes(raw))
}

fn read_file(file: &mut impl VfsFile) -> Result<Vec<u8>, WalError> {
    let len = usize::try_from(file.len()?).map_err(|_| WalError::Torn)?;
    let mut bytes = vec![0u8; len];
    if len > 0 {
        file.read_exact(0, &mut bytes)?;
    }

    Ok(bytes)
}

fn zero_between(file: &mut impl VfsFile, start: u64, end: u64) -> Result<(), WalError> {
    if end <= start {
        return Ok(());
    }

    let len = usize::try_from(end - start).map_err(|_| WalError::Corrupt)?;
    let zeros = vec![0u8; len];
    file.write_all(start, &zeros)?;

    Ok(())
}

fn add_len(offset: u64, len: usize) -> Result<u64, WalError> {
    let len = u64::try_from(len).map_err(|_| WalError::Corrupt)?;
    offset.checked_add(len).ok_or(WalError::Corrupt)
}

fn header_end() -> Result<u64, WalError> {
    u64::try_from(HEADER_SIZE).map_err(|_| WalError::Corrupt)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        CommittedPage, Durability, HEADER_CHECKSUM, HEADER_SIZE, HEADER_TAIL, MAGIC, VERSION,
        VERSION_OFFSET, Wal, WalError, decode_header, encode_frame, encode_header, recover,
    };
    use crate::checksum::Checksum;
    use crate::page::{Lsn, PAGE_SIZE, PageHeader, PageId, PageKind};
    use crate::vfs::{Fault, FaultInjecting, FaultKind, FileRole, OpenOptions, Vfs, VfsFile};
    use crate::vfs_memory::MemoryVfs;

    const FILE_ID: [u8; 16] = [7; 16];

    fn leaf(page_id: u64, lsn: u64, fill: u8) -> [u8; PAGE_SIZE] {
        let mut image = [fill; PAGE_SIZE];
        PageHeader::new(PageKind::BTreeLeaf, PageId::new(page_id), Lsn::new(lsn))
            .encode(&mut image)
            .unwrap();
        image
    }

    fn committed(txn_id: u64, page_id: u64, lsn: u64, image: &[u8]) -> CommittedPage {
        CommittedPage {
            txn_id,
            page_id: PageId::new(page_id),
            lsn: Lsn::new(lsn),
            image: image.to_vec(),
        }
    }

    fn snapshot(vfs: &MemoryVfs, path: &Path) -> Vec<u8> {
        let mut file = vfs.open(path, FileRole::Wal, &OpenOptions::new()).unwrap();
        let len = usize::try_from(file.len().unwrap()).unwrap();
        let mut bytes = vec![0u8; len];
        if len > 0 {
            file.read_exact(0, &mut bytes).unwrap();
        }
        bytes
    }

    fn file_len(vfs: &MemoryVfs, path: &Path) -> u64 {
        vfs.open(path, FileRole::Wal, &OpenOptions::new())
            .unwrap()
            .len()
            .unwrap()
    }

    #[test]
    fn example_torn_header() {
        assert!(matches!(recover(&[]), Err(WalError::Torn)));
        let err = recover(&[0; HEADER_SIZE - 1]).unwrap_err();
        assert!(matches!(err, WalError::Torn));
    }

    #[test]
    fn example_durability_values_differ() {
        let durable = Durability::Full;
        let volatile = Durability::None;
        assert_eq!(durable, Durability::Full);
        assert_ne!(durable, volatile);
    }

    #[test]
    fn example_commit_frame_matches() {
        use super::WalFrame;
        let frame = WalFrame::Commit {
            txn_id: 1,
            lsn: Lsn::new(1),
            page_count: 1,
        };
        assert!(matches!(
            frame,
            WalFrame::Commit {
                txn_id: 1,
                page_count: 1,
                ..
            }
        ));
    }

    #[test]
    fn example_create_starts_empty() {
        let vfs = MemoryVfs::new();
        let file = vfs
            .open(
                Path::new("db.wal"),
                FileRole::Wal,
                &OpenOptions::new().create(true),
            )
            .unwrap();
        let wal = Wal::create(file, [7; 16]).unwrap();
        assert_eq!(wal.file_id(), [7; 16]);
        assert_eq!(wal.durable_lsn(), Lsn::new(0));
        assert!(wal.pages().is_empty());
    }

    #[test]
    fn example_append_is_invisible_until_commit() {
        let vfs = MemoryVfs::new();
        let file = vfs
            .open(
                Path::new("db.wal"),
                FileRole::Wal,
                &OpenOptions::new().create(true),
            )
            .unwrap();
        let mut wal = Wal::create(file, [7; 16]).unwrap();
        let image = leaf(4, 1, 0x11);
        wal.append_page_image(1, PageId::new(4), Lsn::new(1), &image)
            .unwrap();
        assert!(wal.pages().is_empty());
        assert_eq!(wal.durable_lsn(), Lsn::new(0));
    }

    #[test]
    fn example_commit_records_the_page_after_sync() {
        let vfs = MemoryVfs::new();
        let file = vfs
            .open(
                Path::new("db.wal"),
                FileRole::Wal,
                &OpenOptions::new().create(true),
            )
            .unwrap();
        let mut wal = Wal::create(file, [7; 16]).unwrap();
        let image = leaf(4, 1, 0x11);
        wal.append_page_image(1, PageId::new(4), Lsn::new(1), &image)
            .unwrap();
        wal.commit(Durability::Full, 1, Lsn::new(1), 1).unwrap();
        assert_eq!(wal.durable_lsn(), Lsn::new(1));
        assert_eq!(wal.pages().len(), 1);
        let page = &wal.pages()[0];
        assert_eq!(page.txn_id(), 1);
        assert_eq!(page.page_id(), PageId::new(4));
        assert_eq!(page.lsn(), Lsn::new(1));
        assert_eq!(page.image(), &image);
    }

    #[test]
    fn example_sync_does_not_publish_pending_images() {
        let vfs = MemoryVfs::new();
        let file = vfs
            .open(
                Path::new("db.wal"),
                FileRole::Wal,
                &OpenOptions::new().create(true),
            )
            .unwrap();
        let mut wal = Wal::create(file, [7; 16]).unwrap();
        let image = leaf(4, 1, 0x11);
        wal.append_page_image(1, PageId::new(4), Lsn::new(1), &image)
            .unwrap();
        wal.sync().unwrap();
        assert_eq!(wal.durable_lsn(), Lsn::new(0));
        assert!(wal.pages().is_empty());
    }

    #[test]
    fn example_open_reads_a_synced_commit() {
        let vfs = MemoryVfs::new();
        let path = Path::new("db.wal");
        let file = vfs
            .open(path, FileRole::Wal, &OpenOptions::new().create(true))
            .unwrap();
        let mut wal = Wal::create(file, [7; 16]).unwrap();
        let image = leaf(4, 1, 0x11);
        wal.append_page_image(1, PageId::new(4), Lsn::new(1), &image)
            .unwrap();
        wal.commit(Durability::Full, 1, Lsn::new(1), 1).unwrap();
        drop(wal);

        let file = vfs.open(path, FileRole::Wal, &OpenOptions::new()).unwrap();
        let wal = Wal::open(file, [7; 16]).unwrap();
        assert_eq!(wal.durable_lsn(), Lsn::new(1));
        assert_eq!(wal.pages()[0].image(), &image);
    }

    #[test]
    fn example_recover_keeps_a_full_commit_and_drops_a_torn_tail() {
        let vfs = MemoryVfs::new();
        let path = Path::new("db.wal");
        let file = vfs
            .open(path, FileRole::Wal, &OpenOptions::new().create(true))
            .unwrap();
        let mut wal = Wal::create(file, [7; 16]).unwrap();
        let image = leaf(4, 1, 0x11);
        wal.append_page_image(1, PageId::new(4), Lsn::new(1), &image)
            .unwrap();
        wal.commit(Durability::None, 1, Lsn::new(1), 1).unwrap();
        drop(wal);

        let bytes = snapshot(&vfs, path);
        let recovered = recover(&bytes).unwrap();
        assert_eq!(recovered.file_id(), [7; 16]);
        assert_eq!(recovered.durable_lsn(), Lsn::new(1));
        assert_eq!(
            recovered.valid_end(),
            u64::try_from(bytes.len()).expect("file length fits in u64")
        );
        assert!(
            recover(&bytes[..bytes.len() - 1])
                .unwrap()
                .pages()
                .is_empty()
        );
    }

    #[test]
    fn every_prefix_returns_only_commits_fully_inside_it() {
        let vfs = MemoryVfs::new();
        let path = Path::new("prefix.wal");
        let file = vfs
            .open(path, FileRole::Wal, &OpenOptions::new().create(true))
            .unwrap();
        let mut wal = Wal::create(file, FILE_ID).unwrap();
        let mut marks = Vec::new();

        let first = leaf(4, 1, 0x11);
        wal.append_page_image(1, PageId::new(4), Lsn::new(1), &first)
            .unwrap();
        wal.commit(Durability::Full, 1, Lsn::new(1), 1).unwrap();
        marks.push((file_len(&vfs, path), vec![committed(1, 4, 1, &first)]));

        let second = leaf(5, 2, 0x22);
        let third = leaf(6, 2, 0x33);
        wal.append_page_image(2, PageId::new(5), Lsn::new(2), &second)
            .unwrap();
        wal.append_page_image(2, PageId::new(6), Lsn::new(2), &third)
            .unwrap();
        wal.commit(Durability::Full, 2, Lsn::new(2), 2).unwrap();
        marks.push((
            file_len(&vfs, path),
            vec![committed(2, 5, 2, &second), committed(2, 6, 2, &third)],
        ));

        let pending = leaf(7, 3, 0x44);
        wal.append_page_image(3, PageId::new(7), Lsn::new(3), &pending)
            .unwrap();
        let bytes = snapshot(&vfs, path);

        for len in 0..=bytes.len() {
            let expected: Vec<CommittedPage> = marks
                .iter()
                .take_while(|(end, _)| *end <= u64::try_from(len).unwrap())
                .flat_map(|(_, pages)| pages.iter().cloned())
                .collect();
            match recover(&bytes[..len]) {
                Err(WalError::Torn) => {
                    assert!(len < HEADER_SIZE, "prefix {len} returned Torn");
                    assert!(expected.is_empty());
                }
                Ok(recovery) => {
                    assert!(len >= HEADER_SIZE, "prefix {len} recovered a short header");
                    assert_eq!(recovery.file_id(), FILE_ID, "prefix {len}");
                    let end = recovery.valid_end();
                    assert!(
                        end >= u64::try_from(HEADER_SIZE).unwrap()
                            && end <= u64::try_from(len).unwrap(),
                        "prefix {len} valid end {end}"
                    );
                    assert_eq!(recovery.pages(), expected.as_slice(), "prefix {len}");
                }
                Err(err) => panic!("prefix {len}: {err}"),
            }
        }
    }

    #[test]
    fn a_bad_frame_hides_every_later_frame() {
        let vfs = MemoryVfs::new();
        let path = Path::new("tail.wal");
        let file = vfs
            .open(path, FileRole::Wal, &OpenOptions::new().create(true))
            .unwrap();
        let mut wal = Wal::create(file, FILE_ID).unwrap();
        let first = leaf(4, 1, 0x11);
        wal.append_page_image(1, PageId::new(4), Lsn::new(1), &first)
            .unwrap();
        wal.commit(Durability::Full, 1, Lsn::new(1), 1).unwrap();
        drop(wal);

        let mut bytes = snapshot(&vfs, path);
        let good_end = bytes.len();
        let later = leaf(8, 2, 0x55);
        let mut payload = vec![0u8; 24 + PAGE_SIZE];
        payload[..8].copy_from_slice(&2u64.to_le_bytes());
        payload[8..16].copy_from_slice(&8u64.to_le_bytes());
        payload[16..24].copy_from_slice(&2u64.to_le_bytes());
        payload[24..].copy_from_slice(&later);
        bytes.extend(encode_frame(9, &[0; 24]).unwrap());
        bytes.extend(encode_frame(1, &payload).unwrap());
        bytes.extend(encode_frame(2, &commit_bytes(2, 2, 1)).unwrap());
        vfs.replace(path, FileRole::Wal, &bytes).unwrap();

        let file = vfs.open(path, FileRole::Wal, &OpenOptions::new()).unwrap();
        let wal = Wal::open(file, FILE_ID).unwrap();
        assert_eq!(wal.durable_lsn(), Lsn::new(1));
        assert_eq!(wal.pages(), &[committed(1, 4, 1, &first)]);
        drop(wal);

        let mut checksum = bytes.clone();
        checksum[good_end + 20] ^= 0x01;
        vfs.replace(path, FileRole::Wal, &checksum).unwrap();
        let file = vfs.open(path, FileRole::Wal, &OpenOptions::new()).unwrap();
        let wal = Wal::open(file, FILE_ID).unwrap();
        assert_eq!(wal.pages().len(), 1);
        drop(wal);

        let mut truncated = bytes[..good_end + 4].to_vec();
        truncated.extend(encode_frame(2, &commit_bytes(2, 2, 1)).unwrap());
        vfs.replace(path, FileRole::Wal, &truncated).unwrap();
        let file = vfs.open(path, FileRole::Wal, &OpenOptions::new()).unwrap();
        let wal = Wal::open(file, FILE_ID).unwrap();
        assert_eq!(wal.durable_lsn(), Lsn::new(1));
        assert_eq!(wal.pages().len(), 1);
    }

    #[test]
    fn full_sync_failure_does_not_record_the_commit_and_retry_can() {
        // Counted calls: open, header write, len, header sync, image write,
        // commit write, commit sync.
        let vfs = FaultInjecting::new(
            MemoryVfs::new(),
            [Fault {
                at: 7,
                kind: FaultKind::Error,
            }],
        );
        let file = vfs
            .open(
                Path::new("fault.wal"),
                FileRole::Wal,
                &OpenOptions::new().create(true),
            )
            .unwrap();
        let mut wal = Wal::create(file, FILE_ID).unwrap();
        let image = leaf(4, 1, 0x11);
        wal.append_page_image(1, PageId::new(4), Lsn::new(1), &image)
            .unwrap();
        let err = wal.commit(Durability::Full, 1, Lsn::new(1), 1).unwrap_err();
        assert!(matches!(err, WalError::Io(_)), "{err}");
        assert_eq!(wal.durable_lsn(), Lsn::new(0));
        assert!(wal.pages().is_empty());

        let retry = leaf(4, 1, 0x22);
        wal.append_page_image(1, PageId::new(4), Lsn::new(1), &retry)
            .unwrap();
        wal.commit(Durability::Full, 1, Lsn::new(1), 1).unwrap();
        assert_eq!(wal.durable_lsn(), Lsn::new(1));
        assert_eq!(wal.pages()[0].image(), &retry);
    }

    #[test]
    fn a_failed_later_sync_keeps_the_previous_full_commit() {
        // The tenth counted call is the second commit's sync.
        let vfs = FaultInjecting::new(
            MemoryVfs::new(),
            [Fault {
                at: 10,
                kind: FaultKind::Error,
            }],
        );
        let path = Path::new("fault-prefix.wal");
        let file = vfs
            .open(path, FileRole::Wal, &OpenOptions::new().create(true))
            .unwrap();
        let mut wal = Wal::create(file, FILE_ID).unwrap();
        let first = leaf(4, 1, 0x11);
        wal.append_page_image(1, PageId::new(4), Lsn::new(1), &first)
            .unwrap();
        wal.commit(Durability::Full, 1, Lsn::new(1), 1).unwrap();
        let second = leaf(5, 2, 0x22);
        wal.append_page_image(2, PageId::new(5), Lsn::new(2), &second)
            .unwrap();
        let err = wal.commit(Durability::Full, 2, Lsn::new(2), 1).unwrap_err();
        assert!(matches!(err, WalError::Io(_)), "{err}");
        assert_eq!(wal.durable_lsn(), Lsn::new(1));
        assert_eq!(wal.pages().len(), 1);
        drop(wal);
        vfs.inner().crash();

        let file = vfs.open(path, FileRole::Wal, &OpenOptions::new()).unwrap();
        let wal = Wal::open(file, FILE_ID).unwrap();
        assert_eq!(wal.durable_lsn(), Lsn::new(1));
        assert_eq!(wal.pages(), &[committed(1, 4, 1, &first)]);
    }

    #[test]
    fn an_acknowledged_full_commit_survives_crash_and_none_does_not() {
        let vfs = MemoryVfs::new();
        let path = Path::new("crash.wal");
        let file = vfs
            .open(path, FileRole::Wal, &OpenOptions::new().create(true))
            .unwrap();
        let mut wal = Wal::create(file, FILE_ID).unwrap();
        let durable = leaf(4, 1, 0x11);
        wal.append_page_image(1, PageId::new(4), Lsn::new(1), &durable)
            .unwrap();
        wal.commit(Durability::Full, 1, Lsn::new(1), 1).unwrap();
        let lost = leaf(5, 2, 0x22);
        wal.append_page_image(2, PageId::new(5), Lsn::new(2), &lost)
            .unwrap();
        wal.commit(Durability::None, 2, Lsn::new(2), 1).unwrap();
        assert_eq!(wal.durable_lsn(), Lsn::new(2));
        drop(wal);
        vfs.crash();

        let file = vfs.open(path, FileRole::Wal, &OpenOptions::new()).unwrap();
        let wal = Wal::open(file, FILE_ID).unwrap();
        assert_eq!(wal.durable_lsn(), Lsn::new(1));
        assert_eq!(wal.pages(), &[committed(1, 4, 1, &durable)]);
    }

    #[test]
    fn synced_images_without_a_commit_are_absent_after_crash() {
        let vfs = MemoryVfs::new();
        let path = Path::new("unsynced-commit.wal");
        let file = vfs
            .open(path, FileRole::Wal, &OpenOptions::new().create(true))
            .unwrap();
        let mut wal = Wal::create(file, FILE_ID).unwrap();
        let image = leaf(4, 1, 0x11);
        wal.append_page_image(1, PageId::new(4), Lsn::new(1), &image)
            .unwrap();
        wal.sync().unwrap();
        drop(wal);
        vfs.crash();

        let file = vfs.open(path, FileRole::Wal, &OpenOptions::new()).unwrap();
        let wal = Wal::open(file, FILE_ID).unwrap();
        assert_eq!(wal.durable_lsn(), Lsn::new(0));
        assert!(wal.pages().is_empty());
    }

    #[test]
    fn header_rejects_a_foreign_version_magic_and_file_id() {
        let mut header = encode_header(&FILE_ID).unwrap();
        header[VERSION_OFFSET..VERSION_OFFSET + 2].copy_from_slice(&2u16.to_le_bytes());
        reseal_header(&mut header);
        assert!(matches!(
            decode_header(&header),
            Err(WalError::UnsupportedVersion)
        ));

        header = encode_header(&FILE_ID).unwrap();
        header[0] ^= 0x01;
        assert!(matches!(decode_header(&header), Err(WalError::BadMagic)));

        header = encode_header(&FILE_ID).unwrap();
        header[HEADER_CHECKSUM] ^= 0x01;
        assert!(matches!(
            decode_header(&header),
            Err(WalError::ChecksumMismatch)
        ));

        header = encode_header(&FILE_ID).unwrap();
        header[HEADER_TAIL] = 1;
        reseal_header(&mut header);
        assert!(matches!(decode_header(&header), Err(WalError::Corrupt)));

        let vfs = MemoryVfs::new();
        let path = Path::new("id.wal");
        let file = vfs
            .open(path, FileRole::Wal, &OpenOptions::new().create(true))
            .unwrap();
        drop(Wal::create(file, FILE_ID).unwrap());
        let file = vfs.open(path, FileRole::Wal, &OpenOptions::new()).unwrap();
        assert!(matches!(
            Wal::open(file, [9; 16]),
            Err(WalError::FileIdMismatch)
        ));
    }

    #[test]
    fn append_and_commit_reject_inconsistent_frames() {
        let vfs = MemoryVfs::new();
        let file = vfs
            .open(
                Path::new("bad.wal"),
                FileRole::Wal,
                &OpenOptions::new().create(true),
            )
            .unwrap();
        let mut wal = Wal::create(file, FILE_ID).unwrap();
        let image = leaf(4, 1, 0x11);
        assert!(matches!(
            wal.append_page_image(0, PageId::new(4), Lsn::new(1), &image),
            Err(WalError::Corrupt)
        ));
        assert!(matches!(
            wal.append_page_image(1, PageId::new(5), Lsn::new(1), &image),
            Err(WalError::Corrupt)
        ));
        wal.append_page_image(1, PageId::new(4), Lsn::new(1), &image)
            .unwrap();
        assert!(matches!(
            wal.append_page_image(1, PageId::new(4), Lsn::new(1), &image),
            Err(WalError::Corrupt)
        ));
        assert!(matches!(
            wal.commit(Durability::None, 1, Lsn::new(1), 2),
            Err(WalError::Corrupt)
        ));
        assert_eq!(wal.durable_lsn(), Lsn::new(0));
        wal.commit(Durability::None, 1, Lsn::new(1), 1).unwrap();
        assert_eq!(wal.durable_lsn(), Lsn::new(1));
    }

    #[test]
    fn header_magic_is_the_file_bytes_pzlw() {
        let header = encode_header(&FILE_ID).unwrap();
        assert_eq!(&header[..4], &MAGIC);
        assert_eq!(&MAGIC, b"PZLW");
        assert_eq!(
            u16::from_le_bytes([header[VERSION_OFFSET], header[VERSION_OFFSET + 1]]),
            VERSION
        );
    }

    fn reseal_header(header: &mut [u8; HEADER_SIZE]) {
        header[HEADER_CHECKSUM..HEADER_CHECKSUM + 4].fill(0);
        let digest = Checksum::hash_skipping(header, HEADER_CHECKSUM..HEADER_CHECKSUM + 4).unwrap();
        header[HEADER_CHECKSUM..HEADER_CHECKSUM + 4]
            .copy_from_slice(&digest.as_u32().to_le_bytes());
    }

    fn commit_bytes(txn_id: u64, lsn: u64, page_count: u64) -> [u8; 24] {
        let mut payload = [0u8; 24];
        payload[..8].copy_from_slice(&txn_id.to_le_bytes());
        payload[8..16].copy_from_slice(&lsn.to_le_bytes());
        payload[16..].copy_from_slice(&page_count.to_le_bytes());
        payload
    }
}
