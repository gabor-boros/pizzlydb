//! Fixed-size page images with a versioned header and a CRC-32C checksum.
//!
//! Every page is [`PAGE_SIZE`] bytes (8192). The size is a property of the
//! format, not a value chosen when a file is opened. Multi-byte header fields
//! are little-endian.
//!
//! The header occupies the first [`HEADER_SIZE`] bytes:
//!
//! - `0..4`: magic [`MAGIC`] (`PZLY`)
//! - `4..6`: [`FORMAT_VERSION`] as `u16`
//! - `6..8`: [`PageKind`] as `u16`
//! - `8..16`: [`PageId`] as `u64`
//! - `16..24`: [`Lsn`] as `u64`
//! - `24..28`: CRC-32C as `u32`
//!
//! [`PageHeader::decode`] checks the length, magic, version, and kind before
//! the checksum. It returns a header only when all of them match. The checksum
//! covers every byte of the page. The stored checksum is treated as zeros
//! while hashing. Decode does not allocate.
//!
//! A caller that requested page `id` compares [`PageHeader::page_id`] with
//! `id` after a successful decode.
//!
//! [`PageKind`] discriminants belong to version 1. Append a new kind. Do not
//! reuse or renumber an existing value.
//!
//! - `0`: [`PageKind::Free`]
//! - `1`: [`PageKind::Superblock`]
//! - `2`: [`PageKind::Allocator`]
//! - `3`: [`PageKind::Bitmap`]
//! - `4`: [`PageKind::FreeSpaceMap`]
//! - `5`: [`PageKind::BTreeInternal`]
//! - `6`: [`PageKind::BTreeLeaf`]
//! - `7`: [`PageKind::Overflow`]
//! - `8`: [`PageKind::RetireSpill`]
//! - `9`: [`PageKind::State`]
//!
//! Pages 0 and 1 are the superblock copies. [`ALLOCATOR_PAGE_ID`] is the
//! allocator header. [`BITMAP_PAGE_ID`] is the allocation bitmap.
//! [`FIRST_DATA_PAGE_ID`] is the first id available for a data page. Those
//! ids stay allocated for the life of the file. Free-space hints are stored
//! in the allocator header. No page id is reserved for [`PageKind::FreeSpaceMap`].
//! [`PageKind::State`] is the ordered-map root record. The engine allocates it
//! once at [`FIRST_DATA_PAGE_ID`].

use crate::checksum::Checksum;
use crate::codec::{CodecError, get_u16, get_u32, get_u64, put_u16, put_u32, put_u64};

/// Page size in bytes. Every page image is this long.
pub const PAGE_SIZE: usize = 8192;

/// On-disk magic. The file bytes are `PZLY`, in that order.
pub const MAGIC: [u8; 4] = *b"PZLY";

/// Page-header version written and accepted by this crate.
pub const FORMAT_VERSION: u16 = 1;

/// Offset of [`MAGIC`].
pub const MAGIC_OFFSET: usize = 0;

/// Offset of the version `u16`.
pub const VERSION_OFFSET: usize = 4;

/// Offset of the [`PageKind`] `u16`.
pub const KIND_OFFSET: usize = 6;

/// Offset of the [`PageId`] `u64`.
pub const PAGE_ID_OFFSET: usize = 8;

/// Offset of the [`Lsn`] `u64`.
pub const LSN_OFFSET: usize = 16;

/// Offset of the CRC-32C `u32`.
pub const CHECKSUM_OFFSET: usize = 24;

/// Length of the CRC-32C field.
pub const CHECKSUM_LEN: usize = 4;

/// Length of the header. Bytes at this offset and after are the page body.
pub const HEADER_SIZE: usize = CHECKSUM_OFFSET + CHECKSUM_LEN;

const _: () = assert!(HEADER_SIZE < PAGE_SIZE);
const _: () = assert!(MAGIC.len() == VERSION_OFFSET - MAGIC_OFFSET);

/// Page number in the page file.
///
/// # Examples
///
/// ```ignore
/// use pizzlydb_storage::page::PageId;
///
/// let id = PageId::new(4);
/// assert_eq!(id.as_u64(), 4);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PageId(u64);

