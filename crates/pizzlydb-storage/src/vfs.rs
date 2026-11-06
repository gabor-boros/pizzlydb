//! Positional I/O for the page engine.
//!
//! [`Vfs`] opens a path and syncs a directory. [`VfsFile`] reads and writes at a
//! byte offset. [`VfsFile::read_at`] and [`VfsFile::write_at`] may transfer fewer
//! bytes than requested. [`VfsFile::read_exact`] and [`VfsFile::write_all`] repeat
//! until the buffer is full.
//!
//! [`VfsFile::sync`] makes the bytes of that file durable. Data written without
//! `sync` may be absent after a crash. [`Vfs::sync_dir`] makes directory entries
//! in that directory durable. File data and directory entries are separate barriers.
//!
//! [`VfsFile::close`] syncs and then releases the descriptor. [`Drop`] releases the
//! descriptor and cannot report an error. Call `sync` or `close` when that result
//! is required.
//!
//! [`FaultInjecting`] applies a script of short I/O, injected errors, and torn
//! writes to any backend. The same script and the same calls fail at the same
//! numbered operation.

use std::cell::RefCell;
use std::io;
use std::path::Path;
use std::rc::Rc;

use thiserror::Error;

pub use crate::vfs_memory::MemoryVfs;
#[cfg(unix)]
pub use crate::vfs_posix::PosixVfs;

/// Failure from a [`Vfs`] operation.
///
/// Platform error codes stay on [`VfsError::Io::source`] for logs and are not
/// a separate variant.
#[derive(Debug, Error)]
pub enum VfsError {
    /// The path is not an existing file.
    #[error("file not found")]
    NotFound,
    /// Exclusive create found an existing path.
    #[error("file already exists")]
    AlreadyExists,
    /// The path or open options cannot be used.
    #[error("invalid path")]
    InvalidPath,
    /// `read_exact` reached the end of the file before the buffer was full.
    #[error("truncated input")]
    Truncated,
    /// The transfer failed. `source` is retained for logs.
    #[error("I/O failure")]
    Io {
        /// Operating-system or backend error. Engine code matches [`VfsError::Io`]
        /// and does not branch on the platform code inside `source`.
        #[source]
        source: io::Error,
    },
}

/// Build an [`VfsError::Io`] that has no platform code.
pub(crate) fn io_error(message: &'static str) -> VfsError {
    VfsError::Io {
        source: io::Error::other(message),
    }
}

/// Write rejected because the handle was opened read-only.
pub(crate) fn read_only_error() -> VfsError {
    VfsError::Io {
        source: io::Error::new(io::ErrorKind::PermissionDenied, "read-only file"),
    }
}

fn injected_fault() -> VfsError {
    io_error("injected fault")
}

/// Reject an empty path or a path that contains a NUL byte.
pub(crate) fn validate_path(path: &Path) -> Result<(), VfsError> {
    let bytes = path.as_os_str().as_encoded_bytes();
    if bytes.is_empty() || bytes.contains(&0) {
        return Err(VfsError::InvalidPath);
    }
    Ok(())
}

/// Reject a path or option combination that no backend can open.
pub(crate) fn validate_open(path: &Path, options: &OpenOptions) -> Result<(), VfsError> {
    validate_path(path)?;
    if options.is_exclusive() && !options.is_create() {
        return Err(VfsError::InvalidPath);
    }
    Ok(())
}

fn checked_offset(offset: u64, advanced: usize) -> Result<u64, VfsError> {
    let advanced = u64::try_from(advanced).map_err(|_| io_error("offset overflow"))?;
    offset
        .checked_add(advanced)
        .ok_or_else(|| io_error("offset overflow"))
}

/// Kind of file the engine is opening.
///
/// - [`FileRole::Main`] is the page file.
/// - [`FileRole::Wal`] is the log beside it.
/// - [`FileRole::Temp`] is scratch space.
///
/// The memory backend keys files by path and role, so one path can hold both a
/// main file and a temp file. The [POSIX backend](`crate::vfs_posix::PosixVfs`)
/// stores the role on the handle and uses the path as the filesystem path.
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
///     Path::new("db"),
///     FileRole::Main,
///     &OpenOptions::new().create(true),
/// ).unwrap();
///
/// assert_eq!(file.role(), FileRole::Main);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileRole {
    /// The database page file.
    Main,
    /// The write-ahead log beside the page file.
    Wal,
    /// Scratch space. Not the main database file.
    Temp,
}

