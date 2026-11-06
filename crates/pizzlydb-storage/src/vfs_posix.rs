//! Blocking filesystem [`Vfs`](crate::vfs::Vfs).
//!
//! Paths are used as given. Reads and writes go through Unix positional I/O.
//! [`VfsFile::sync`](crate::vfs::VfsFile::sync) and [`Vfs::sync_dir`](crate::vfs::Vfs::sync_dir)
//! call `File::sync_all`. Exclusive create uses `create_new`, so a failed create
//! does not truncate an existing file. Interrupted operations are retried.
//!
//! Parent directories are not created. [`FileRole`](crate::vfs::FileRole) is
//! stored on the handle. The same filesystem path is one file for every role.
//!
//! When `create` and `read_only` are both set, the descriptor is opened
//! read-write so the file can be created. [`VfsFile::write_at`](crate::vfs::VfsFile::write_at)
//! still rejects writes on that handle.
//!
//! Dropping a handle closes the descriptor and discards any error from the
//! operating system. [`VfsFile::close`](crate::vfs::VfsFile::close) syncs first
//! and returns that error after the descriptor is closed.

use std::fs::File;
use std::io::{self, ErrorKind};
use std::os::unix::fs::FileExt;
use std::path::Path;

use crate::vfs::{
    FileRole, OpenOptions, Vfs, VfsError, VfsFile, io_error, read_only_error, validate_open,
    validate_path,
};

/// Filesystem backend.
///
/// The value has no root directory. Each call receives its own path.
///
/// # Examples
///
/// ```
/// use pizzlydb_storage::vfs::{FileRole, OpenOptions, PosixVfs, Vfs, VfsFile};
///
/// let dir = tempfile::tempdir().unwrap();
/// let vfs = PosixVfs::new();
/// let path = dir.path().join("db");
///
/// let mut file = vfs.open(
///     &path,
///     FileRole::Main,
///     &OpenOptions::new().create(true),
/// ).unwrap();
///
/// file.write_all(0, b"pizzly").unwrap();
/// file.sync().unwrap();
/// vfs.sync_dir(dir.path()).unwrap();
/// file.close().unwrap();
/// ```
#[derive(Clone, Copy, Debug, Default)]
pub struct PosixVfs;

impl PosixVfs {
    /// Backend that uses the paths passed to each call.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Vfs for PosixVfs {
    type File = PosixFile;

    fn open(
        &self,
        path: &Path,
        role: FileRole,
        options: &OpenOptions,
    ) -> Result<Self::File, VfsError> {
        validate_open(path, options)?;

        let mut opts = std::fs::OpenOptions::new();

        if options.is_read_only() && !options.is_create() {
            opts.read(true);
        } else {
            opts.read(true).write(true);
        }

        if options.is_create() && options.is_exclusive() {
            opts.create_new(true);
        } else if options.is_create() {
            opts.create(true);
        }

        Ok(PosixFile {
            file: Some(opts.open(path).map_err(map_io)?),
            role,
            read_only: options.is_read_only(),
        })
    }

    fn sync_dir(&self, directory: &Path) -> Result<(), VfsError> {
        validate_path(directory)?;

        let file = retry_io(|| File::open(directory)).map_err(map_io)?;
        let metadata = retry_io(|| file.metadata()).map_err(map_io)?;
        if !metadata.is_dir() {
            return Err(io_error("not a directory"));
        }

        retry_io(|| file.sync_all()).map_err(map_io)?;
        Ok(())
    }
}

/// One filesystem descriptor.
///
/// [`Drop`] closes the descriptor and discards an error from that close.
#[derive(Debug)]
pub struct PosixFile {
    file: Option<File>,
    role: FileRole,
    read_only: bool,
}

impl PosixFile {
    fn file(&self) -> Result<&File, VfsError> {
        self.file.as_ref().ok_or_else(|| io_error("file is closed"))
    }
}

impl VfsFile for PosixFile {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, VfsError> {
        let file = self.file()?;
        retry_io(|| file.read_at(buf, offset)).map_err(map_io)
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<usize, VfsError> {
        if self.read_only {
            return Err(read_only_error());
        }

        let file = self.file()?;
        retry_io(|| file.write_at(data, offset)).map_err(map_io)
    }

    fn sync(&mut self) -> Result<(), VfsError> {
        let file = self.file()?;
        retry_io(|| file.sync_all()).map_err(map_io)
    }

    fn len(&self) -> Result<u64, VfsError> {
        let file = self.file()?;
        Ok(retry_io(|| file.metadata()).map_err(map_io)?.len())
    }

    fn close(mut self) -> Result<(), VfsError> {
        let result = self.sync();
        self.file.take();
        result
    }

