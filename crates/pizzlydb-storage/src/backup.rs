//! Copy one committed snapshot, and replace a database from that copy.
//!
//! [`backup`] writes the page images of one snapshot and a new empty log.
//! [`restore`] checks that copy, then replaces a live path. [`publish`] is the
//! directory rename both restore and later graph tools use. [`export_checkpoint`]
//! copies a main file whose log cannot be opened, and does not write the source.

use std::path::{Path, PathBuf};

use crate::engine::{
    STATE_PAGE_ID, Storage, StorageError, decode_state, map_super, map_wal, parent_dir,
    random_file_id, wal_path,
};
use crate::page::{Lsn, PAGE_SIZE, PageId};
use crate::superblock::{COPY_A, COPY_B, Superblock};
use crate::vfs::{FileRole, OpenOptions, Vfs, VfsError, VfsFile};
use crate::wal::Wal;

/// Pages copied for one snapshot.
///
/// `bytes_written` counts the main file only. The empty log is not included.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use pizzlydb_storage::{backup, Durability, MemoryVfs, Storage};
///
/// let vfs = MemoryVfs::new();
/// let db = Storage::create(&vfs, Path::new("src"), 4, Durability::Full, |id| id.fill(1)).unwrap();
/// let mut batch = db.begin_write().unwrap();
/// batch.put(b"a", b"1").unwrap();
/// batch.commit(Durability::Full).unwrap();
/// drop(db);
///
/// let report = backup(&vfs, Path::new("src"), Path::new("bak")).unwrap();
/// assert!(report.pages >= 64);
/// assert_eq!(report.bytes_written, report.pages * 8192);
/// let opened = Storage::open(&vfs, Path::new("bak"), 4, Durability::Full).unwrap();
/// assert_eq!(opened.snapshot().unwrap().get(b"a").unwrap().as_deref(), Some(&b"1"[..]));
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackupReport {
    /// Pages in the snapshot, including unused pages below the high-water mark.
    pub pages: u64,
    /// Bytes read from the source while copying those pages.
    pub bytes_read: u64,
    /// Bytes written to the destination main file.
    pub bytes_written: u64,
    /// Snapshot sequence stored as the destination checkpoint.
    pub snapshot: Lsn,
}

/// Copy the committed snapshot at `source` to a new database at `destination`.
///
/// The destination log is empty and starts with a new file identity. Commits
/// made on `source` after this call returns are not in the destination.
/// `source` is not written.
///
/// # Limitations
///
/// - **Offline only.** The source is opened read-write, and nothing excludes a
///   live writer. [Read-only open](https://github.com/gabor-boros/pizzlydb/blob/main/docs/roadmap/015-exclusive-ownership-and-read-only-open.md)
///   and [online backup](https://github.com/gabor-boros/pizzlydb/blob/main/docs/roadmap/105-online-consistent-backup.md)
///   are later work.
/// - **Every namespace.** The copy is the whole page file, not one namespace.
///   A single-namespace copy is [salvage](https://github.com/gabor-boros/pizzlydb/blob/main/docs/roadmap/109-repair-procedures-and-salvage-hardening.md).
/// - **One snapshot.** This is one committed image, not a stream of
///   transactions. Later commits need [online backup](https://github.com/gabor-boros/pizzlydb/blob/main/docs/roadmap/105-online-consistent-backup.md).
/// - **Not an in-memory script.** Pages are copied from the source file. The
///   in-memory input limit belongs to [bulk load](https://github.com/gabor-boros/pizzlydb/blob/main/docs/roadmap/110-bounded-memory-bulk-loader.md).
///
/// # Errors
///
/// Returns [`StorageError::Io`] when a path is missing, the paths are equal,
/// or `destination` already exists. Returns [`StorageError::Corrupt`] when a
/// page image does not belong to the snapshot.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use pizzlydb_storage::{backup, Durability, MemoryVfs, Storage};
///
/// let vfs = MemoryVfs::new();
/// let db = Storage::create(&vfs, Path::new("src"), 4, Durability::Full, |id| id.fill(1)).unwrap();
/// drop(db);
/// let report = backup(&vfs, Path::new("src"), Path::new("copy")).unwrap();
/// assert_eq!(report.snapshot, pizzlydb_storage::Lsn::new(0));
/// ```
pub fn backup<V: Vfs>(
    vfs: &V,
    source: &Path,
    destination: &Path,
) -> Result<BackupReport, StorageError> {
    if source == destination {
        return Err(StorageError::Io(VfsError::InvalidPath));
    }

    let db = Storage::open(vfs, source, 8, crate::Durability::Full)?;
    let view = db.snapshot()?;
    let lsn = view.sequence();
    let state = db.page_image(lsn, STATE_PAGE_ID)?;
    let (root, pages) = decode_state(&state)?;
    let file_id = random_file_id()?;
    let (allocator, freemap) = db.superblock_slots();
    let page_len = page_len()?;

    let mut out = vfs.open(
        destination,
        FileRole::Main,
        &OpenOptions::new().create(true).exclusive(true),
    )?;
    let mut bytes_read = 0u64;
    for raw in 0..pages {
        let image = db.page_image(lsn, PageId::new(raw))?;
        let len = u64::try_from(image.len()).map_err(|_| StorageError::Corrupt)?;
        bytes_read = bytes_read.checked_add(len).ok_or(StorageError::Corrupt)?;
        let offset = raw.checked_mul(page_len).ok_or(StorageError::Corrupt)?;
        out.write_all(offset, &image)?;
    }
    drop(view);

    let primary =
        Superblock::new(file_id, 2, lsn, pages, allocator, freemap, root).map_err(map_super)?;
    let secondary =
        Superblock::new(file_id, 1, lsn, pages, allocator, freemap, root).map_err(map_super)?;
    let page_a = primary.encode(COPY_A).map_err(map_super)?;
    let page_b = secondary.encode(COPY_B).map_err(map_super)?;
    out.write_all(0, &page_a)?;
    out.write_all(page_len, &page_b)?;
    out.sync()?;
    drop(out);

    let wal_file = vfs.open(
        &wal_path(destination),
        FileRole::Wal,
        &OpenOptions::new().create(true).exclusive(true),
    )?;
    drop(Wal::create(wal_file, file_id).map_err(map_wal)?);
    vfs.sync_dir(parent_dir(destination))?;

    let bytes_written = pages.checked_mul(page_len).ok_or(StorageError::Corrupt)?;
    Ok(BackupReport {
        pages,
        bytes_read,
        bytes_written,
        snapshot: lsn,
    })
}