/// Flags for [`Vfs::open`].
///
/// - `create` makes a missing file and does not truncate an existing file.
/// - `exclusive` requires `create` and fails with [`VfsError::AlreadyExists`]
///   when the path is already present. A failed exclusive create does not change the
///   bytes already stored at that path. `read_only` rejects [`VfsFile::write_at`].
///
/// # Examples
///
/// ```
/// use pizzlydb_storage::vfs::OpenOptions;
///
/// let options = OpenOptions::new().create(true).exclusive(true);
/// assert!(options.is_create());
/// assert!(options.is_exclusive());
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OpenOptions {
    create: bool,
    exclusive: bool,
    read_only: bool,
}

impl OpenOptions {
    /// Open an existing file for reading and writing.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            create: false,
            exclusive: false,
            read_only: false,
        }
    }

    /// Create the file when the path is absent.
    ///
    /// An existing file is opened without truncation.
    #[must_use]
    pub const fn create(mut self, create: bool) -> Self {
        self.create = create;
        self
    }

    /// Fail when `create` would open a path that already exists.
    ///
    /// `exclusive(true)` without [`Self::create`]`(true)` is [`VfsError::InvalidPath`].
    #[must_use]
    pub const fn exclusive(mut self, exclusive: bool) -> Self {
        self.exclusive = exclusive;
        self
    }

    /// Reject later calls to [`VfsFile::write_at`] on this handle.
    ///
    /// When `create` is also set, the backend may open a read-write descriptor
    /// so the file can be created. Writes through this handle still fail.
    #[must_use]
    pub const fn read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// Whether a missing path is created.
    #[must_use]
    pub const fn is_create(&self) -> bool {
        self.create
    }

    /// Whether an existing path fails the open.
    #[must_use]
    pub const fn is_exclusive(&self) -> bool {
        self.exclusive
    }

    /// Whether [`VfsFile::write_at`] is rejected.
    #[must_use]
    pub const fn is_read_only(&self) -> bool {
        self.read_only
    }
}

/// Positional file store.
///
/// The page engine is generic over `V: Vfs`. `open` borrows the backend so the
/// returned handle can outlive that borrow.
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
/// )
/// .unwrap();
///
/// file.write_all(0, b"abcdef").unwrap();
///
/// let mut buf = [0; 6];
/// file.read_exact(0, &mut buf).unwrap();
/// assert_eq!(&buf, b"abcdef");
///
/// file.sync().unwrap();
/// ```
pub trait Vfs {
    /// Open file owned by this backend.
    type File: VfsFile;

    /// Open `path` for `role`.
    ///
    /// # Errors
    ///
    /// Returns [`VfsError::NotFound`] when the path is absent and `create` is not
    /// set, [`VfsError::AlreadyExists`] for a failed exclusive create,
    /// [`VfsError::InvalidPath`] for an empty path, a NUL byte, or `exclusive`
    /// without `create`, and [`VfsError::Io`] for any other failure.
    fn open(
        &self,
        path: &Path,
        role: FileRole,
        options: &OpenOptions,
    ) -> Result<Self::File, VfsError>;

    /// Make directory entries in `directory` durable.
    ///
    /// This is the barrier for a rename in that directory. It does not sync file
    /// bytes. Call [`VfsFile::sync`] on each file whose data must survive a crash.
    ///
    /// # Errors
    ///
    /// Returns [`VfsError::InvalidPath`] for an empty path or a NUL byte, and
    /// [`VfsError::Io`] when the directory cannot be synced. The memory backend
    /// validates `directory` and then publishes every current file name; it has
    /// one directory.
    fn sync_dir(&self, directory: &Path) -> Result<(), VfsError>;
}

/// One open file.
///
/// `read_at` and `write_at` report the number of bytes transferred. A short
/// count is unfinished work, not a failed call. `read_exact` and `write_all`
/// are the operations that require a full buffer.
///
/// On error, a prefix of the buffer may have been read or a prefix of the file
/// may have been written.
pub trait VfsFile {
    /// Copy bytes at `offset` into `buf`.
    ///
    /// The returned count may be smaller than `buf.len()`, including zero at the
    /// end of the file. An empty `buf` succeeds with a count of zero and does
    /// not read.
    ///
    /// # Errors
    ///
    /// Returns [`VfsError::Io`] when the handle is closed, stale, or the backend
    /// cannot read.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, VfsError>;

    /// Copy `data` into the file at `offset`.
    ///
    /// The returned count may be smaller than `data.len()`. A gap before
    /// `offset` is filled with zeros when the backend extends the file. An empty
    /// `data` succeeds with a count of zero and does not extend the file.
    ///
    /// # Errors
    ///
    /// Returns [`VfsError::Io`] when the handle is read-only, closed, stale, or
    /// the backend cannot write.
    fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<usize, VfsError>;

    /// Make the file's current bytes durable.
    ///
    /// Success means a later crash still observes those bytes. The memory
    /// backend copies the visible bytes into the durable buffer. The POSIX
    /// backend calls `sync_all`.
    ///
    /// # Errors
    ///
    /// Returns [`VfsError::Io`] when the barrier fails. The descriptor stays open.
    fn sync(&mut self) -> Result<(), VfsError>;