    fn role(&self) -> FileRole {
        self.role
    }
}

impl Drop for PosixFile {
    fn drop(&mut self) {
        // The descriptor is closed. The operating system error, if any, is discarded.
        self.file.take();
    }
}

fn map_io(error: io::Error) -> VfsError {
    match error.kind() {
        ErrorKind::NotFound => VfsError::NotFound,
        ErrorKind::AlreadyExists => VfsError::AlreadyExists,
        ErrorKind::InvalidInput => VfsError::InvalidPath,
        _ => VfsError::Io { source: error },
    }
}

fn retry_io<T>(mut op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    loop {
        match op() {
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            other => return other,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::PosixVfs;
    use crate::vfs::{FileRole, OpenOptions, Vfs, VfsError, VfsFile};

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn assert_io(error: &VfsError) {
        assert!(matches!(error, VfsError::Io { .. }), "{error:?}");
    }

    #[test]
    fn round_trip_sync_and_reopen() {
        let dir = temp_dir();
        let vfs = PosixVfs::new();
        let path = dir.path().join("db");
        let mut file = vfs
            .open(&path, FileRole::Main, &OpenOptions::new().create(true))
            .unwrap();
        file.write_all(0, b"ab").unwrap();
        file.write_all(4, b"cd").unwrap();
        file.sync().unwrap();
        assert_eq!(file.len().unwrap(), 6);
        let mut buf = [0; 6];
        file.read_exact(0, &mut buf).unwrap();
        assert_eq!(&buf, b"ab\0\0cd");
        assert_eq!(file.role(), FileRole::Main);
        vfs.sync_dir(dir.path()).unwrap();
        file.close().unwrap();

        let mut file = vfs.open(&path, FileRole::Wal, &OpenOptions::new()).unwrap();
        assert_eq!(file.role(), FileRole::Wal);
        let mut buf = [0; 6];
        file.read_exact(0, &mut buf).unwrap();
        assert_eq!(&buf, b"ab\0\0cd");
    }

    #[test]
    fn exclusive_create_does_not_truncate() {
        let dir = temp_dir();
        let vfs = PosixVfs::new();
        let path = dir.path().join("db");
        let mut file = vfs
            .open(&path, FileRole::Main, &OpenOptions::new().create(true))
            .unwrap();
        file.write_all(0, b"hello").unwrap();
        file.sync().unwrap();
        drop(file);

        let error = vfs
            .open(
                &path,
                FileRole::Main,
                &OpenOptions::new().create(true).exclusive(true),
            )
            .unwrap_err();
        assert!(matches!(error, VfsError::AlreadyExists), "{error:?}");

        let mut file = vfs
            .open(&path, FileRole::Main, &OpenOptions::new())
            .unwrap();
        let mut buf = [0; 5];
        file.read_exact(0, &mut buf).unwrap();
        assert_eq!(&buf, b"hello");
    }

    #[test]
    fn missing_path_and_invalid_path() {
        let dir = temp_dir();
        let vfs = PosixVfs::new();
        let error = vfs
            .open(
                &dir.path().join("missing"),
                FileRole::Main,
                &OpenOptions::new(),
            )
            .unwrap_err();
        assert!(matches!(error, VfsError::NotFound), "{error:?}");
        let error = vfs
            .open(
                Path::new(""),
                FileRole::Main,
                &OpenOptions::new().create(true),
            )
            .unwrap_err();
        assert!(matches!(error, VfsError::InvalidPath), "{error:?}");
        let error = vfs
            .open(
                &dir.path().join("db"),
                FileRole::Main,
                &OpenOptions::new().exclusive(true),
            )
            .unwrap_err();
        assert!(matches!(error, VfsError::InvalidPath), "{error:?}");
    }

    #[test]
    fn read_only_write_is_rejected_and_sync_dir_requires_a_directory() {
        let dir = temp_dir();
        let vfs = PosixVfs::new();
        let path = dir.path().join("db");
        let mut file = vfs
            .open(
                &path,
                FileRole::Temp,
                &OpenOptions::new().create(true).read_only(true),
            )
            .unwrap();
        let error = file.write_at(0, b"x").unwrap_err();
        assert_io(&error);
        assert_eq!(file.len().unwrap(), 0);
        file.sync().unwrap();
        drop(file);
        let error = vfs.sync_dir(&path).unwrap_err();
        assert_io(&error);
        vfs.sync_dir(dir.path()).unwrap();
    }

    #[test]
    fn nul_path_is_invalid() {
        use std::os::unix::ffi::OsStrExt;
        let path = Path::new(std::ffi::OsStr::from_bytes(b"db\0main"));
        let error = PosixVfs::new()
            .open(path, FileRole::Main, &OpenOptions::new().create(true))
            .unwrap_err();
        assert!(matches!(error, VfsError::InvalidPath), "{error:?}");
    }
}