/// Replace `live` with a checked copy of `backup_path`.
///
/// `backup_path` is only read. The previous `live` bytes stay in place when
/// the check or the directory publish fails. The published database gets a
/// new file identity, so it does not accept `backup_path`'s log.
///
/// # Limitations
///
/// - **Offline only.** Nothing excludes a live writer on either path.
///   [Read-only open](https://github.com/gabor-boros/pizzlydb/blob/main/docs/roadmap/015-exclusive-ownership-and-read-only-open.md)
///   is later work.
/// - **Every namespace.** Restore publishes the whole copy.
/// - **One snapshot.** The published image is the backup's snapshot.
///   [Online backup](https://github.com/gabor-boros/pizzlydb/blob/main/docs/roadmap/105-online-consistent-backup.md)
///   owns a continuous copy.
/// - **Not an in-memory script.** Both files are copied from disk.
///
/// # Errors
///
/// Returns [`StorageError::Io`] when the paths are equal or a file is missing,
/// and [`StorageError::Corrupt`] when [`verify_quick`](crate::verify_quick) fails.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use pizzlydb_storage::{backup, restore, Durability, MemoryVfs, Storage};
///
/// let vfs = MemoryVfs::new();
/// let db = Storage::create(&vfs, Path::new("src"), 4, Durability::Full, |id| id.fill(1)).unwrap();
/// let mut batch = db.begin_write().unwrap();
/// batch.put(b"a", b"1").unwrap();
/// batch.commit(Durability::Full).unwrap();
/// drop(db);
/// backup(&vfs, Path::new("src"), Path::new("bak")).unwrap();
/// restore(&vfs, Path::new("bak"), Path::new("live")).unwrap();
/// let live = Storage::open(&vfs, Path::new("live"), 4, Durability::Full).unwrap();
/// assert_eq!(live.snapshot().unwrap().get(b"a").unwrap().as_deref(), Some(&b"1"[..]));
/// ```
pub fn restore<V: Vfs>(vfs: &V, backup_path: &Path, live: &Path) -> Result<(), StorageError> {
    if backup_path == live {
        return Err(StorageError::Io(VfsError::InvalidPath));
    }

    crate::verify::verify_quick(vfs, backup_path)?;
    let partial = suffix_path(live, ".partial");
    copy_file(vfs, backup_path, &partial, FileRole::Main)?;
    copy_file(
        vfs,
        &wal_path(backup_path),
        &wal_path(&partial),
        FileRole::Wal,
    )?;
    assign_fresh_identity(vfs, &partial)?;
    publish(vfs, &partial, live)
}

