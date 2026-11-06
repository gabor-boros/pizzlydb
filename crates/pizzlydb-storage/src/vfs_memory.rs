//! Single-threaded in-memory [`Vfs`](crate::vfs::Vfs) for tests.
//!
//! Files are keyed by path and role. The path is stored as given and is not
//! normalized. Bytes written by [`VfsFile::write_at`](crate::vfs::VfsFile::write_at)
//! are visible to later reads on that file. [`VfsFile::sync`](crate::vfs::VfsFile::sync)
//! copies those bytes into the durable buffer. [`Vfs::sync_dir`](crate::vfs::Vfs::sync_dir)
//! publishes current names with the durable bytes already recorded; it does not
//! publish an unsynced tail.
//!
//! [`MemoryVfs::crash`] discards every byte that is not in the durable directory
//! and invalidates open handles. [`MemoryVfs::remove`] and [`MemoryVfs::replace`]
//! fail while a handle is open.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::vfs::{
    FileRole, OpenOptions, Vfs, VfsError, VfsFile, io_error, read_only_error, validate_open,
    validate_path,
};

#[derive(Debug)]
struct MemFile {
    visible: Vec<u8>,
    /// Bytes preserved by [`MemoryVfs::crash`].
    durable: Vec<u8>,
    open_count: usize,
    /// Set by `sync` and `sync_dir`. Unpublished files disappear in `crash`.
    published: bool,
}

impl MemFile {
    fn empty() -> Self {
        Self {
            visible: Vec::new(),
            durable: Vec::new(),
            open_count: 1,
            published: false,
        }
    }
}

#[derive(Debug)]
struct MemoryState {
    files: HashMap<(PathBuf, FileRole), MemFile>,
    /// Bumped by [`MemoryVfs::crash`]. Handles store the value from `open`.
    generation: u64,
}

/// Process-local files used by tests.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use pizzlydb_storage::vfs::{FileRole, MemoryVfs, OpenOptions, Vfs, VfsFile};
///
/// let vfs = MemoryVfs::new();
/// let mut file = vfs.open(
///     Path::new("db"),
///     FileRole::Main,
///     &OpenOptions::new().create(true),
/// ).unwrap();
///
/// file.write_all(0, b"hello").unwrap();
/// file.sync().unwrap();
/// file.write_all(5, b" world").unwrap();
/// vfs.crash();
/// drop(file);
///
/// let mut restored = vfs.open(
///     Path::new("db"),
///     FileRole::Main,
///     &OpenOptions::new(),
/// ).unwrap();
///
/// let mut buf = [0; 5];
/// restored.read_exact(0, &mut buf).unwrap();
///
/// assert_eq!(&buf, b"hello");
/// ```
#[derive(Debug)]
pub struct MemoryVfs {
    state: Rc<RefCell<MemoryState>>,
}