impl PageId {
    /// Page number `id`. Zero is a valid page number.
    #[must_use]
    pub const fn new(id: u64) -> Self {
        Self(id)
    }

    /// Raw page number.
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

/// Sequence number assigned to a committed batch.
///
/// Zero is the sequence before any commit. A later batch receives a greater
/// value.
///
/// # Examples
///
/// ```
/// use pizzlydb_storage::Lsn;
///
/// let lsn = Lsn::new(9);
/// assert_eq!(lsn.as_u64(), 9);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Lsn(u64);

impl Lsn {
    /// Log sequence number `lsn`. Zero is a valid sequence number.
    #[must_use]
    pub const fn new(lsn: u64) -> Self {
        Self(lsn)
    }

    /// Raw log sequence number.
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

/// Allocator header. It occupies page 2.
pub const ALLOCATOR_PAGE_ID: PageId = PageId::new(2);

/// Allocation bitmap. It occupies page 3.
pub const BITMAP_PAGE_ID: PageId = PageId::new(3);

/// First page id available for a data page.
///
/// Pages 0 and 1 are the superblock copies. [`ALLOCATOR_PAGE_ID`] and
/// [`BITMAP_PAGE_ID`] are the other reserved ids. Allocation does not return
/// those four ids.
pub const FIRST_DATA_PAGE_ID: PageId = PageId::new(4);

/// Kind of a page image.
///
/// The numeric values are version 1 of this crate and are the on-disk `u16`.
/// Append a new kind. Do not reuse or renumber an existing value.
///
/// Pages 0 and 1 are the superblock copies. [`ALLOCATOR_PAGE_ID`] and
/// [`BITMAP_PAGE_ID`] stay allocated for the life of the file.
/// [`FIRST_DATA_PAGE_ID`] is the first id a data page may use. Free-space
/// hints live in the allocator header. No page id is reserved for
/// [`Self::FreeSpaceMap`].
///
/// # Examples
///
/// ```ignore
/// use pizzlydb_storage::codec::CodecError;
/// use pizzlydb_storage::page::PageKind;
///
/// assert_eq!(PageKind::try_from(6), Ok(PageKind::BTreeLeaf));
/// assert_eq!(PageKind::BTreeLeaf.as_u16(), 6);
/// assert_eq!(PageKind::try_from(10), Err(CodecError::BadKind));
/// ```
#[repr(u16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PageKind {
    /// Unused page.
    Free = 0,
    /// Database superblock.
    Superblock = 1,
    /// Allocator header at [`ALLOCATOR_PAGE_ID`].
    Allocator = 2,
    /// Extent allocation bitmap at [`BITMAP_PAGE_ID`].
    Bitmap = 3,
    /// Free-space map page.
    ///
    /// Hints for this format are stored in the allocator header. This kind
    /// has no reserved page id.
    FreeSpaceMap = 4,
    /// Internal node of a B-tree.
    BTreeInternal = 5,
    /// Leaf node of a B-tree.
    BTreeLeaf = 6,
    /// Overflow page.
    Overflow = 7,
    /// Retire records that do not fit in the allocator header.
    RetireSpill = 8,
    /// Root and high-water record for the ordered map.
    ///
    /// The engine writes this kind at [`FIRST_DATA_PAGE_ID`]. Keys are stored on
    /// other pages.
    State = 9,
}

impl PageKind {
    /// On-disk discriminant.
    #[must_use]
    pub const fn as_u16(self) -> u16 {
        self as u16
    }
}

impl TryFrom<u16> for PageKind {
    type Error = CodecError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Ok(match value {
            0 => Self::Free,
            1 => Self::Superblock,
            2 => Self::Allocator,
            3 => Self::Bitmap,
            4 => Self::FreeSpaceMap,
            5 => Self::BTreeInternal,
            6 => Self::BTreeLeaf,
            7 => Self::Overflow,
            8 => Self::RetireSpill,
            9 => Self::State,
            _ => return Err(CodecError::BadKind),
        })
    }
}