    /// Logical length in bytes, including bytes not yet covered by [`Self::sync`].
    ///
    /// # Errors
    ///
    /// Returns [`VfsError::Io`] when the length cannot be read.
    fn len(&self) -> Result<u64, VfsError>;

    /// Whether [`Self::len`] is zero.
    ///
    /// # Errors
    ///
    /// Returns [`VfsError::Io`] when the length cannot be read.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::vfs::{FileRole, MemoryVfs, OpenOptions, Vfs, VfsFile};
    ///
    /// let vfs = MemoryVfs::new();
    /// let file = vfs
    ///     .open(
    ///         Path::new("empty"),
    ///         FileRole::Main,
    ///         &OpenOptions::new().create(true),
    ///     )
    ///     .unwrap();
    ///
    /// assert!(file.is_empty().unwrap());
    /// ```
    fn is_empty(&self) -> Result<bool, VfsError> {
        Ok(self.len()? == 0)
    }

    /// Sync, then release the descriptor.
    ///
    /// The descriptor is released even when [`Self::sync`] fails. The returned
    /// error is that sync error. [`Drop`] does not sync and cannot report an
    /// error. Releasing a handle that is already closed succeeds.
    ///
    /// # Errors
    ///
    /// Returns the error from [`Self::sync`], if any.
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
    /// )
    /// .unwrap();
    ///
    /// file.write_all(0, b"hello").unwrap();
    /// file.close().unwrap();
    /// ```
    fn close(self) -> Result<(), VfsError>
    where
        Self: Sized;

    /// Role passed to [`Vfs::open`].
    fn role(&self) -> FileRole;

    /// Fill `buf`, or return [`VfsError::Truncated`] when a read transfers no bytes.
    ///
    /// `Ok(())` means every byte of `buf` was read. A short [`Self::read_at`] is
    /// not success. An empty `buf` succeeds without calling `read_at`. On error,
    /// `buf` may hold a prefix of the requested bytes.
    ///
    /// # Errors
    ///
    /// Returns [`VfsError::Truncated`] at end of file, [`VfsError::Io`] when a
    /// read fails or the offset would overflow, and the same [`VfsError::Io`]
    /// when `read_at` reports more bytes than `buf` can hold.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::vfs::{
    ///     Fault, FaultInjecting, FaultKind, FileRole, MemoryVfs, OpenOptions, Vfs, VfsFile,
    /// };
    ///
    /// let script = [Fault {
    ///     at: 3,
    ///     kind: FaultKind::Short { max_len: 2 },
    /// }];
    /// let vfs = FaultInjecting::new(MemoryVfs::new(), script);
    /// let mut file = vfs.open(
    ///     Path::new("db"),
    ///     FileRole::Main,
    ///     &OpenOptions::new().create(true),
    /// )
    /// .unwrap();
    ///
    /// file.write_all(0, b"abcdef").unwrap();
    ///
    /// let mut buf = [0; 6];
    /// file.read_exact(0, &mut buf).unwrap();
    ///
    /// assert_eq!(&buf, b"abcdef");
    /// ```
    fn read_exact(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), VfsError> {
        let mut filled = 0;
        while filled < buf.len() {
            let at = checked_offset(offset, filled)?;
            let n = self.read_at(at, &mut buf[filled..])?;
            if n == 0 {
                return Err(VfsError::Truncated);
            }

            let next = filled
                .checked_add(n)
                .ok_or_else(|| io_error("offset overflow"))?;

            if next > buf.len() {
                return Err(io_error("read returned more bytes than the buffer"));
            }

            filled = next;
        }

        Ok(())
    }

    /// Write every byte of `data`.
    ///
    /// `Ok(())` means every byte was accepted by [`Self::write_at`]. A short
    /// write is retried at the following offset. A transfer of zero bytes before
    /// `data` is exhausted is [`VfsError::Io`], not success. An empty `data`
    /// succeeds without calling `write_at`. On error, a prefix of `data` may
    /// already be stored.
    ///
    /// # Errors
    ///
    /// Returns [`VfsError::Io`] when a write fails, transfers no bytes, or the
    /// offset would overflow.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::vfs::{
    ///     Fault, FaultInjecting, FaultKind, FileRole, MemoryVfs, OpenOptions, Vfs, VfsFile,
    /// };
    ///
    /// let script = [Fault {
    ///     at: 2,
    ///     kind: FaultKind::Short { max_len: 2 },
    /// }];
    ///
    /// let vfs = FaultInjecting::new(MemoryVfs::new(), script);
    /// let mut file = vfs.open(
    ///     Path::new("db"),
    ///     FileRole::Main,
    ///     &OpenOptions::new().create(true),
    /// )
    /// .unwrap();
    ///
    /// file.write_all(0, b"abcdef").unwrap();
    ///
    /// let mut buf = [0; 6];
    /// file.read_exact(0, &mut buf).unwrap();
    ///
    /// assert_eq!(&buf, b"abcdef");
    /// ```
    fn write_all(&mut self, offset: u64, data: &[u8]) -> Result<(), VfsError> {
        let mut written = 0;
        while written < data.len() {
            let at = checked_offset(offset, written)?;
            let n = self.write_at(at, &data[written..])?;
            if n == 0 {
                return Err(io_error("write returned no bytes"));
            }

            let next = written
                .checked_add(n)
                .ok_or_else(|| io_error("offset overflow"))?;

            if next > data.len() {
                return Err(io_error("write returned more bytes than the buffer"));
            }

            written = next;
        }
        Ok(())
    }
}