impl Default for MemoryVfs {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryVfs {
    /// Empty store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Rc::new(RefCell::new(MemoryState {
                files: HashMap::new(),
                generation: 0,
            })),
        }
    }

    /// Discard unsynced bytes and invalidate open handles.
    ///
    /// Files that have never been published by [`VfsFile::sync`](crate::vfs::VfsFile::sync)
    /// or [`Vfs::sync_dir`](crate::vfs::Vfs::sync_dir) are removed. A published file
    /// keeps its durable bytes, not its visible tail. Handles opened before the
    /// call then fail with [`VfsError::Io`](crate::vfs::VfsError::Io). Dropping
    /// those handles does not change the restored open count.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::vfs::{FileRole, MemoryVfs, OpenOptions, Vfs, VfsFile};
    ///
    /// let vfs = MemoryVfs::new();
    /// let mut file = vfs.open(
    ///     Path::new("db"),
    ///     FileRole::Wal,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// file.write_all(0, b"wal").unwrap();
    /// drop(file);
    /// vfs.crash();
    ///
    /// let missing = vfs.open(
    ///     Path::new("db"),
    ///     FileRole::Wal,
    ///     &OpenOptions::new(),
    /// ).unwrap_err();
    ///
    /// assert!(matches!(missing, pizzlydb_storage::vfs::VfsError::NotFound));
    /// ```
    pub fn crash(&self) {
        let mut state = self.state.borrow_mut();
        state.generation = state.generation.wrapping_add(1);

        let previous = std::mem::take(&mut state.files);
        for (key, file) in previous {
            if !file.published {
                continue;
            }

            state.files.insert(
                key,
                MemFile {
                    visible: file.durable.clone(),
                    durable: file.durable,
                    open_count: 0,
                    published: true,
                },
            );
        }
    }

    /// Delete `path` for `role`.
    ///
    /// # Errors
    ///
    /// Returns [`VfsError::NotFound`](crate::vfs::VfsError::NotFound) when the
    /// file is absent, [`VfsError::InvalidPath`](crate::vfs::VfsError::InvalidPath)
    /// for an empty path or a NUL byte, and [`VfsError::Io`](crate::vfs::VfsError::Io)
    /// while a handle is open.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::vfs::{FileRole, MemoryVfs, OpenOptions, Vfs};
    ///
    /// let vfs = MemoryVfs::new();
    /// let file = vfs.open(
    ///     Path::new("db"),
    ///     FileRole::Main,
    ///     &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// drop(file);
    /// vfs.remove(Path::new("db"), FileRole::Main).unwrap();
    /// ```
    pub fn remove(&self, path: &Path, role: FileRole) -> Result<(), VfsError> {
        let key = file_key(path, role)?;

        let mut state = self.state.borrow_mut();
        let open_count = state.files.get(&key).ok_or(VfsError::NotFound)?.open_count;
        if open_count > 0 {
            return Err(io_error("file is open"));
        }

        state.files.remove(&key);
        Ok(())
    }

    /// Replace both the visible and durable bytes of an existing file.
    ///
    /// The file is published, so [`Self::crash`] keeps `bytes`.
    ///
    /// # Errors
    ///
    /// Returns [`VfsError::NotFound`](crate::vfs::VfsError::NotFound) when the
    /// file is absent, [`VfsError::InvalidPath`](crate::vfs::VfsError::InvalidPath)
    /// for an empty path or a NUL byte, and [`VfsError::Io`](crate::vfs::VfsError::Io)
    /// while a handle is open.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::vfs::{FileRole, MemoryVfs, OpenOptions, Vfs, VfsFile};
    ///
    /// let vfs = MemoryVfs::new();
    /// let file = vfs.open(
    ///      Path::new("db"),
    ///      FileRole::Main,
    ///      &OpenOptions::new().create(true),
    /// ).unwrap();
    ///
    /// drop(file);
    /// vfs.replace(Path::new("db"), FileRole::Main, b"zz").unwrap();
    ///
    /// let mut file = vfs.open(
    ///      Path::new("db"),
    ///      FileRole::Main,
    ///      &OpenOptions::new(),
    /// ).unwrap();
    ///
    /// let mut buf = [0; 2];
    /// file.read_exact(0, &mut buf).unwrap();
    /// assert_eq!(&buf, b"zz");
    /// ```
    pub fn replace(&self, path: &Path, role: FileRole, bytes: &[u8]) -> Result<(), VfsError> {
        let key = file_key(path, role)?;

        let mut state = self.state.borrow_mut();
        let file = state.files.get_mut(&key).ok_or(VfsError::NotFound)?;
        if file.open_count > 0 {
            return Err(io_error("file is open"));
        }

        file.visible.clear();
        file.visible.extend_from_slice(bytes);
        file.durable.clone_from(&file.visible);
        file.published = true;

        Ok(())
    }
}

fn file_key(path: &Path, role: FileRole) -> Result<(PathBuf, FileRole), VfsError> {
    validate_path(path)?;
    Ok((path.to_path_buf(), role))
}