/// Header fields trusted after a successful decode.
///
/// # Examples
///
/// ```ignore
/// use pizzlydb_storage::page::{
///     HEADER_SIZE, Lsn, PAGE_SIZE, PageHeader, PageId, PageKind,
/// };
///
/// let header = PageHeader::new(PageKind::BTreeLeaf, PageId::new(7), Lsn::new(3));
///
/// let mut page = [0x5A; PAGE_SIZE];
/// header.encode(&mut page).unwrap();
///
/// assert_eq!(PageHeader::decode(&page).unwrap(), header);
/// assert_eq!(page[HEADER_SIZE], 0x5A);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PageHeader {
    kind: PageKind,
    page_id: PageId,
    lsn: Lsn,
}

impl PageHeader {
    /// Header for `kind` at `page_id` with log sequence number `lsn`.
    #[must_use]
    pub const fn new(kind: PageKind, page_id: PageId, lsn: Lsn) -> Self {
        Self { kind, page_id, lsn }
    }

    /// Page kind.
    #[must_use]
    pub const fn kind(self) -> PageKind {
        self.kind
    }

    /// Page number stored in the header.
    #[must_use]
    pub const fn page_id(self) -> PageId {
        self.page_id
    }

    /// Log sequence number stored in the header.
    #[must_use]
    pub const fn lsn(self) -> Lsn {
        self.lsn
    }

    /// Write this header into `page` and store the CRC-32C of the image.
    ///
    /// `page` must be exactly [`PAGE_SIZE`] bytes. Bytes at [`HEADER_SIZE`] and
    /// after are included in the checksum and are left as the caller filled them.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError::Truncated`] when `page` is not [`PAGE_SIZE`] bytes.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use pizzlydb_storage::page::{
    ///     HEADER_SIZE, Lsn, PAGE_SIZE, PageHeader, PageId, PageKind,
    /// };
    ///
    /// let header = PageHeader::new(PageKind::BTreeLeaf, PageId::new(7), Lsn::new(3));
    ///
    /// let mut page = [0x5A; PAGE_SIZE];
    /// header.encode(&mut page).unwrap();
    ///
    /// assert_eq!(PageHeader::decode(&page).unwrap(), header);
    /// assert_eq!(page[HEADER_SIZE], 0x5A);
    /// ```
    pub fn encode(&self, page: &mut [u8]) -> Result<(), CodecError> {
        self.write(page)?;
        seal(page)
    }

    /// Read a header from a page image.
    ///
    /// # Errors
    ///
    /// - [`CodecError::Truncated`] when `page` is not [`PAGE_SIZE`] bytes.
    /// - [`CodecError::BadMagic`] when the image does not start with [`MAGIC`].
    /// - [`CodecError::UnsupportedVersion`] when the version is not [`FORMAT_VERSION`].
    /// - [`CodecError::BadKind`] when the kind value is not a [`PageKind`].
    /// - [`CodecError::ChecksumMismatch`] when the stored CRC-32C does not match the image.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use pizzlydb_storage::codec::CodecError;
    /// use pizzlydb_storage::page::{
    ///     HEADER_SIZE, Lsn, PAGE_SIZE, PageHeader, PageId, PageKind,
    /// };
    ///
    /// let header = PageHeader::new(PageKind::BTreeLeaf, PageId::new(7), Lsn::new(3));
    ///
    /// let mut page = [0x5A; PAGE_SIZE];
    /// header.encode(&mut page).unwrap();
    /// page[HEADER_SIZE] ^= 0x01;
    ///
    /// assert_eq!(PageHeader::decode(&page), Err(CodecError::ChecksumMismatch));
    /// assert_eq!(PageHeader::decode(&page[..100]), Err(CodecError::Truncated));
    /// ```
    pub fn decode(page: &[u8]) -> Result<Self, CodecError> {
        if page.len() != PAGE_SIZE {
            return Err(CodecError::Truncated);
        }

        if page[MAGIC_OFFSET..MAGIC_OFFSET + MAGIC.len()] != MAGIC {
            return Err(CodecError::BadMagic);
        }

        let version = get_u16(page, VERSION_OFFSET)?;
        if version != FORMAT_VERSION {
            return Err(CodecError::UnsupportedVersion);
        }

        let kind = PageKind::try_from(get_u16(page, KIND_OFFSET)?)?;
        let page_id = PageId::new(get_u64(page, PAGE_ID_OFFSET)?);
        let lsn = Lsn::new(get_u64(page, LSN_OFFSET)?);
        let stored = Checksum::from_u32(get_u32(page, CHECKSUM_OFFSET)?);
        let computed = Checksum::hash_skipping(page, checksum_range())?;

        if stored != computed {
            return Err(CodecError::ChecksumMismatch);
        }

        Ok(Self { kind, page_id, lsn })
    }