/// One scripted failure, addressed by a 1-based operation number.
///
/// `at` values below 1 never match. When several entries share `at`, the first
/// one is used.
///
/// # Examples
///
/// ```
/// use pizzlydb_storage::vfs::{Fault, FaultKind};
///
/// let fault = Fault {
///     at: 2,
///     kind: FaultKind::Error,
/// };
/// assert_eq!(fault.at, 2);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fault {
    /// 1-based index among counted calls.
    pub at: u64,
    /// Failure applied when the counter reaches [`Self::at`].
    pub kind: FaultKind,
}

/// What a matching [`Fault`] does to that call.
///
/// # Examples
///
/// ```
/// use pizzlydb_storage::vfs::FaultKind;
///
/// let torn = FaultKind::Torn { prefix: 1 };
/// assert_eq!(torn, FaultKind::Torn { prefix: 1 });
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultKind {
    /// Fail the call and leave the file unchanged.
    Error,
    /// Transfer at most `max_len` bytes and return that count.
    Short {
        /// Maximum number of bytes this call may transfer.
        max_len: usize,
    },
    /// Persist at most `prefix` bytes of a write, then fail the call.
    ///
    /// [`VfsFile::write_all`] stops on that error, so it does not write the rest
    /// of the buffer.
    Torn {
        /// Number of leading bytes stored before the call fails.
        prefix: usize,
    },
}

#[derive(Debug)]
struct FaultState {
    seen: u64,
    faults: Vec<Fault>,
}

fn take_fault(state: &Rc<RefCell<FaultState>>) -> Result<Option<FaultKind>, VfsError> {
    let mut state = state.borrow_mut();
    state.seen = state
        .seen
        .checked_add(1)
        .ok_or_else(|| io_error("operation counter overflow"))?;

    let op = state.seen;
    Ok(state
        .faults
        .iter()
        .find(|fault| fault.at == op)
        .map(|fault| fault.kind))
}

/// Backend wrapper that fails calls selected by a script.
///
/// Counted calls are 1-based and shared by the wrapper and the handles it
/// returns: `open`, `read_at`, `write_at`, `sync`, `len`, `sync_dir`, and
/// `close`. `read_exact` and `write_all` are not separate operations. [`Drop`]
/// is not counted. [`VfsFile::role`] is not counted.
///
/// [`FaultKind::Short`] limits `read_at` and `write_at`. [`FaultKind::Torn`]
/// persists a prefix through `write_at` and then returns [`VfsError::Io`].
/// [`FaultKind::Error`] returns [`VfsError::Io`] and does not call the inner
/// backend. A `Short` or `Torn` fault on a call that has no byte buffer also
/// returns [`VfsError::Io`] without calling the inner backend.
///
/// A fault on `close` skips the inner close, so that close does not sync. The
/// handle is still released when the wrapper value is dropped.
///
/// The wrapper is single-threaded.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use pizzlydb_storage::vfs::{
///     Fault, FaultInjecting, FaultKind, FileRole, MemoryVfs, OpenOptions, Vfs, VfsFile,
/// };
///
/// let vfs = FaultInjecting::new(
///     MemoryVfs::new(),
///     [Fault {
///         at: 2,
///         kind: FaultKind::Error,
///     }],
/// );
///
/// let mut file = vfs.open(
///     Path::new("db"),
///     FileRole::Main,
///     &OpenOptions::new().create(true),
/// )
/// .unwrap();
///
/// let error = file.write_all(0, b"page").unwrap_err();
/// assert!(matches!(error, pizzlydb_storage::vfs::VfsError::Io { .. }));
/// ```
#[derive(Debug)]
pub struct FaultInjecting<V> {
    inner: V,
    state: Rc<RefCell<FaultState>>,
}