impl Vfs for MemoryVfs {
    type File = MemoryFile;

    fn open(
        &self,
        path: &Path,
        role: FileRole,
        options: &OpenOptions,
    ) -> Result<Self::File, VfsError> {
        validate_open(path, options)?;
        let key = (path.to_path_buf(), role);

        let mut state = self.state.borrow_mut();
        match state.files.get_mut(&key) {
            Some(file) => {
                if options.is_exclusive() {
                    return Err(VfsError::AlreadyExists);
                }
                file.open_count = file
                    .open_count
                    .checked_add(1)
                    .ok_or_else(|| io_error("too many open handles"))?;
            }
            None => {
                if !options.is_create() {
                    return Err(VfsError::NotFound);
                }
                state.files.insert(key.clone(), MemFile::empty());
            }
        }

        Ok(MemoryFile {
            state: Rc::clone(&self.state),
            key,
            generation: state.generation,
            read_only: options.is_read_only(),
            open: true,
        })
    }

    fn sync_dir(&self, directory: &Path) -> Result<(), VfsError> {
        validate_path(directory)?;

        let mut state = self.state.borrow_mut();
        for file in state.files.values_mut() {
            file.published = true;
        }

        Ok(())
    }
}

/// Handle for one file in a [`MemoryVfs`].
///
/// Dropping the handle releases it and does not sync. [`VfsFile::close`](crate::vfs::VfsFile::close)
/// syncs, then releases, and returns the sync error if there is one.
#[derive(Debug)]
pub struct MemoryFile {
    state: Rc<RefCell<MemoryState>>,
    key: (PathBuf, FileRole),
    generation: u64,
    read_only: bool,
    open: bool,
}

impl MemoryFile {
    fn with_open_file<T>(
        &self,
        body: impl FnOnce(&mut MemFile) -> Result<T, VfsError>,
    ) -> Result<T, VfsError> {
        if !self.open {
            return Err(io_error("file is closed"));
        }

        let mut state = self.state.borrow_mut();
        if state.generation != self.generation {
            return Err(io_error("file handle is stale"));
        }

        let file = state
            .files
            .get_mut(&self.key)
            .ok_or_else(|| io_error("file handle is stale"))?;

        body(file)
    }

    fn release(&mut self) {
        if !self.open {
            return;
        }

        self.open = false;

        // A borrow still held during unwind means this handle cannot update the
        // count. The handle is already marked closed, so Drop will not retry.
        let Ok(mut state) = self.state.try_borrow_mut() else {
            return;
        };

        if state.generation != self.generation {
            return;
        }

        if let Some(file) = state.files.get_mut(&self.key) {
            debug_assert!(file.open_count > 0);
            file.open_count = file.open_count.saturating_sub(1);
        }
    }
}

impl VfsFile for MemoryFile {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, VfsError> {
        self.with_open_file(|file| {
            if buf.is_empty() {
                return Ok(0);
            }

            // An offset that does not fit in `usize` is past the end of any
            // in-memory buffer.
            let Ok(start) = usize::try_from(offset) else {
                return Ok(0);
            };

            if start >= file.visible.len() {
                return Ok(0);
            }

            let n = (file.visible.len() - start).min(buf.len());
            buf[..n].copy_from_slice(&file.visible[start..start + n]);
            Ok(n)
        })
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<usize, VfsError> {
        let read_only = self.read_only;
        self.with_open_file(|file| {
            if read_only {
                return Err(read_only_error());
            }

            if data.is_empty() {
                return Ok(0);
            }

            let start = usize::try_from(offset).map_err(|_| io_error("offset overflow"))?;
            let end = start
                .checked_add(data.len())
                .ok_or_else(|| io_error("offset overflow"))?;

            if end > file.visible.len() {
                file.visible.resize(end, 0);
            }

            file.visible[start..end].copy_from_slice(data);
            Ok(data.len())
        })
    }