    fn write(&self, page: &mut [u8]) -> Result<(), CodecError> {
        if page.len() != PAGE_SIZE {
            return Err(CodecError::Truncated);
        }

        page[MAGIC_OFFSET..MAGIC_OFFSET + MAGIC.len()].copy_from_slice(&MAGIC);
        put_u16(page, VERSION_OFFSET, FORMAT_VERSION)?;
        put_u16(page, KIND_OFFSET, self.kind.as_u16())?;
        put_u64(page, PAGE_ID_OFFSET, self.page_id.as_u64())?;
        put_u64(page, LSN_OFFSET, self.lsn.as_u64())?;
        put_u32(page, CHECKSUM_OFFSET, 0)?;

        Ok(())
    }
}

impl TryFrom<&[u8]> for PageHeader {
    type Error = CodecError;

    fn try_from(page: &[u8]) -> Result<Self, Self::Error> {
        Self::decode(page)
    }
}

/// Store the CRC-32C of `page` in the checksum field.
///
/// The stored field is covered as zeros. Magic, version, and kind are not
/// checked. `page` must be exactly [`PAGE_SIZE`] bytes.
///
/// # Errors
///
/// Returns [`CodecError::Truncated`] when `page` is not [`PAGE_SIZE`] bytes.
///
/// # Examples
///
/// ```ignore
/// use pizzlydb_storage::page::{
///     seal, Lsn, PAGE_SIZE, PageHeader, PageId, PageKind,
/// };
///
/// let header = PageHeader::new(PageKind::Free, PageId::new(0), Lsn::new(0));
///
/// let mut page = [0; PAGE_SIZE];
/// header.encode(&mut page).unwrap();
/// page[40] ^= 0x01;
///
/// seal(&mut page).unwrap();
///
/// assert_eq!(PageHeader::decode(&page).unwrap().page_id(), PageId::new(0));
/// ```
pub fn seal(page: &mut [u8]) -> Result<(), CodecError> {
    if page.len() != PAGE_SIZE {
        return Err(CodecError::Truncated);
    }

    put_u32(page, CHECKSUM_OFFSET, 0)?;
    let sum = Checksum::hash_skipping(page, checksum_range())?;
    put_u32(page, CHECKSUM_OFFSET, sum.as_u32())
}

fn checksum_range() -> std::ops::Range<usize> {
    CHECKSUM_OFFSET..CHECKSUM_OFFSET + CHECKSUM_LEN
}

#[cfg(test)]
mod tests {
    use super::{
        CHECKSUM_LEN, CHECKSUM_OFFSET, FORMAT_VERSION, HEADER_SIZE, KIND_OFFSET, LSN_OFFSET, Lsn,
        MAGIC, MAGIC_OFFSET, PAGE_ID_OFFSET, PAGE_SIZE, PageHeader, PageId, PageKind,
        VERSION_OFFSET, seal,
    };
    use crate::checksum::Checksum;
    use crate::codec::{CodecError, get_u32, put_u16, put_u32};

    fn sample_header() -> PageHeader {
        PageHeader::new(PageKind::BTreeLeaf, PageId::new(7), Lsn::new(3))
    }

    fn sealed_page() -> [u8; PAGE_SIZE] {
        let mut page = [0x5A; PAGE_SIZE];
        sample_header().encode(&mut page).unwrap();
        page
    }

    #[test]
    fn example_page_id_and_lsn() {
        assert_eq!(PageId::new(4).as_u64(), 4);
        assert_eq!(Lsn::new(9).as_u64(), 9);
    }

