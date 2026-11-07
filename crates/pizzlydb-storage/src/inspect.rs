//! Read a main file and stop at the first damaged page.
//!
//! [`inspect`] does not open the log and does not write the file.

use std::cell::RefCell;
use std::path::Path;

use crate::btree::{BTree, PageRead, TreeError};
use crate::codec::{CodecError, get_u64};
use crate::page::{ALLOCATOR_PAGE_ID, BITMAP_PAGE_ID, HEADER_SIZE, PAGE_SIZE, PageHeader, PageId};
use crate::space::{Space, SpaceError, SpillImage};
use crate::superblock::{COPY_A, COPY_B, Superblock};
use crate::vfs::{FileRole, OpenOptions, Vfs, VfsError, VfsFile};

const SPILL_HEAD_OFFSET: usize = PAGE_SIZE - 16;
const SPILL_NEXT_OFFSET: usize = HEADER_SIZE;
const MAX_SPILL_HOPS: usize = 4096;

/// What the first bad page failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DamageReason {
    /// The file ends before this page.
    Truncated,
    /// A superblock copy does not decode.
    Superblock,
    /// The page checksum, magic, or version does not match.
    Checksum,
    /// The header page id is not the file slot.
    PageId,
    /// The header kind is not a known page kind.
    PageKind,
    /// The allocator or bitmap pages do not load.
    Bitmap,
    /// The root does not pass the tree check.
    BTree,
}

/// The first damaged page, if any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Damage {
    /// File slot that failed.
    pub page_id: PageId,
    /// Why that slot failed.
    pub reason: DamageReason,
}

/// Logical page count and the first damaged page.
///
/// `logical_pages` is zero when the first superblock does not decode.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use pizzlydb_storage::{inspect, Durability, MemoryVfs, Storage};
///
/// let vfs = MemoryVfs::new();
/// let db = Storage::create(&vfs, Path::new("db"), 4, Durability::Full, |id| id.fill(1)).unwrap();
/// drop(db);
/// let report = inspect(&vfs, Path::new("db")).unwrap();
/// assert!(report.damage.is_none());
/// assert!(report.logical_pages >= 64);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InspectReport {
    /// Page count from the selected superblock, or zero when copy A fails.
    pub logical_pages: u64,
    /// First failure. `None` when the walk finishes.
    pub damage: Option<Damage>,
}

/// Walk `path` in page order and stop at the first failure.
///
/// The file length is unchanged. Pages that are all zeros are skipped.
///
/// # Errors
///
/// Returns [`crate::StorageError::Io`] when `path` cannot be opened. A damaged
/// page is reported in [`InspectReport`], not as an error.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use pizzlydb_storage::{inspect, Durability, MemoryVfs, Storage};
///
/// let vfs = MemoryVfs::new();
/// let db = Storage::create(&vfs, Path::new("db"), 4, Durability::Full, |id| id.fill(1)).unwrap();
/// drop(db);
/// assert!(inspect(&vfs, Path::new("db")).unwrap().damage.is_none());
/// ```
pub fn inspect<V: Vfs>(vfs: &V, path: &Path) -> Result<InspectReport, crate::StorageError> {
    let mut file = vfs.open(path, FileRole::Main, &OpenOptions::new().read_only(true))?;
    let len = file.len().map_err(crate::StorageError::from)?;
    let page_len = u64::try_from(PAGE_SIZE).map_err(|_| crate::StorageError::Corrupt)?;
    if len < page_len.saturating_mul(2) {
        return Ok(report(
            0,
            Some(damage(PageId::new(0), DamageReason::Truncated)),
        ));
    }

    let mut page0 = [0u8; PAGE_SIZE];
    if let Err(err) = file.read_exact(0, &mut page0) {
        return read_failure(err, 0, PageId::new(0));
    }
    let copy_a = match Superblock::decode(&page0, COPY_A) {
        Ok(record) => record,
        Err(_) => {
            return Ok(report(
                0,
                Some(damage(PageId::new(0), DamageReason::Superblock)),
            ));
        }
    };

    let mut page1 = [0u8; PAGE_SIZE];
    if let Err(err) = file.read_exact(page_len, &mut page1) {
        return read_failure(err, copy_a.logical_page_count(), PageId::new(1));
    }
    if Superblock::decode(&page1, COPY_B).is_err() {
        return Ok(report(
            copy_a.logical_page_count(),
            Some(damage(PageId::new(1), DamageReason::Superblock)),
        ));
    }
    let (selected, _) = match Superblock::select(&page0, &page1) {
        Ok(pair) => pair,
        Err(_) => {
            return Ok(report(
                copy_a.logical_page_count(),
                Some(damage(PageId::new(0), DamageReason::Superblock)),
            ));
        }
    };
    let logical = selected.logical_page_count();
    let Some(logical_bytes) = logical.checked_mul(page_len) else {
        return Ok(report(
            logical,
            Some(damage(PageId::new(len / page_len), DamageReason::Truncated)),
        ));
    };
    if len < logical_bytes {
        return Ok(report(
            logical,
            Some(damage(PageId::new(len / page_len), DamageReason::Truncated)),
        ));
    }

    let mut allocator = None;
    let mut bitmap = None;
    for raw in 0..logical {
        let id = PageId::new(raw);
        let mut image = [0u8; PAGE_SIZE];
        if file.read_exact(raw * page_len, &mut image).is_err() {
            return Ok(report(logical, Some(damage(id, DamageReason::Truncated))));
        }
        if image.iter().all(|byte| *byte == 0) {
            continue;
        }
        if let Some(reason) = header_damage(&image, id) {
            return Ok(report(logical, Some(damage(id, reason))));
        }
        if id == ALLOCATOR_PAGE_ID {
            allocator = Some(image);
        }
        if id == BITMAP_PAGE_ID {
            bitmap = Some(image);
        }
    }

    let (Some(header), Some(bitmap_page)) = (allocator, bitmap) else {
        return Ok(report(
            logical,
            Some(damage(BITMAP_PAGE_ID, DamageReason::Bitmap)),
        ));
    };
    let spills = match read_spills(&mut file, &header) {
        Ok(pages) => pages,
        Err(_) => {
            return Ok(report(
                logical,
                Some(damage(BITMAP_PAGE_ID, DamageReason::Bitmap)),
            ));
        }
    };
    if Space::load(&header, &bitmap_page, &spills).is_err() {
        return Ok(report(
            logical,
            Some(damage(BITMAP_PAGE_ID, DamageReason::Bitmap)),
        ));
    }

    let root = selected.btree_root_page_id();
    if root.as_u64() != 0 {
        let pages = FilePages {
            file: RefCell::new(file),
        };
        if BTree::from_root(root).verify(&pages).is_err() {
            return Ok(report(logical, Some(damage(root, DamageReason::BTree))));
        }
    }

    Ok(report(logical, None))
}