/// Rename `temporary` and its log onto `destination`, then sync the directory.
///
/// An existing destination is moved aside and put back when a later step fails.
/// After a successful sync, the aside files are removed. Equal paths are rejected.
///
/// # Errors
///
/// Returns [`StorageError::Io`] when a rename or the directory sync fails. On
/// that error the previous destination bytes are restored unless the restoring
/// rename also fails.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use pizzlydb_storage::{publish, Durability, MemoryVfs, Storage};
///
/// let vfs = MemoryVfs::new();
/// let db = Storage::create(&vfs, Path::new("partial"), 4, Durability::Full, |id| id.fill(1)).unwrap();
/// drop(db);
/// publish(&vfs, Path::new("partial"), Path::new("live")).unwrap();
/// assert!(Storage::open(&vfs, Path::new("live"), 4, Durability::Full).is_ok());
/// ```
pub fn publish<V: Vfs>(vfs: &V, temporary: &Path, destination: &Path) -> Result<(), StorageError> {
    if temporary == destination {
        return Err(StorageError::Io(VfsError::InvalidPath));
    }

    let mut done = Vec::new();
    if let Err(err) = relocate(vfs, temporary, destination, &mut done) {
        return undo(vfs, &done, err);
    }

    match vfs.sync_dir(parent_dir(destination)) {
        Ok(()) => {
            let _ = vfs.remove(&replaced_path(destination), FileRole::Main);
            let _ = vfs.remove(&replaced_path(&wal_path(destination)), FileRole::Wal);
            Ok(())
        }
        Err(err) => undo(vfs, &done, StorageError::Io(err)),
    }
}

/// Copy the source main file to `copy_path` and create an empty log beside it.
///
/// The source path and its log are not written. When neither superblock
/// decodes, `copy_path` is not created.
///
/// # Errors
///
/// Returns [`StorageError::Corrupt`] when neither superblock decodes, and
/// [`StorageError::Io`] when the source cannot be read or `copy_path` exists.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use pizzlydb_storage::{export_checkpoint, Durability, MemoryVfs, Storage};
///
/// let vfs = MemoryVfs::new();
/// let db = Storage::create(&vfs, Path::new("src"), 4, Durability::Full, |id| id.fill(1)).unwrap();
/// drop(db);
/// export_checkpoint(&vfs, Path::new("src"), Path::new("image")).unwrap();
/// assert!(Storage::open(&vfs, Path::new("image"), 4, Durability::Full).is_ok());
/// ```
pub fn export_checkpoint<V: Vfs>(
    vfs: &V,
    source: &Path,
    copy_path: &Path,
) -> Result<(), StorageError> {
    if source == copy_path {
        return Err(StorageError::Io(VfsError::InvalidPath));
    }

    let mut file = vfs.open(source, FileRole::Main, &OpenOptions::new().read_only(true))?;
    let mut first = [0u8; PAGE_SIZE];
    let mut second = [0u8; PAGE_SIZE];
    let page_len = page_len()?;
    let first_ok = file.read_exact(0, &mut first).is_ok();
    let second_ok = file.read_exact(page_len, &mut second).is_ok();
    match (first_ok, second_ok) {
        (true, true) => {
            Superblock::select(&first, &second).map_err(map_super)?;
        }
        (true, false) => {
            Superblock::decode(&first, COPY_A).map_err(map_super)?;
        }
        (false, true) => {
            Superblock::decode(&second, COPY_B).map_err(map_super)?;
        }
        (false, false) => return Err(StorageError::Corrupt),
    }
    drop(file);

    copy_file(vfs, source, copy_path, FileRole::Main)?;
    let file_id = random_file_id()?;
    restamp_main(vfs, copy_path, file_id)?;
    let wal_file = vfs.open(
        &wal_path(copy_path),
        FileRole::Wal,
        &OpenOptions::new().create(true).exclusive(true),
    )?;
    drop(Wal::create(wal_file, file_id).map_err(map_wal)?);
    vfs.sync_dir(parent_dir(copy_path))?;
    Ok(())
}

struct Move {
    from: PathBuf,
    to: PathBuf,
    role: FileRole,
}

fn relocate<V: Vfs>(
    vfs: &V,
    temporary: &Path,
    destination: &Path,
    done: &mut Vec<Move>,
) -> Result<(), StorageError> {
    park(vfs, destination, FileRole::Main, done)?;
    move_file(vfs, temporary, destination, FileRole::Main, done)?;
    park(vfs, &wal_path(destination), FileRole::Wal, done)?;
    move_file(
        vfs,
        &wal_path(temporary),
        &wal_path(destination),
        FileRole::Wal,
        done,
    )?;
    Ok(())
}

fn park<V: Vfs>(
    vfs: &V,
    path: &Path,
    role: FileRole,
    done: &mut Vec<Move>,
) -> Result<(), StorageError> {
    match vfs.rename(path, &replaced_path(path), role) {
        Ok(()) => {
            done.push(Move {
                from: path.to_path_buf(),
                to: replaced_path(path),
                role,
            });
            Ok(())
        }
        Err(VfsError::NotFound) => Ok(()),
        Err(err) => Err(err.into()),
    }
}