    #[test]
    fn example_page_kind() {
        assert_eq!(PageKind::try_from(6), Ok(PageKind::BTreeLeaf));
        assert_eq!(PageKind::BTreeLeaf.as_u16(), 6);
        assert_eq!(PageKind::try_from(10), Err(CodecError::BadKind));
    }

    #[test]
    fn page_kind_discriminants_are_stable() {
        assert_eq!(PageKind::Free.as_u16(), 0);
        assert_eq!(PageKind::Superblock.as_u16(), 1);
        assert_eq!(PageKind::Allocator.as_u16(), 2);
        assert_eq!(PageKind::Bitmap.as_u16(), 3);
        assert_eq!(PageKind::FreeSpaceMap.as_u16(), 4);
        assert_eq!(PageKind::BTreeInternal.as_u16(), 5);
        assert_eq!(PageKind::BTreeLeaf.as_u16(), 6);
        assert_eq!(PageKind::Overflow.as_u16(), 7);
        assert_eq!(PageKind::RetireSpill.as_u16(), 8);
        assert_eq!(PageKind::State.as_u16(), 9);
        assert_eq!(PageKind::try_from(0xFFFF), Err(CodecError::BadKind));
    }

    #[test]
    fn example_encode_roundtrip_preserves_the_body() {
        let header = sample_header();
        let mut page = [0x5A; PAGE_SIZE];
        header.encode(&mut page).unwrap();
        let decoded = PageHeader::decode(&page).unwrap();
        assert_eq!(decoded, header);
        assert_eq!(decoded.kind(), PageKind::BTreeLeaf);
        assert_eq!(decoded.page_id(), PageId::new(7));
        assert_eq!(decoded.lsn(), Lsn::new(3));
        assert_eq!(page[HEADER_SIZE], 0x5A);
        assert_eq!(&page[MAGIC_OFFSET..MAGIC_OFFSET + MAGIC.len()], &MAGIC);
        assert_eq!(
            &page[VERSION_OFFSET..VERSION_OFFSET + 2],
            &FORMAT_VERSION.to_le_bytes()
        );
        assert_eq!(
            &page[KIND_OFFSET..KIND_OFFSET + 2],
            &PageKind::BTreeLeaf.as_u16().to_le_bytes()
        );
        assert_eq!(
            &page[PAGE_ID_OFFSET..PAGE_ID_OFFSET + 8],
            &7_u64.to_le_bytes()
        );
        assert_eq!(&page[LSN_OFFSET..LSN_OFFSET + 8], &3_u64.to_le_bytes());
        assert!(page[HEADER_SIZE..].iter().all(|byte| *byte == 0x5A));
    }

    #[test]
    fn example_decode_detects_a_flipped_payload_byte_and_a_short_prefix() {
        let header = sample_header();
        let mut page = [0x5A; PAGE_SIZE];
        header.encode(&mut page).unwrap();
        page[HEADER_SIZE] ^= 0x01;
        assert_eq!(PageHeader::decode(&page), Err(CodecError::ChecksumMismatch));
        let sealed = sealed_page();
        assert_eq!(
            PageHeader::decode(&sealed[..100]),
            Err(CodecError::Truncated)
        );
        assert_eq!(PageHeader::decode(&[]), Err(CodecError::Truncated));
        assert_eq!(
            PageHeader::decode(&[0; PAGE_SIZE + 1]),
            Err(CodecError::Truncated)
        );
    }

    #[test]
    fn try_from_matches_decode() {
        let page = sealed_page();
        assert_eq!(
            PageHeader::try_from(page.as_slice()).unwrap(),
            sample_header()
        );
    }

    #[test]
    fn example_seal_after_a_body_change() {
        let header = PageHeader::new(PageKind::Free, PageId::new(0), Lsn::new(0));
        let mut page = [0; PAGE_SIZE];
        header.encode(&mut page).unwrap();
        page[40] ^= 0x01;
        seal(&mut page).unwrap();
        assert_eq!(PageHeader::decode(&page).unwrap().page_id(), PageId::new(0));
        assert_eq!(page[40], 0x01);
    }