    fn sync(&mut self) -> Result<(), VfsError> {
        self.with_open_file(|file| {
            file.durable.clone_from(&file.visible);
            file.published = true;
            Ok(())
        })
    }

    fn len(&self) -> Result<u64, VfsError> {
        self.with_open_file(|file| {
            u64::try_from(file.visible.len()).map_err(|_| io_error("length overflow"))
        })
    }

    fn close(mut self) -> Result<(), VfsError> {
        let result = self.sync();
        self.release();
        result
    }

    fn role(&self) -> FileRole {
        self.key.1
    }
}

impl Drop for MemoryFile {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::MemoryVfs;
    use crate::vfs::{FileRole, OpenOptions, Vfs, VfsError, VfsFile};

    fn assert_io(error: &VfsError) {
        assert!(matches!(error, VfsError::Io { .. }), "{error:?}");
    }

    fn create() -> OpenOptions {
        OpenOptions::new().create(true)
    }

    fn read_vec(file: &mut impl VfsFile) -> Vec<u8> {
        let len = usize::try_from(file.len().unwrap()).unwrap();
        let mut buf = vec![0; len];
        if !buf.is_empty() {
            file.read_exact(0, &mut buf).unwrap();
        }
        buf
    }

    #[test]
    fn missing_file_is_not_found() {
        let vfs = MemoryVfs::new();
        let error = vfs
            .open(Path::new("db"), FileRole::Main, &OpenOptions::new())
            .unwrap_err();
        assert!(matches!(error, VfsError::NotFound), "{error:?}");
    }

    #[test]
    fn empty_path_and_exclusive_without_create_are_invalid() {
        let vfs = MemoryVfs::new();
        let error = vfs
            .open(Path::new(""), FileRole::Main, &create())
            .unwrap_err();
        assert!(matches!(error, VfsError::InvalidPath), "{error:?}");
        let error = vfs
            .open(
                Path::new("db"),
                FileRole::Main,
                &OpenOptions::new().exclusive(true),
            )
            .unwrap_err();
        assert!(matches!(error, VfsError::InvalidPath), "{error:?}");
        let error = vfs.sync_dir(Path::new("")).unwrap_err();
        assert!(matches!(error, VfsError::InvalidPath), "{error:?}");
    }

    #[cfg(unix)]
    #[test]
    fn nul_path_is_invalid() {
        use std::os::unix::ffi::OsStrExt;
        let path = Path::new(std::ffi::OsStr::from_bytes(b"db\0main"));
        let vfs = MemoryVfs::new();
        let error = vfs.open(path, FileRole::Main, &create()).unwrap_err();
        assert!(matches!(error, VfsError::InvalidPath), "{error:?}");
    }

    #[test]
    fn exclusive_create_leaves_existing_bytes() {
        let vfs = MemoryVfs::new();
        let mut file = vfs
            .open(Path::new("db"), FileRole::Main, &create())
            .unwrap();
        file.write_all(0, b"hello").unwrap();
        let error = vfs
            .open(
                Path::new("db"),
                FileRole::Main,
                &OpenOptions::new().create(true).exclusive(true),
            )
            .unwrap_err();
        assert!(matches!(error, VfsError::AlreadyExists), "{error:?}");
        assert_eq!(read_vec(&mut file), b"hello");
        let error = vfs.remove(Path::new("db"), FileRole::Main).unwrap_err();
        assert_io(&error);
    }

    #[test]
    fn create_does_not_truncate() {
        let vfs = MemoryVfs::new();
        {
            let mut file = vfs
                .open(Path::new("db"), FileRole::Main, &create())
                .unwrap();
            file.write_all(0, b"hello").unwrap();
        }
        let mut file = vfs
            .open(Path::new("db"), FileRole::Main, &create())
            .unwrap();
        assert_eq!(read_vec(&mut file), b"hello");
    }

