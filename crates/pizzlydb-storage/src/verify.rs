//! Check that a database file opens and that its tree pages are allocated.
//!
//! [`verify_quick`] reads one committed snapshot. It does not check graph keys.
//! A caller that needs those checks runs them in `pizzlydb-graph` before restore.

use std::path::Path;

use crate::engine::{STATE_PAGE_ID, Storage, StorageError, decode_state};
use crate::page::{PAGE_SIZE, PageHeader, PageId};
use crate::vfs::{Vfs, VfsError};

/// Open `path` and check the committed snapshot.
///
/// The file length must cover the superblock page count. Each page at or below
/// the state high-water mark is either all zeros and unallocated, or a page
/// whose header matches that id. Every tree page the root reaches is allocated.
///
/// # Limitations
///
/// - **Offline only.** The path is opened read-only and takes a shared lock.
///   A live read-write opener returns [`StorageError::Locked`].
/// - **Every namespace.** The check is the page file, not one graph namespace.
///   Graph agreement is `pizzlydb_graph::verify_full`. Online scrubbing is
///   [full verification](https://github.com/gabor-boros/pizzlydb/blob/main/docs/roadmap/107-full-verification-and-online-scrubbing.md).
/// - **One snapshot.** The check reads one committed sequence, not every
///   historical transaction.
/// - **Not an in-memory script.** The input is the database file. An
///   in-memory load belongs to [bulk load](https://github.com/gabor-boros/pizzlydb/blob/main/docs/roadmap/110-bounded-memory-bulk-loader.md).
///
/// # Errors
///
/// Returns [`StorageError::Corrupt`] when a header, allocation bit, or tree
/// link is wrong, and [`StorageError::Io`] when `path` cannot be opened.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use pizzlydb_storage::{verify_quick, Durability, MemoryVfs, Storage};
///
/// let vfs = MemoryVfs::new();
/// let db = Storage::create(&vfs, Path::new("db"), 4, Durability::Full, |id| id.fill(1)).unwrap();
/// drop(db);
/// assert!(verify_quick(&vfs, Path::new("db")).is_ok());
/// ```
pub fn verify_quick<V: Vfs>(vfs: &V, path: &Path) -> Result<(), StorageError> {
    let db = Storage::open_read_only(vfs, path, 8, crate::Durability::Full)?;
    let view = db.snapshot()?;
    let lsn = view.sequence();
    let state = db.page_image(lsn, STATE_PAGE_ID)?;
    let (root, high_water) = decode_state(&state)?;
    let logical = db.logical_pages();
    let page_len = u64::try_from(PAGE_SIZE).map_err(|_| StorageError::Corrupt)?;
    let logical_bytes = logical.checked_mul(page_len).ok_or(StorageError::Corrupt)?;
    if db.main_len()? < logical_bytes {
        return Err(StorageError::Corrupt);
    }

    let space = db.committed_space();
    for raw in 0..high_water {
        let id = PageId::new(raw);
        let image = match db.page_image(lsn, id) {
            Ok(image) => image,
            // A grown high-water mark can name pages the main file does not
            // store yet. Those pages are holes until a checkpoint extends it.
            Err(StorageError::Io(VfsError::Truncated)) if !space.is_allocated(id) => continue,
            Err(StorageError::Io(VfsError::Truncated)) => return Err(StorageError::Corrupt),
            Err(err) => return Err(err),
        };

        if image.iter().all(|byte| *byte == 0) {
            if space.is_allocated(id) {
                return Err(StorageError::Corrupt);
            }
            continue;
        }

        let header = match PageHeader::decode(&image) {
            Ok(header) => header,
            Err(crate::codec::CodecError::ChecksumMismatch) => {
                return Err(StorageError::ChecksumMismatch { page_id: raw });
            }
            Err(_) => return Err(StorageError::Corrupt),
        };

        if header.page_id() != id || header.page_id().as_u64() >= high_water {
            return Err(StorageError::Corrupt);
        }
    }
    db.verify_tree(lsn, root, &space)?;
    drop(view);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::verify_quick;
    use crate::engine::{STATE_PAGE_ID, decode_state};
    use crate::page::{BITMAP_PAGE_ID, HEADER_SIZE, PAGE_SIZE, PageHeader, PageKind, seal};
    use crate::vfs::{FileRole, OpenOptions, Vfs, VfsFile};
    use crate::vfs_memory::MemoryVfs;
    use crate::{Durability, Storage, StorageError};

    fn committed(keys: usize) -> MemoryVfs {
        let vfs = MemoryVfs::new();
        let db =
            Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1)).unwrap();
        let mut batch = db.begin_write().unwrap();
        for index in 0..keys {
            let key = format!("{index:08}");
            batch.put(key.as_bytes(), b"value-xx").unwrap();
        }
        batch.commit(Durability::Full).unwrap();
        db.checkpoint().unwrap();
        drop(db);
        vfs
    }

    fn read_page(vfs: &MemoryVfs, id: u64) -> [u8; PAGE_SIZE] {
        let mut file = vfs
            .open(Path::new("db"), FileRole::Main, &OpenOptions::new())
            .unwrap();
        let mut page = [0u8; PAGE_SIZE];
        file.read_exact(id * u64::try_from(PAGE_SIZE).unwrap(), &mut page)
            .unwrap();
        page
    }

    fn write_page(vfs: &MemoryVfs, id: u64, page: &[u8; PAGE_SIZE]) {
        let mut file = vfs
            .open(Path::new("db"), FileRole::Main, &OpenOptions::new())
            .unwrap();
        file.write_all(id * u64::try_from(PAGE_SIZE).unwrap(), page)
            .unwrap();
        file.sync().unwrap();
    }

    fn find_kind(vfs: &MemoryVfs, kind: PageKind) -> Option<(u64, [u8; PAGE_SIZE])> {
        let file = vfs
            .open(Path::new("db"), FileRole::Main, &OpenOptions::new())
            .unwrap();
        let pages = file.len().unwrap() / u64::try_from(PAGE_SIZE).unwrap();
        drop(file);
        (0..pages).find_map(|id| {
            let page = read_page(vfs, id);
            let header = PageHeader::decode(&page).ok()?;
            (header.kind() == kind).then_some((id, page))
        })
    }

    #[test]
    fn checksum_separator_and_bitmap_faults_are_corrupt() {
        let vfs = committed(1);
        let (id, mut page) = find_kind(&vfs, PageKind::BTreeLeaf).unwrap();
        page[HEADER_SIZE] ^= 0x01;
        write_page(&vfs, id, &page);
        assert!(matches!(
            verify_quick(&vfs, Path::new("db")),
            Err(StorageError::ChecksumMismatch { page_id }) if page_id == id
        ));

        let vfs = committed(800);
        let (id, mut page) = find_kind(&vfs, PageKind::BTreeInternal).expect("internal page");
        page[HEADER_SIZE + 44] ^= 0xff;
        seal(&mut page).unwrap();
        write_page(&vfs, id, &page);
        assert!(matches!(
            verify_quick(&vfs, Path::new("db")),
            Err(StorageError::Corrupt)
        ));

        let vfs = committed(1);
        let state = read_page(&vfs, STATE_PAGE_ID.as_u64());
        let (root, _) = decode_state(&state).unwrap();
        let mut bitmap = read_page(&vfs, BITMAP_PAGE_ID.as_u64());
        let raw = root.as_u64();
        let byte = HEADER_SIZE + usize::try_from(raw / 8).unwrap();
        let bit = u32::try_from(raw % 8).unwrap();
        bitmap[byte] &= !(1u8 << bit);
        seal(&mut bitmap).unwrap();
        write_page(&vfs, BITMAP_PAGE_ID.as_u64(), &bitmap);
        assert!(matches!(
            verify_quick(&vfs, Path::new("db")),
            Err(StorageError::Corrupt)
        ));
    }
}