fn header_damage(image: &[u8], id: PageId) -> Option<DamageReason> {
    match PageHeader::decode(image) {
        Ok(header) if header.page_id() == id => None,
        Ok(_) => Some(DamageReason::PageId),
        Err(CodecError::BadKind) => Some(DamageReason::PageKind),
        Err(CodecError::Truncated) => Some(DamageReason::Truncated),
        Err(_) => Some(DamageReason::Checksum),
    }
}

fn read_failure(
    err: VfsError,
    logical_pages: u64,
    page_id: PageId,
) -> Result<InspectReport, crate::StorageError> {
    match err {
        VfsError::Truncated => Ok(report(
            logical_pages,
            Some(damage(page_id, DamageReason::Truncated)),
        )),
        other => Err(other.into()),
    }
}

fn read_spills<F: VfsFile>(
    file: &mut F,
    header: &[u8; PAGE_SIZE],
) -> Result<Vec<SpillImage>, SpaceError> {
    let mut head = get_u64(header, SPILL_HEAD_OFFSET).map_err(|_| SpaceError::Corrupt)?;
    let mut spills = Vec::new();
    for _ in 0..MAX_SPILL_HOPS {
        if head == 0 {
            return Ok(spills);
        }
        let id = PageId::new(head);
        let mut bytes = [0u8; PAGE_SIZE];
        let offset = head
            .checked_mul(u64::try_from(PAGE_SIZE).map_err(|_| SpaceError::Corrupt)?)
            .ok_or(SpaceError::Corrupt)?;
        file.read_exact(offset, &mut bytes)
            .map_err(|_| SpaceError::Corrupt)?;
        let next = get_u64(&bytes, SPILL_NEXT_OFFSET).map_err(|_| SpaceError::Corrupt)?;
        spills.push(SpillImage { page_id: id, bytes });
        head = next;
    }
    Err(SpaceError::Corrupt)
}

fn damage(page_id: PageId, reason: DamageReason) -> Damage {
    Damage { page_id, reason }
}

fn report(logical_pages: u64, damage: Option<Damage>) -> InspectReport {
    InspectReport {
        logical_pages,
        damage,
    }
}

struct FilePages<F: VfsFile> {
    file: RefCell<F>,
}

impl<F: VfsFile> PageRead for FilePages<F> {
    type Pin<'a>
        = Vec<u8>
    where
        Self: 'a;

    fn pin(&self, id: PageId) -> Result<Self::Pin<'_>, TreeError> {
        let offset = id
            .as_u64()
            .checked_mul(u64::try_from(PAGE_SIZE).map_err(|_| TreeError::Corrupt)?)
            .ok_or(TreeError::Corrupt)?;
        let mut image = vec![0u8; PAGE_SIZE];
        self.file
            .borrow_mut()
            .read_exact(offset, &mut image)
            .map_err(|_| TreeError::Corrupt)?;
        let header = PageHeader::decode(&image).map_err(|_| TreeError::Corrupt)?;
        if header.page_id() != id {
            return Err(TreeError::Corrupt);
        }
        Ok(image)
    }
}