impl<V> FaultInjecting<V> {
    /// Wrap `inner` with `faults`.
    ///
    /// The counter starts at zero. The first counted call is operation 1.
    pub fn new(inner: V, faults: impl IntoIterator<Item = Fault>) -> Self {
        Self {
            inner,
            state: Rc::new(RefCell::new(FaultState {
                seen: 0,
                faults: faults.into_iter().collect(),
            })),
        }
    }

    /// Borrow the wrapped backend.
    pub fn inner(&self) -> &V {
        &self.inner
    }

    /// Mutably borrow the wrapped backend.
    ///
    /// Calls made on the inner value directly are not counted.
    pub fn inner_mut(&mut self) -> &mut V {
        &mut self.inner
    }
}

impl<V: Vfs> Vfs for FaultInjecting<V> {
    type File = FaultFile<V::File>;

    fn open(
        &self,
        path: &Path,
        role: FileRole,
        options: &OpenOptions,
    ) -> Result<Self::File, VfsError> {
        if take_fault(&self.state)?.is_some() {
            return Err(injected_fault());
        }

        Ok(FaultFile {
            inner: Some(self.inner.open(path, role, options)?),
            state: Rc::clone(&self.state),
            role,
        })
    }

    fn sync_dir(&self, directory: &Path) -> Result<(), VfsError> {
        if take_fault(&self.state)?.is_some() {
            return Err(injected_fault());
        }

        self.inner.sync_dir(directory)
    }
}

/// Handle returned by [`FaultInjecting`].
#[derive(Debug)]
pub struct FaultFile<F> {
    inner: Option<F>,
    state: Rc<RefCell<FaultState>>,
    role: FileRole,
}

impl<F> FaultFile<F> {
    fn inner_mut(&mut self) -> Result<&mut F, VfsError> {
        self.inner
            .as_mut()
            .ok_or_else(|| io_error("file is closed"))
    }
}

impl<F: VfsFile> VfsFile for FaultFile<F> {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, VfsError> {
        match take_fault(&self.state)? {
            Some(FaultKind::Short { max_len }) => {
                let n = max_len.min(buf.len());
                self.inner_mut()?.read_at(offset, &mut buf[..n])
            }
            Some(FaultKind::Error | FaultKind::Torn { .. }) => Err(injected_fault()),
            None => self.inner_mut()?.read_at(offset, buf),
        }
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<usize, VfsError> {
        match take_fault(&self.state)? {
            Some(FaultKind::Error) => Err(injected_fault()),
            Some(FaultKind::Short { max_len }) => {
                let n = max_len.min(data.len());
                self.inner_mut()?.write_at(offset, &data[..n])
            }
            Some(FaultKind::Torn { prefix }) => {
                let n = prefix.min(data.len());
                if n > 0 {
                    self.inner_mut()?.write_at(offset, &data[..n])?;
                }
                Err(injected_fault())
            }
            None => self.inner_mut()?.write_at(offset, data),
        }
    }

    fn sync(&mut self) -> Result<(), VfsError> {
        if take_fault(&self.state)?.is_some() {
            return Err(injected_fault());
        }

        self.inner_mut()?.sync()
    }

    fn len(&self) -> Result<u64, VfsError> {
        if take_fault(&self.state)?.is_some() {
            return Err(injected_fault());
        }

        self.inner
            .as_ref()
            .ok_or_else(|| io_error("file is closed"))?
            .len()
    }

    fn close(mut self) -> Result<(), VfsError> {
        if take_fault(&self.state)?.is_some() {
            return Err(injected_fault());
        }

        match self.inner.take() {
            Some(file) => file.close(),
            None => Ok(()),
        }
    }