fn move_file<V: Vfs>(
    vfs: &V,
    from: &Path,
    to: &Path,
    role: FileRole,
    done: &mut Vec<Move>,
) -> Result<(), StorageError> {
    vfs.rename(from, to, role)?;
    done.push(Move {
        from: from.to_path_buf(),
        to: to.to_path_buf(),
        role,
    });
    Ok(())
}

fn undo<V: Vfs>(vfs: &V, done: &[Move], err: StorageError) -> Result<(), StorageError> {
    for step in done.iter().rev() {
        if let Err(undo_err) = vfs.rename(&step.to, &step.from, step.role) {
            return Err(undo_err.into());
        }
    }
    Err(err)
}

fn copy_file<V: Vfs>(vfs: &V, from: &Path, to: &Path, role: FileRole) -> Result<(), StorageError> {
    let mut src = vfs.open(from, role, &OpenOptions::new().read_only(true))?;
    let len = src.len()?;
    let mut dst = vfs.open(to, role, &OpenOptions::new().create(true).exclusive(true))?;
    let mut buf = [0u8; PAGE_SIZE];
    let mut offset = 0u64;
    while offset < len {
        let n = src.read_at(offset, &mut buf)?;
        if n == 0 {
            return Err(StorageError::Corrupt);
        }
        dst.write_all(offset, &buf[..n])?;
        offset = offset
            .checked_add(u64::try_from(n).map_err(|_| StorageError::Corrupt)?)
            .ok_or(StorageError::Corrupt)?;
    }
    dst.sync()?;
    Ok(())
}

pub(crate) fn suffix_path(path: &Path, suffix: &str) -> PathBuf {
    let mut out = path.to_path_buf();
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    out.set_file_name(name);
    out
}

fn replaced_path(path: &Path) -> PathBuf {
    suffix_path(path, ".replaced")
}

fn page_len() -> Result<u64, StorageError> {
    u64::try_from(PAGE_SIZE).map_err(|_| StorageError::Corrupt)
}

fn assign_fresh_identity<V: Vfs>(vfs: &V, path: &Path) -> Result<[u8; 16], StorageError> {
    let file_id = random_file_id()?;
    restamp_main(vfs, path, file_id)?;
    restamp_wal(vfs, path, file_id)?;
    Ok(file_id)
}

fn restamp_main<V: Vfs>(vfs: &V, path: &Path, file_id: [u8; 16]) -> Result<(), StorageError> {
    let mut file = vfs.open(path, FileRole::Main, &OpenOptions::new())?;
    let mut first = [0u8; PAGE_SIZE];
    let mut second = [0u8; PAGE_SIZE];
    let page_len = page_len()?;
    file.read_exact(0, &mut first)?;
    let second_ok = file.read_exact(page_len, &mut second).is_ok();
    let stamped_a = restamp_superblock(&first, COPY_A, file_id)?;
    let stamped_b = if second_ok {
        restamp_superblock(&second, COPY_B, file_id)?
    } else {
        None
    };

    if stamped_a.is_none() && stamped_b.is_none() {
        return Err(StorageError::Corrupt);
    }

    if let Some(page) = stamped_a {
        file.write_all(0, &page)?;
    }

    if let Some(page) = stamped_b {
        file.write_all(page_len, &page)?;
    }

    file.sync()?;
    Ok(())
}

fn restamp_superblock(
    page: &[u8],
    id: PageId,
    file_id: [u8; 16],
) -> Result<Option<[u8; PAGE_SIZE]>, StorageError> {
    let Ok(record) = Superblock::decode(page, id) else {
        return Ok(None);
    };

    let record = Superblock::new(
        file_id,
        record.generation(),
        record.checkpoint_lsn(),
        record.logical_page_count(),
        record.allocator_page_id(),
        record.free_space_map_page_id(),
        record.btree_root_page_id(),
    )
    .map_err(map_super)?;

    record.encode(id).map(Some).map_err(map_super)
}

fn restamp_wal<V: Vfs>(vfs: &V, path: &Path, file_id: [u8; 16]) -> Result<(), StorageError> {
    let mut file = vfs.open(&wal_path(path), FileRole::Wal, &OpenOptions::new())?;
    let len = usize::try_from(file.len()?).map_err(|_| StorageError::Corrupt)?;
    let mut bytes = vec![0u8; len];
    if !bytes.is_empty() {
        file.read_exact(0, &mut bytes)?;
    }

    crate::wal::restamp_header(&mut bytes, &file_id).map_err(map_wal)?;
    file.write_all(0, &bytes)?;
    file.sync()?;

    Ok(())
}