    #[test]
    fn path_and_role_are_independent() {
        let vfs = MemoryVfs::new();
        let mut main = vfs
            .open(Path::new("db"), FileRole::Main, &create())
            .unwrap();
        main.write_all(0, b"main").unwrap();
        let mut temp = vfs
            .open(Path::new("db"), FileRole::Temp, &create())
            .unwrap();
        temp.write_all(0, b"temp").unwrap();
        assert_eq!(main.role(), FileRole::Main);
        assert_eq!(temp.role(), FileRole::Temp);
        assert_eq!(read_vec(&mut main), b"main");
        assert_eq!(read_vec(&mut temp), b"temp");
    }

    #[test]
    fn handles_share_visible_bytes() {
        let vfs = MemoryVfs::new();
        let mut first = vfs
            .open(Path::new("db"), FileRole::Main, &create())
            .unwrap();
        let mut second = vfs
            .open(Path::new("db"), FileRole::Main, &OpenOptions::new())
            .unwrap();
        first.write_all(0, b"same").unwrap();
        assert_eq!(read_vec(&mut second), b"same");
    }

    #[test]
    fn read_only_write_is_rejected() {
        let vfs = MemoryVfs::new();
        let mut file = vfs
            .open(
                Path::new("db"),
                FileRole::Main,
                &OpenOptions::new().create(true).read_only(true),
            )
            .unwrap();
        let error = file.write_at(0, b"x").unwrap_err();
        assert_io(&error);
        let error = file.write_at(0, b"").unwrap_err();
        assert_io(&error);
        assert_eq!(file.len().unwrap(), 0);
    }

    #[test]
    fn remove_and_replace_wait_for_the_last_handle() {
        let vfs = MemoryVfs::new();
        let first = vfs
            .open(Path::new("db"), FileRole::Main, &create())
            .unwrap();
        let second = vfs
            .open(Path::new("db"), FileRole::Main, &OpenOptions::new())
            .unwrap();
        assert_io(&vfs.remove(Path::new("db"), FileRole::Main).unwrap_err());
        assert_io(
            &vfs.replace(Path::new("db"), FileRole::Main, b"no")
                .unwrap_err(),
        );
        drop(first);
        assert_io(&vfs.remove(Path::new("db"), FileRole::Main).unwrap_err());
        drop(second);
        vfs.replace(Path::new("db"), FileRole::Main, b"zz").unwrap();
        {
            let mut file = vfs
                .open(Path::new("db"), FileRole::Main, &OpenOptions::new())
                .unwrap();
            assert_eq!(read_vec(&mut file), b"zz");
        }
        vfs.crash();
        let mut file = vfs
            .open(Path::new("db"), FileRole::Main, &OpenOptions::new())
            .unwrap();
        assert_eq!(read_vec(&mut file), b"zz");
        drop(file);
        vfs.remove(Path::new("db"), FileRole::Main).unwrap();
        let error = vfs
            .open(Path::new("db"), FileRole::Main, &OpenOptions::new())
            .unwrap_err();
        assert!(matches!(error, VfsError::NotFound), "{error:?}");
    }

    #[test]
    fn sync_then_crash_keeps_the_synced_prefix() {
        let vfs = MemoryVfs::new();
        let mut file = vfs
            .open(Path::new("db"), FileRole::Main, &create())
            .unwrap();
        file.write_all(0, b"hello").unwrap();
        file.sync().unwrap();
        file.write_all(5, b" world").unwrap();
        assert_eq!(read_vec(&mut file), b"hello world");
        vfs.crash();
        let error = file.read_at(0, &mut [0; 1]).unwrap_err();
        assert_io(&error);
        let error = file.close().unwrap_err();
        assert_io(&error);
        let mut restored = vfs
            .open(Path::new("db"), FileRole::Main, &OpenOptions::new())
            .unwrap();
        assert_eq!(read_vec(&mut restored), b"hello");
        drop(restored);
        vfs.remove(Path::new("db"), FileRole::Main).unwrap();
    }