    #[test]
    fn body_and_page_id_are_covered_by_the_checksum() {
        let header = sample_header();
        let mut untouched = [0x5A; PAGE_SIZE];
        let mut flipped = [0x5A; PAGE_SIZE];
        flipped[HEADER_SIZE] = 0x5B;
        header.encode(&mut untouched).unwrap();
        header.encode(&mut flipped).unwrap();
        assert_ne!(
            get_u32(&untouched, CHECKSUM_OFFSET).unwrap(),
            get_u32(&flipped, CHECKSUM_OFFSET).unwrap()
        );

        let mut page = sealed_page();
        let before = get_u32(&page, CHECKSUM_OFFSET).unwrap();
        page[PAGE_ID_OFFSET] ^= 0x01;
        assert_eq!(PageHeader::decode(&page), Err(CodecError::ChecksumMismatch));
        seal(&mut page).unwrap();
        assert_ne!(get_u32(&page, CHECKSUM_OFFSET).unwrap(), before);
        assert_eq!(PageHeader::decode(&page).unwrap().page_id(), PageId::new(6));
    }

    #[test]
    fn checksum_that_includes_its_own_field_does_not_decode() {
        let mut page = sealed_page();
        put_u32(&mut page, CHECKSUM_OFFSET, 0xFFFF_FFFF).unwrap();
        let included = Checksum::hash(&page);
        let skipped =
            Checksum::hash_skipping(&page, CHECKSUM_OFFSET..CHECKSUM_OFFSET + CHECKSUM_LEN)
                .unwrap();
        assert_ne!(included, skipped);
        put_u32(&mut page, CHECKSUM_OFFSET, included.as_u32()).unwrap();
        assert_eq!(PageHeader::decode(&page), Err(CodecError::ChecksumMismatch));
    }

    #[test]
    fn resealed_structural_damage_is_rejected() {
        let mut page = sealed_page();
        page[MAGIC_OFFSET..MAGIC_OFFSET + MAGIC.len()].copy_from_slice(b"NOPE");
        seal(&mut page).unwrap();
        assert_eq!(PageHeader::decode(&page), Err(CodecError::BadMagic));

        let mut page = sealed_page();
        put_u16(&mut page, VERSION_OFFSET, 99).unwrap();
        seal(&mut page).unwrap();
        assert_eq!(
            PageHeader::decode(&page),
            Err(CodecError::UnsupportedVersion)
        );

        let mut page = sealed_page();
        put_u16(&mut page, KIND_OFFSET, 10).unwrap();
        seal(&mut page).unwrap();
        assert_eq!(PageHeader::decode(&page), Err(CodecError::BadKind));
    }

    #[test]
    fn a_valid_kind_with_a_stale_checksum_is_a_mismatch() {
        let mut page = sealed_page();
        put_u16(&mut page, KIND_OFFSET, PageKind::Overflow.as_u16()).unwrap();
        assert_eq!(PageHeader::decode(&page), Err(CodecError::ChecksumMismatch));
        seal(&mut page).unwrap();
        assert_eq!(
            PageHeader::decode(&page).unwrap().kind(),
            PageKind::Overflow
        );
    }

    #[test]
    fn zeroed_and_arbitrary_pages_are_rejected() {
        assert_eq!(
            PageHeader::decode(&[0; PAGE_SIZE]),
            Err(CodecError::BadMagic)
        );
        assert!(PageHeader::decode(&[0xA5; PAGE_SIZE]).is_err());
    }

    #[test]
    fn short_encode_and_seal_do_not_write() {
        let header = sample_header();
        let mut buf = [0x11; 16];
        assert_eq!(header.encode(&mut buf), Err(CodecError::Truncated));
        assert_eq!(buf, [0x11; 16]);
        assert_eq!(seal(&mut buf), Err(CodecError::Truncated));
        assert_eq!(buf, [0x11; 16]);
    }

    #[test]
    fn prefix_of_a_sealed_page_is_truncated() {
        let page = sealed_page();
        assert_eq!(PageHeader::decode(&page[..100]), Err(CodecError::Truncated));
    }
}