    fn role(&self) -> FileRole {
        self.role
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::path::Path;

    use super::{
        Fault, FaultInjecting, FaultKind, FileRole, OpenOptions, Vfs, VfsError, VfsFile,
        injected_fault, io_error,
    };
    use crate::vfs_memory::MemoryVfs;

    fn assert_io(error: &VfsError) {
        assert!(matches!(error, VfsError::Io { .. }), "{error:?}");
    }

    struct ChunkIo {
        chunk: usize,
        bytes: Vec<u8>,
        reads: usize,
        writes: usize,
    }

    impl VfsFile for ChunkIo {
        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, VfsError> {
            self.reads += 1;
            if self.chunk == 0 || buf.is_empty() {
                return Ok(0);
            }
            let Ok(start) = usize::try_from(offset) else {
                return Ok(0);
            };
            if start >= self.bytes.len() {
                return Ok(0);
            }
            let n = self.chunk.min(buf.len()).min(self.bytes.len() - start);
            buf[..n].copy_from_slice(&self.bytes[start..start + n]);
            Ok(n)
        }

        fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<usize, VfsError> {
            self.writes += 1;
            if self.chunk == 0 || data.is_empty() {
                return Ok(0);
            }
            let n = self.chunk.min(data.len());
            let start = usize::try_from(offset).map_err(|_| io_error("offset overflow"))?;
            let end = start
                .checked_add(n)
                .ok_or_else(|| io_error("offset overflow"))?;
            if end > self.bytes.len() {
                self.bytes.resize(end, 0);
            }
            self.bytes[start..end].copy_from_slice(&data[..n]);
            Ok(n)
        }

        fn sync(&mut self) -> Result<(), VfsError> {
            Ok(())
        }

        fn len(&self) -> Result<u64, VfsError> {
            u64::try_from(self.bytes.len()).map_err(|_| io_error("offset overflow"))
        }

        fn close(self) -> Result<(), VfsError> {
            Ok(())
        }

        fn role(&self) -> FileRole {
            FileRole::Main
        }
    }

    struct OneByte;

    impl VfsFile for OneByte {
        fn read_at(&mut self, _offset: u64, buf: &mut [u8]) -> Result<usize, VfsError> {
            if buf.is_empty() {
                return Ok(0);
            }
            buf[0] = b'x';
            Ok(1)
        }

        fn write_at(&mut self, _offset: u64, data: &[u8]) -> Result<usize, VfsError> {
            if data.is_empty() { Ok(0) } else { Ok(1) }
        }

        fn sync(&mut self) -> Result<(), VfsError> {
            Ok(())
        }

        fn len(&self) -> Result<u64, VfsError> {
            Ok(0)
        }

        fn close(self) -> Result<(), VfsError> {
            Ok(())
        }

        fn role(&self) -> FileRole {
            FileRole::Main
        }
    }

    struct LyingRead;

    impl VfsFile for LyingRead {
        fn read_at(&mut self, _offset: u64, buf: &mut [u8]) -> Result<usize, VfsError> {
            Ok(buf.len().saturating_add(1))
        }

        fn write_at(&mut self, _offset: u64, data: &[u8]) -> Result<usize, VfsError> {
            Ok(data.len().saturating_add(1))
        }

        fn sync(&mut self) -> Result<(), VfsError> {
            Ok(())
        }

        fn len(&self) -> Result<u64, VfsError> {
            Ok(0)
        }

        fn close(self) -> Result<(), VfsError> {
            Ok(())
        }

        fn role(&self) -> FileRole {
            FileRole::Main
        }
    }

    #[test]
    fn io_display_hides_errno() {
        let error = VfsError::Io {
            source: io::Error::from_raw_os_error(28),
        };
        assert_eq!(error.to_string(), "I/O failure");
        let source = std::error::Error::source(&error).expect("source");
        let io_error = source.downcast_ref::<io::Error>().expect("io error");
        assert_eq!(io_error.raw_os_error(), Some(28));
        assert_eq!(VfsError::NotFound.to_string(), "file not found");
        assert_eq!(VfsError::AlreadyExists.to_string(), "file already exists");
        assert_eq!(VfsError::InvalidPath.to_string(), "invalid path");
        assert_eq!(VfsError::Truncated.to_string(), "truncated input");
    }

    #[test]
    fn read_exact_assembles_short_reads() {
        let mut file = ChunkIo {
            chunk: 2,
            bytes: b"abcdef".to_vec(),
            reads: 0,
            writes: 0,
        };
        let mut buf = [0; 6];
        file.read_exact(0, &mut buf).unwrap();
        assert_eq!(&buf, b"abcdef");
        assert_eq!(file.reads, 3);
    }

    #[test]
    fn read_exact_truncates_on_zero_read() {
        let mut file = ChunkIo {
            chunk: 0,
            bytes: b"abcdef".to_vec(),
            reads: 0,
            writes: 0,
        };
        let mut buf = [0; 4];
        let error = file.read_exact(0, &mut buf).unwrap_err();
        assert!(matches!(error, VfsError::Truncated), "{error:?}");

        let mut file = ChunkIo {
            chunk: 8,
            bytes: b"ab".to_vec(),
            reads: 0,
            writes: 0,
        };
        let error = file.read_exact(0, &mut buf).unwrap_err();
        assert!(matches!(error, VfsError::Truncated), "{error:?}");
        assert_eq!(&buf[..2], b"ab");
    }

    #[test]
    fn write_all_assembles_short_writes_and_rejects_zero() {
        let mut file = ChunkIo {
            chunk: 2,
            bytes: Vec::new(),
            reads: 0,
            writes: 0,
        };
        file.write_all(0, b"abcdef").unwrap();
        assert_eq!(file.bytes, b"abcdef");
        assert_eq!(file.writes, 3);

        let mut file = ChunkIo {
            chunk: 0,
            bytes: Vec::new(),
            reads: 0,
            writes: 0,
        };
        let error = file.write_all(0, b"abcdef").unwrap_err();
        assert_io(&error);
        assert!(file.bytes.is_empty());
    }

    #[test]
    fn empty_buffers_do_not_call_the_backend() {
        let mut file = ChunkIo {
            chunk: 2,
            bytes: Vec::new(),
            reads: 0,
            writes: 0,
        };
        file.read_exact(0, &mut []).unwrap();
        file.write_all(0, b"").unwrap();
        assert_eq!(file.reads, 0);
        assert_eq!(file.writes, 0);
    }

    #[test]
    fn exact_helpers_reject_offset_overflow() {
        let mut file = OneByte;
        let error = file.read_exact(u64::MAX, &mut [0, 0]).unwrap_err();
        assert_io(&error);
        let error = file.write_all(u64::MAX, &[1, 2]).unwrap_err();
        assert_io(&error);
    }

    #[test]
    fn exact_helpers_reject_a_lying_transfer_count() {
        let mut file = LyingRead;
        let error = file.read_exact(0, &mut [0, 0, 0, 0]).unwrap_err();
        assert_io(&error);
        let error = file.write_all(0, &[1, 2, 3, 4]).unwrap_err();
        assert_io(&error);
    }

    fn durability_script() -> [Fault; 3] {
        [
            Fault {
                at: 2,
                kind: FaultKind::Short { max_len: 2 },
            },
            Fault {
                at: 3,
                kind: FaultKind::Torn { prefix: 1 },
            },
            Fault {
                at: 4,
                kind: FaultKind::Error,
            },
        ]
    }

    fn run_durability_script(vfs: &impl super::Vfs, path: &Path) -> Vec<u8> {
        let mut file = vfs
            .open(path, FileRole::Main, &OpenOptions::new().create(true))
            .unwrap();
        let n = file.write_at(0, b"abcdef").unwrap();
        assert_eq!(n, 2, "operation 2 is the short write");
        let torn = file.write_at(2, b"xyz").unwrap_err();
        assert_io(&torn);
        let sync = file.sync().unwrap_err();
        assert_io(&sync);
        let mut buf = [0; 8];
        let n = file.read_at(0, &mut buf).unwrap();
        buf[..n].to_vec()
    }

    #[test]
    fn fault_script_fails_the_same_numbered_operation() {
        let first = run_durability_script(
            &FaultInjecting::new(MemoryVfs::new(), durability_script()),
            Path::new("db"),
        );
        let second = run_durability_script(
            &FaultInjecting::new(MemoryVfs::new(), durability_script()),
            Path::new("db"),
        );
        assert_eq!(first, b"abx");
        assert_eq!(second, first);

        #[cfg(unix)]
        {
            let dir = tempfile::tempdir().unwrap();
            let posix = |name: &str| {
                let vfs =
                    FaultInjecting::new(crate::vfs_posix::PosixVfs::new(), durability_script());
                run_durability_script(&vfs, &dir.path().join(name))
            };
            assert_eq!(posix("run-1"), first);
            assert_eq!(posix("run-2"), first);
        }
    }

    #[test]
    fn read_exact_retries_a_short_read() {
        let vfs = FaultInjecting::new(
            MemoryVfs::new(),
            [Fault {
                at: 3,
                kind: FaultKind::Short { max_len: 2 },
            }],
        );
        let mut file = vfs
            .open(
                Path::new("db"),
                FileRole::Main,
                &OpenOptions::new().create(true),
            )
            .unwrap();
        file.write_all(0, b"abcdef").unwrap();
        let mut buf = [0; 6];
        file.read_exact(0, &mut buf).unwrap();
        assert_eq!(&buf, b"abcdef");
    }

    #[test]
    fn write_all_completes_after_a_short_write() {
        let vfs = FaultInjecting::new(
            MemoryVfs::new(),
            [Fault {
                at: 2,
                kind: FaultKind::Short { max_len: 2 },
            }],
        );
        let mut file = vfs
            .open(
                Path::new("db"),
                FileRole::Main,
                &OpenOptions::new().create(true),
            )
            .unwrap();
        file.write_all(0, b"abcdef").unwrap();
        let mut buf = [0; 6];
        file.read_exact(0, &mut buf).unwrap();
        assert_eq!(&buf, b"abcdef");
    }

    #[test]
    fn torn_write_is_not_repaired_by_write_all() {
        let vfs = FaultInjecting::new(
            MemoryVfs::new(),
            [Fault {
                at: 2,
                kind: FaultKind::Torn { prefix: 2 },
            }],
        );
        let mut file = vfs
            .open(
                Path::new("db"),
                FileRole::Main,
                &OpenOptions::new().create(true),
            )
            .unwrap();
        let error = file.write_all(0, b"abcdef").unwrap_err();
        assert_io(&error);
        let mut buf = [0; 6];
        let n = file.read_at(0, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"ab");
    }

    #[test]
    fn zero_short_write_fails_write_all() {
        let vfs = FaultInjecting::new(
            MemoryVfs::new(),
            [Fault {
                at: 2,
                kind: FaultKind::Short { max_len: 0 },
            }],
        );
        let mut file = vfs
            .open(
                Path::new("db"),
                FileRole::Main,
                &OpenOptions::new().create(true),
            )
            .unwrap();
        let error = file.write_all(0, b"abcdef").unwrap_err();
        assert_io(&error);
        assert_eq!(file.len().unwrap(), 0);
    }

    #[test]
    fn fault_index_below_one_does_not_fire() {
        let vfs = FaultInjecting::new(
            MemoryVfs::new(),
            [Fault {
                at: 0,
                kind: FaultKind::Error,
            }],
        );
        let mut file = vfs
            .open(
                Path::new("db"),
                FileRole::Main,
                &OpenOptions::new().create(true),
            )
            .unwrap();
        file.write_all(0, b"ok").unwrap();
    }

    #[test]
    fn duplicate_fault_index_uses_the_first_entry() {
        let vfs = FaultInjecting::new(
            MemoryVfs::new(),
            [
                Fault {
                    at: 2,
                    kind: FaultKind::Error,
                },
                Fault {
                    at: 2,
                    kind: FaultKind::Short { max_len: 3 },
                },
            ],
        );
        let mut file = vfs
            .open(
                Path::new("db"),
                FileRole::Main,
                &OpenOptions::new().create(true),
            )
            .unwrap();
        let error = file.write_at(0, b"abcdef").unwrap_err();
        assert_io(&error);
        assert_eq!(file.len().unwrap(), 0);
    }

    #[test]
    fn fault_on_open_does_not_create() {
        let vfs = FaultInjecting::new(
            MemoryVfs::new(),
            [Fault {
                at: 1,
                kind: FaultKind::Short { max_len: 1 },
            }],
        );
        let error = vfs
            .open(
                Path::new("db"),
                FileRole::Main,
                &OpenOptions::new().create(true),
            )
            .unwrap_err();
        assert_io(&error);
        let missing = vfs
            .inner()
            .open(Path::new("db"), FileRole::Main, &OpenOptions::new())
            .unwrap_err();
        assert!(matches!(missing, VfsError::NotFound), "{missing:?}");
    }

    #[test]
    fn fault_on_sync_dir_does_not_publish() {
        let mut vfs = FaultInjecting::new(
            MemoryVfs::new(),
            [Fault {
                at: 3,
                kind: FaultKind::Torn { prefix: 4 },
            }],
        );
        let mut file = vfs
            .open(
                Path::new("db"),
                FileRole::Main,
                &OpenOptions::new().create(true),
            )
            .unwrap();
        file.write_all(0, b"hello").unwrap();
        let error = vfs.sync_dir(Path::new(".")).unwrap_err();
        drop(file);
        assert_io(&error);
        vfs.inner_mut().crash();
        let missing = vfs
            .inner()
            .open(Path::new("db"), FileRole::Main, &OpenOptions::new())
            .unwrap_err();
        assert!(matches!(missing, VfsError::NotFound), "{missing:?}");
    }

    #[test]
    fn faulted_close_releases_without_syncing() {
        let vfs = FaultInjecting::new(
            MemoryVfs::new(),
            [Fault {
                at: 3,
                kind: FaultKind::Error,
            }],
        );
        let mut file = vfs
            .open(
                Path::new("db"),
                FileRole::Main,
                &OpenOptions::new().create(true),
            )
            .unwrap();
        file.write_all(0, b"hello").unwrap();
        let error = file.close().unwrap_err();
        assert_io(&error);
        vfs.inner().remove(Path::new("db"), FileRole::Main).unwrap();

        let vfs = FaultInjecting::new(
            MemoryVfs::new(),
            [Fault {
                at: 3,
                kind: FaultKind::Error,
            }],
        );
        let mut file = vfs
            .open(
                Path::new("db"),
                FileRole::Main,
                &OpenOptions::new().create(true),
            )
            .unwrap();
        file.write_all(0, b"hello").unwrap();
        let error = file.close().unwrap_err();
        assert_io(&error);
        vfs.inner().crash();
        let missing = vfs
            .inner()
            .open(Path::new("db"), FileRole::Main, &OpenOptions::new())
            .unwrap_err();
        assert!(matches!(missing, VfsError::NotFound), "{missing:?}");
    }

    #[test]
    fn injected_fault_is_an_io_error() {
        assert_io(&injected_fault());
    }
}