    #[test]
    fn unsynced_create_disappears_on_crash() {
        let vfs = MemoryVfs::new();
        let mut file = vfs
            .open(Path::new("db"), FileRole::Main, &create())
            .unwrap();
        file.write_all(0, b"hello").unwrap();
        drop(file);
        vfs.crash();
        let error = vfs
            .open(Path::new("db"), FileRole::Main, &OpenOptions::new())
            .unwrap_err();
        assert!(matches!(error, VfsError::NotFound), "{error:?}");
    }

    #[test]
    fn sync_dir_publishes_names_but_not_unsynced_bytes() {
        let vfs = MemoryVfs::new();
        let mut file = vfs
            .open(Path::new("db"), FileRole::Main, &create())
            .unwrap();
        file.write_all(0, b"hello").unwrap();
        file.sync().unwrap();
        file.write_all(5, b"!").unwrap();
        vfs.sync_dir(Path::new(".")).unwrap();
        drop(file);
        vfs.crash();
        let mut restored = vfs
            .open(Path::new("db"), FileRole::Main, &OpenOptions::new())
            .unwrap();
        assert_eq!(read_vec(&mut restored), b"hello");

        let vfs = MemoryVfs::new();
        let mut file = vfs
            .open(Path::new("db"), FileRole::Temp, &create())
            .unwrap();
        file.write_all(0, b"hello").unwrap();
        vfs.sync_dir(Path::new(".")).unwrap();
        drop(file);
        vfs.crash();
        let restored = vfs
            .open(Path::new("db"), FileRole::Temp, &OpenOptions::new())
            .unwrap();
        assert_eq!(restored.len().unwrap(), 0);
    }

    #[test]
    fn close_syncs_and_drop_does_not() {
        let vfs = MemoryVfs::new();
        let mut file = vfs
            .open(Path::new("db"), FileRole::Main, &create())
            .unwrap();
        file.write_all(0, b"hello").unwrap();
        file.close().unwrap();
        vfs.crash();
        let mut restored = vfs
            .open(Path::new("db"), FileRole::Main, &OpenOptions::new())
            .unwrap();
        assert_eq!(read_vec(&mut restored), b"hello");

        let mut file = vfs
            .open(Path::new("tmp"), FileRole::Temp, &create())
            .unwrap();
        file.write_all(0, b"scratch").unwrap();
        drop(file);
        vfs.crash();
        let error = vfs
            .open(Path::new("tmp"), FileRole::Temp, &OpenOptions::new())
            .unwrap_err();
        assert!(matches!(error, VfsError::NotFound), "{error:?}");
    }

    #[test]
    fn read_exact_rejects_every_shorter_prefix() {
        let payload = b"abcdef";
        for len in 0..payload.len() {
            let vfs = MemoryVfs::new();
            let mut file = vfs
                .open(Path::new("db"), FileRole::Main, &create())
                .unwrap();
            file.write_all(0, payload).unwrap();
            drop(file);
            vfs.replace(Path::new("db"), FileRole::Main, &payload[..len])
                .unwrap();
            let mut file = vfs
                .open(Path::new("db"), FileRole::Main, &OpenOptions::new())
                .unwrap();
            let mut buf = [0; 6];
            let error = file.read_exact(0, &mut buf).unwrap_err();
            assert!(matches!(error, VfsError::Truncated), "{error:?}");
        }
    }

    #[test]
    fn gap_write_is_zero_filled() {
        let vfs = MemoryVfs::new();
        let mut file = vfs
            .open(Path::new("db"), FileRole::Main, &create())
            .unwrap();
        file.write_all(2, b"x").unwrap();
        assert_eq!(read_vec(&mut file), b"\0\0x");
        file.write_all(1, b"yz").unwrap();
        assert_eq!(read_vec(&mut file), b"\0yz");
    }
}
