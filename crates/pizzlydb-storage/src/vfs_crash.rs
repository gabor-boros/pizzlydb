//! Crash simulator for power-loss behavior real kernels exhibit.
//!
//! [`MemoryVfs::crash`](crate::MemoryVfs::crash) keeps every synced byte and
//! drops every unsynced byte. [`CrashSimVfs`] records each unsynced write and,
//! on [`CrashSimVfs::crash`], can persist an arbitrary subset of those writes,
//! reorder them, tear a sector, and drop a rename or create that has not
//! reached [`Vfs::sync_dir`](crate::Vfs::sync_dir).
//!
//! A failed [`VfsFile::sync`](crate::VfsFile::sync) either keeps or discards
//! the dirty bytes. Later syncs return success and do not resurrect bytes
//! that were discarded. Callers must treat any sync error as fatal.
//!
//! `set_len`, preallocation, data-only sync, and file locks are not on
//! [`Vfs`](crate::Vfs) yet. When [011](https://github.com/gabor-boros/pizzlydb/blob/main/docs/roadmap/011-vfs-capabilities.md)
//! adds them, this backend models each one as durable only after the matching
//! sync barrier.
//!
//! # Examples
//!
//! ```
//! use std::path::Path;
//!
//! use pizzlydb_storage::{CrashSimConfig, CrashSimVfs, FileRole, OpenOptions, Vfs, VfsFile};
//!
//! let vfs = CrashSimVfs::new(CrashSimConfig::default());
//! let mut file = vfs
//!     .open(
//!         Path::new("db"),
//!         FileRole::Main,
//!         &OpenOptions::new().create(true),
//!     )
//!     .unwrap();
//! file.write_all(0, b"page").unwrap();
//! file.sync().unwrap();
//! drop(file);
//! vfs.sync_dir(Path::new(".")).unwrap();
//! vfs.crash(1);
//!
//! let mut restored = vfs
//!     .open(Path::new("db"), FileRole::Main, &OpenOptions::new())
//!     .unwrap();
//! let mut buf = [0; 4];
//! restored.read_exact(0, &mut buf).unwrap();
//! assert_eq!(&buf, b"page");
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::vfs::{
    FileRole, OpenOptions, Vfs, VfsError, VfsFile, io_error, read_only_error, validate_open,
    validate_path,
};

const DEFAULT_SECTOR: usize = 512;

/// How [`CrashSimVfs::crash`] chooses unsynced bytes.
///
/// # Examples
///
/// ```
/// use pizzlydb_storage::PersistMode;
///
/// assert_ne!(PersistMode::DropUnsynced, PersistMode::Random);
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PersistMode {
    /// Keep synced bytes and drop every unsynced write.
    ///
    /// Pending renames are undone. A create that has not reached `sync_dir`
    /// disappears. This is the all-or-nothing model, with directory entries
    /// durable only after `sync_dir`.
    DropUnsynced,
    /// Persist each unsynced sector independently, in a random order.
    ///
    /// A sector that is persisted can also be torn. Pending renames and
    /// creates are kept or lost independently.
    Random,
}

/// What the next file sync does.
///
/// # Examples
///
/// ```
/// use pizzlydb_storage::FsyncFault;
///
/// let fault = FsyncFault::DiscardOnce;
/// assert!(matches!(fault, FsyncFault::DiscardOnce));
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsyncFault {
    /// The next sync fails and drops the dirty bytes.
    ///
    /// A later sync returns success and does not write those bytes back.
    /// Writes made after the failure can still be synced.
    DiscardOnce,
    /// The next sync fails and leaves the dirty bytes in place.
    ///
    /// The following sync writes them and returns success.
    KeepOnce,
}

/// Knobs for [`CrashSimVfs`].
///
/// # Examples
///
/// ```
/// use pizzlydb_storage::{CrashSimConfig, PersistMode};
///
/// let config = CrashSimConfig::default();
/// assert_eq!(config.sector_size, 512);
/// assert!(matches!(config.persist, PersistMode::Random));
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CrashSimConfig {
    /// Sector size used to split unsynced writes. Zero is treated as 512.
    pub sector_size: usize,
    /// Probability that a persisted sector is shorter than the write.
    ///
    /// Values outside `0.0..=1.0` are clamped. Ignored by
    /// [`PersistMode::DropUnsynced`].
    pub torn_probability: f64,
    /// Which unsynced bytes a crash keeps.
    pub persist: PersistMode,
}

impl Default for CrashSimConfig {
    fn default() -> Self {
        Self {
            sector_size: DEFAULT_SECTOR,
            torn_probability: 0.1,
            persist: PersistMode::Random,
        }
    }
}

#[derive(Clone, Debug)]
struct Write {
    offset: u64,
    bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
struct SimFile {
    visible: Vec<u8>,
    durable: Vec<u8>,
    unsynced: Vec<Write>,
    open_count: usize,
    name_durable: bool,
    truncate_to: Option<usize>,
}

impl SimFile {
    fn empty() -> Self {
        Self {
            visible: Vec::new(),
            durable: Vec::new(),
            unsynced: Vec::new(),
            open_count: 1,
            name_durable: false,
            truncate_to: None,
        }
    }
}

#[derive(Clone, Debug)]
struct PendingRename {
    from: (PathBuf, FileRole),
    to: (PathBuf, FileRole),
    replaced: Option<SimFile>,
    name_was_durable: bool,
}

#[derive(Clone, Debug)]
struct SimState {
    files: HashMap<(PathBuf, FileRole), SimFile>,
    pending: Vec<PendingRename>,
    generation: u64,
    op_count: u64,
    fault: Option<FsyncFault>,
    config: CrashSimConfig,
}

#[derive(Debug)]
struct Inner {
    state: SimState,
    capture: bool,
    images: Vec<SimState>,
}

/// In-memory file store whose crash keeps a seeded subset of unsynced bytes.
///
/// Reads see every completed write. [`VfsFile::sync`](crate::VfsFile::sync)
/// makes those bytes durable. [`Vfs::sync_dir`](crate::Vfs::sync_dir) makes
/// the current names durable. A crash invalidates open handles.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use pizzlydb_storage::{
///     CrashSimConfig, CrashSimVfs, FileRole, OpenOptions, PersistMode, Vfs, VfsFile,
/// };
///
/// let config = CrashSimConfig {
///     persist: PersistMode::DropUnsynced,
///     ..CrashSimConfig::default()
/// };
/// let vfs = CrashSimVfs::new(config);
/// let mut file = vfs
///     .open(
///         Path::new("db"),
///         FileRole::Wal,
///         &OpenOptions::new().create(true),
///     )
///     .unwrap();
/// file.write_all(0, b"wal").unwrap();
/// drop(file);
/// vfs.crash(7);
///
/// let missing = vfs
///     .open(Path::new("db"), FileRole::Wal, &OpenOptions::new())
///     .unwrap_err();
/// assert!(matches!(missing, pizzlydb_storage::VfsError::NotFound));
/// ```
#[derive(Clone, Debug)]
pub struct CrashSimVfs {
    inner: Arc<Mutex<Inner>>,
}

impl CrashSimVfs {
    /// Empty store. Crashes use [`CrashSimConfig::default`].
    ///
    /// # Examples
    ///
    /// ```
    /// use pizzlydb_storage::{CrashSimConfig, CrashSimVfs};
    ///
    /// let vfs = CrashSimVfs::new(CrashSimConfig::default());
    /// assert_eq!(vfs.operation_count(), 0);
    /// ```
    #[must_use]
    pub fn new(config: CrashSimConfig) -> Self {
        Self::from_state(SimState::empty(config), false)
    }

    /// Store that remembers the file state after every mutating operation.
    ///
    /// [`for_each_crash`] replays each remembered state. Capturing is off
    /// unless this constructor, or [`Self::new`] followed by a driver, is used.
    #[must_use]
    pub fn capturing(config: CrashSimConfig) -> Self {
        Self::from_state(SimState::empty(config), true)
    }

    fn from_state(state: SimState, capture: bool) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                state,
                capture,
                images: Vec::new(),
            })),
        }
    }

    fn from_image(config: CrashSimConfig, image: &SimState) -> Self {
        let mut state = image.clone();
        state.config = config;
        state.fault = None;
        Self::from_state(state, false)
    }

    /// Mutating operations since this store was created.
    ///
    /// Reads are not counted. A crash does not add an operation.
    #[must_use]
    pub fn operation_count(&self) -> u64 {
        self.inner.lock().expect("crash vfs").state.op_count
    }

    /// Make the next [`VfsFile::sync`](crate::VfsFile::sync) fail as `fault`.
    ///
    /// A second call replaces a fault that has not fired yet.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::{
    ///     CrashSimConfig, CrashSimVfs, FileRole, FsyncFault, OpenOptions, Vfs, VfsFile,
    /// };
    ///
    /// let vfs = CrashSimVfs::new(CrashSimConfig::default());
    /// let mut file = vfs
    ///     .open(
    ///         Path::new("db"),
    ///         FileRole::Main,
    ///         &OpenOptions::new().create(true),
    ///     )
    ///     .unwrap();
    /// file.write_all(0, b"hello").unwrap();
    /// vfs.sync_dir(Path::new(".")).unwrap();
    /// vfs.arm_fsync_fault(FsyncFault::DiscardOnce);
    /// assert!(file.sync().is_err());
    /// assert!(file.sync().is_ok());
    /// drop(file);
    /// vfs.crash(1);
    ///
    /// let mut restored = vfs
    ///     .open(Path::new("db"), FileRole::Main, &OpenOptions::new())
    ///     .unwrap();
    /// assert!(restored.is_empty().unwrap());
    /// ```
    pub fn arm_fsync_fault(&self, fault: FsyncFault) {
        self.inner.lock().expect("crash vfs").state.fault = Some(fault);
    }

    /// Apply one seeded crash to the current unsynced bytes and pending names.
    ///
    /// The same store state and the same seed produce the same image.
    /// Open handles then fail. `seed` selects the subset; it is not mixed
    /// with anything else.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::Path;
    ///
    /// use pizzlydb_storage::{CrashSimConfig, CrashSimVfs, FileRole, OpenOptions, Vfs, VfsFile};
    ///
    /// let vfs = CrashSimVfs::new(CrashSimConfig::default());
    /// let mut file = vfs
    ///     .open(
    ///         Path::new("db"),
    ///         FileRole::Main,
    ///         &OpenOptions::new().create(true),
    ///     )
    ///     .unwrap();
    /// file.write_all(0, b"ab").unwrap();
    /// file.sync().unwrap();
    /// file.write_all(2, b"cd").unwrap();
    /// drop(file);
    /// vfs.sync_dir(Path::new(".")).unwrap();
    /// vfs.crash(4);
    ///
    /// let mut restored = vfs
    ///     .open(Path::new("db"), FileRole::Main, &OpenOptions::new())
    ///     .unwrap();
    /// let mut buf = [0; 2];
    /// restored.read_exact(0, &mut buf).unwrap();
    /// assert_eq!(&buf, b"ab");
    /// ```
    pub fn crash(&self, seed: u64) {
        let mut inner = self.inner.lock().expect("crash vfs");
        inner.state.crash(seed);
    }

    fn note_mutation(&self) {
        let mut inner = self.inner.lock().expect("crash vfs");
        inner.state.op_count = inner.state.op_count.wrapping_add(1);
        if inner.capture {
            let image = inner.state.clone();
            inner.images.push(image);
        }
    }
}

impl SimState {
    fn empty(config: CrashSimConfig) -> Self {
        Self {
            files: HashMap::new(),
            pending: Vec::new(),
            generation: 0,
            op_count: 0,
            fault: None,
            config,
        }
    }

    fn crash(&mut self, seed: u64) {
        let mut rng = Rng::new(seed);
        self.generation = self.generation.wrapping_add(1);
        let sector = self.config.sector_size.max(1);
        let torn = self.config.torn_probability.clamp(0.0, 1.0);
        let random = matches!(self.config.persist, PersistMode::Random);

        let pending = std::mem::take(&mut self.pending);
        for op in pending.into_iter().rev() {
            let keep = random && rng.bool();
            if keep {
                if let Some(file) = self.files.get_mut(&op.to) {
                    file.name_durable = true;
                }
            } else {
                undo_rename(&mut self.files, op);
            }
        }

        let mut drop_keys = Vec::new();
        let mut names: Vec<(PathBuf, FileRole)> = self
            .files
            .keys()
            .map(|(path, role)| (path.clone(), *role))
            .collect();
        names.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then(role_tag(left.1).cmp(&role_tag(right.1)))
        });
        for key in names {
            let keep_create = random && rng.bool();
            let Some(file) = self.files.get_mut(&(key.0.clone(), key.1)) else {
                continue;
            };
            if !file.name_durable && !keep_create {
                drop_keys.push(key);
                continue;
            }
            if !file.name_durable && keep_create {
                file.name_durable = true;
            }
            file.visible = persisted_image(file, sector, torn, random, &mut rng);
            file.durable.clone_from(&file.visible);
            file.unsynced.clear();
            file.open_count = 0;
        }
        for key in drop_keys {
            self.files.remove(&(key.0, key.1));
        }
    }
}

fn role_tag(role: FileRole) -> u8 {
    match role {
        FileRole::Main => 0,
        FileRole::Wal => 1,
        FileRole::Temp => 2,
    }
}

fn persisted_image(
    file: &SimFile,
    sector: usize,
    torn: f64,
    random: bool,
    rng: &mut Rng,
) -> Vec<u8> {
    if !random {
        return file.durable.clone();
    }

    let mut pieces = Vec::new();
    for write in &file.unsynced {
        let mut offset = 0usize;
        while offset < write.bytes.len() {
            let abs = write.offset.saturating_add(offset as u64);
            let sector_off = (abs as usize) % sector;
            let room = sector - sector_off;
            let len = room.min(write.bytes.len() - offset);
            let bytes = write.bytes[offset..offset + len].to_vec();
            pieces.push(Write { offset: abs, bytes });
            offset += len;
        }
    }

    let mut chosen = Vec::new();
    for mut piece in pieces {
        if !rng.bool() || piece.bytes.is_empty() {
            continue;
        }
        if piece.bytes.len() > 1 && rng.chance(torn) {
            let keep = 1 + rng.index(piece.bytes.len() - 1);
            piece.bytes.truncate(keep);
        }
        chosen.push(piece);
    }
    shuffle(&mut chosen, rng);

    let mut image = file.durable.clone();
    for piece in chosen {
        apply_bytes(&mut image, piece.offset, &piece.bytes);
    }
    if let Some(len) = file.truncate_to
        && rng.bool()
    {
        image.truncate(len);
    }
    image
}

fn apply_bytes(image: &mut Vec<u8>, offset: u64, bytes: &[u8]) {
    let Ok(start) = usize::try_from(offset) else {
        return;
    };
    let Some(end) = start.checked_add(bytes.len()) else {
        return;
    };
    if end > image.len() {
        image.resize(end, 0);
    }
    image[start..end].copy_from_slice(bytes);
}

fn apply_unsynced(file: &mut SimFile) {
    for write in std::mem::take(&mut file.unsynced) {
        apply_bytes(&mut file.durable, write.offset, &write.bytes);
    }
    if let Some(len) = file.truncate_to.take() {
        file.durable.truncate(len);
        file.visible.truncate(len);
    }
}

fn undo_rename(files: &mut HashMap<(PathBuf, FileRole), SimFile>, op: PendingRename) {
    if let Some(mut moved) = files.remove(&op.to) {
        moved.name_durable = op.name_was_durable;
        files.insert(op.from, moved);
    }
    if let Some(old) = op.replaced {
        files.insert(op.to, old);
    }
}

fn file_key(path: &Path, role: FileRole) -> Result<(PathBuf, FileRole), VfsError> {
    validate_path(path)?;
    Ok((path.to_path_buf(), role))
}

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn bool(&mut self) -> bool {
        self.next_u64() & 1 == 1
    }

    fn chance(&mut self, probability: f64) -> bool {
        if probability <= 0.0 {
            return false;
        }
        if probability >= 1.0 {
            return true;
        }
        let threshold = (probability * f64::from(u32::MAX)) as u32;
        (self.next_u64() as u32) < threshold
    }

    fn index(&mut self, n: usize) -> usize {
        debug_assert!(n > 0);
        (self.next_u64() as usize) % n
    }
}

fn shuffle<T>(items: &mut [T], rng: &mut Rng) {
    if items.len() < 2 {
        return;
    }
    for index in (1..items.len()).rev() {
        let swap = rng.index(index + 1);
        items.swap(index, swap);
    }
}

/// Seed for sample `sample` at crash point `op`.
///
/// `for_each_crash` uses this so a failure can be replayed from the two
/// numbers it prints.
#[must_use]
pub fn crash_seed(op: u64, sample: u64) -> u64 {
    op.wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ sample.wrapping_mul(0xBF58_476D_1CE4_E5B9)
        ^ 0xA5A5_A5A5_A5A5_A5A5
}

/// Run `workload`, then crash after every mutating operation.
///
/// `samples` is the number of persistence subsets at each operation. The
/// callback receives the operation index, the seed, and a store that has
/// already been crashed. Replaying `crash_seed(op, sample)` on the captured
/// image produces the same store.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use pizzlydb_storage::{
///     CrashSimConfig, FileRole, OpenOptions, Vfs, VfsFile, for_each_crash,
/// };
///
/// let mut crashes = 0u64;
/// for_each_crash(CrashSimConfig::default(), 1, |vfs| {
///     let mut file = vfs
///         .open(
///             Path::new("db"),
///             FileRole::Main,
///             &OpenOptions::new().create(true),
///         )
///         .unwrap();
///     file.write_all(0, b"a").unwrap();
///     drop(file);
/// }, |op, seed, _vfs| {
///     let _ = (op, seed);
///     crashes += 1;
/// });
/// assert!(crashes >= 1);
/// ```
pub fn for_each_crash<W, C>(config: CrashSimConfig, samples: u64, workload: W, mut check: C)
where
    W: FnOnce(&CrashSimVfs),
    C: FnMut(u64, u64, &CrashSimVfs),
{
    let vfs = CrashSimVfs::capturing(config);
    workload(&vfs);
    let images = vfs.inner.lock().expect("crash vfs").images.clone();
    for image in &images {
        let op = image.op_count;
        for sample in 0..samples {
            let seed = crash_seed(op, sample);
            let trial = CrashSimVfs::from_image(config, image);
            trial.crash(seed);
            check(op, seed, &trial);
        }
    }
}

impl Vfs for CrashSimVfs {
    type File = CrashFile;

    fn open(
        &self,
        path: &Path,
        role: FileRole,
        options: &OpenOptions,
    ) -> Result<Self::File, VfsError> {
        validate_open(path, options)?;
        let key = (path.to_path_buf(), role);
        let created = {
            let mut inner = self.inner.lock().expect("crash vfs");
            match inner.state.files.get_mut(&key) {
                Some(file) => {
                    if options.is_exclusive() {
                        return Err(VfsError::AlreadyExists);
                    }
                    file.open_count = file
                        .open_count
                        .checked_add(1)
                        .ok_or_else(|| io_error("too many open handles"))?;
                    false
                }
                None => {
                    if !options.is_create() {
                        return Err(VfsError::NotFound);
                    }
                    inner.state.files.insert(key.clone(), SimFile::empty());
                    true
                }
            }
        };
        if created {
            self.note_mutation();
        }
        let generation = self.inner.lock().expect("crash vfs").state.generation;
        Ok(CrashFile {
            inner: Arc::clone(&self.inner),
            vfs: self.clone(),
            key,
            generation,
            read_only: options.is_read_only(),
            open: true,
        })
    }

    fn sync_dir(&self, directory: &Path) -> Result<(), VfsError> {
        validate_path(directory)?;
        {
            let mut inner = self.inner.lock().expect("crash vfs");
            for file in inner.state.files.values_mut() {
                file.name_durable = true;
            }
            inner.state.pending.clear();
        }
        self.note_mutation();
        Ok(())
    }

    fn rename(&self, from: &Path, to: &Path, role: FileRole) -> Result<(), VfsError> {
        let from_key = file_key(from, role)?;
        let to_key = file_key(to, role)?;
        if from_key.0 == to_key.0 {
            return Err(VfsError::InvalidPath);
        }

        {
            let mut inner = self.inner.lock().expect("crash vfs");
            let state = &mut inner.state;
            if let Some(op) = state.pending.last()
                && op.from == to_key
                && op.to == from_key
            {
                let op = state
                    .pending
                    .pop()
                    .ok_or_else(|| io_error("missing rename"))?;
                undo_rename(&mut state.files, op);
                drop(inner);
                self.note_mutation();
                return Ok(());
            }

            let mut file = state.files.remove(&from_key).ok_or(VfsError::NotFound)?;
            if file.open_count > 0 {
                state.files.insert(from_key, file);
                return Err(io_error("file is open"));
            }
            let replaced = match state.files.remove(&to_key) {
                Some(dest) if dest.open_count > 0 => {
                    state.files.insert(to_key, dest);
                    state.files.insert(from_key, file);
                    return Err(io_error("file is open"));
                }
                other => other,
            };
            let name_was_durable = file.name_durable;
            file.name_durable = false;
            state.files.insert(to_key.clone(), file);
            state.pending.push(PendingRename {
                from: from_key,
                to: to_key,
                replaced,
                name_was_durable,
            });
        }
        self.note_mutation();
        Ok(())
    }

    fn remove(&self, path: &Path, role: FileRole) -> Result<(), VfsError> {
        let key = file_key(path, role)?;
        {
            let mut inner = self.inner.lock().expect("crash vfs");
            let open_count = inner
                .state
                .files
                .get(&key)
                .ok_or(VfsError::NotFound)?
                .open_count;
            if open_count > 0 {
                return Err(io_error("file is open"));
            }
            inner.state.files.remove(&key);
            inner
                .state
                .pending
                .retain(|op| op.from != key && op.to != key);
        }
        self.note_mutation();
        Ok(())
    }
}

/// Handle for one file in a [`CrashSimVfs`].
#[derive(Debug)]
pub struct CrashFile {
    inner: Arc<Mutex<Inner>>,
    vfs: CrashSimVfs,
    key: (PathBuf, FileRole),
    generation: u64,
    read_only: bool,
    open: bool,
}

impl CrashFile {
    fn with_open_file<T>(
        &self,
        body: impl FnOnce(&mut SimFile) -> Result<T, VfsError>,
    ) -> Result<T, VfsError> {
        if !self.open {
            return Err(io_error("file is closed"));
        }
        let mut inner = self.inner.lock().expect("crash vfs");
        if inner.state.generation != self.generation {
            return Err(io_error("file handle is stale"));
        }
        let file = inner
            .state
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
        let Ok(mut inner) = self.inner.try_lock() else {
            return;
        };
        if inner.state.generation != self.generation {
            return;
        }
        if let Some(file) = inner.state.files.get_mut(&self.key) {
            file.open_count = file.open_count.saturating_sub(1);
        }
    }
}

impl VfsFile for CrashFile {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, VfsError> {
        self.with_open_file(|file| {
            if buf.is_empty() {
                return Ok(0);
            }
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
        let wrote = self.with_open_file(|file| {
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
            file.unsynced.push(Write {
                offset,
                bytes: data.to_vec(),
            });
            Ok(data.len())
        })?;
        if wrote > 0 {
            self.vfs.note_mutation();
        }
        Ok(wrote)
    }

    fn sync(&mut self) -> Result<(), VfsError> {
        let fault = self.inner.lock().expect("crash vfs").state.fault.take();
        let result = self.with_open_file(|file| match fault {
            Some(FsyncFault::DiscardOnce) => {
                file.unsynced.clear();
                Err(io_error("fsync failed"))
            }
            Some(FsyncFault::KeepOnce) => Err(io_error("fsync failed")),
            None => {
                apply_unsynced(file);
                Ok(())
            }
        });
        if result.is_ok() {
            self.vfs.note_mutation();
        }
        result
    }

    fn len(&self) -> Result<u64, VfsError> {
        self.with_open_file(|file| {
            u64::try_from(file.visible.len()).map_err(|_| io_error("length overflow"))
        })
    }

    fn set_len(&mut self, len: u64) -> Result<(), VfsError> {
        if self.read_only {
            return Err(read_only_error());
        }
        self.with_open_file(|file| {
            let new_len = usize::try_from(len).map_err(|_| io_error("length overflow"))?;
            if new_len > file.visible.len() {
                let offset =
                    u64::try_from(file.visible.len()).map_err(|_| io_error("length overflow"))?;
                let zeros = vec![0u8; new_len - file.visible.len()];
                file.visible.resize(new_len, 0);
                file.unsynced.push(Write {
                    offset,
                    bytes: zeros,
                });
            } else {
                file.visible.truncate(new_len);
                file.truncate_to = Some(new_len);
            }
            Ok(())
        })?;
        self.vfs.note_mutation();
        Ok(())
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

impl Drop for CrashFile {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{CrashSimConfig, CrashSimVfs, PersistMode, crash_seed, for_each_crash};
    use crate::vfs::{FileRole, OpenOptions, Vfs, VfsError, VfsFile};

    fn read_file(vfs: &CrashSimVfs, path: &str) -> Result<Vec<u8>, VfsError> {
        let mut file = vfs.open(Path::new(path), FileRole::Main, &OpenOptions::new())?;
        let len = usize::try_from(file.len()?).map_err(|_| crate::vfs::io_error("length"))?;
        let mut buf = vec![0u8; len];
        if !buf.is_empty() {
            file.read_exact(0, &mut buf)?;
        }
        Ok(buf)
    }

    fn config(persist: PersistMode) -> CrashSimConfig {
        CrashSimConfig {
            persist,
            torn_probability: 0.0,
            ..CrashSimConfig::default()
        }
    }

    #[test]
    fn synced_rename_survives_every_final_crash() {
        let config = config(PersistMode::Random);
        let vfs = CrashSimVfs::capturing(config);
        {
            let mut file = vfs
                .open(
                    Path::new("from"),
                    FileRole::Main,
                    &OpenOptions::new().create(true),
                )
                .unwrap();
            file.write_all(0, b"hello").unwrap();
            file.sync().unwrap();
            drop(file);
            vfs.sync_dir(Path::new(".")).unwrap();
            vfs.rename(Path::new("from"), Path::new("to"), FileRole::Main)
                .unwrap();
            vfs.sync_dir(Path::new(".")).unwrap();
        }
        let final_op = vfs.operation_count();
        let images = vfs.inner.lock().expect("crash vfs").images.clone();
        let finished = images.iter().filter(|image| image.op_count == final_op);
        let mut checked = 0;
        for image in finished {
            for sample in 0..8 {
                let trial = CrashSimVfs::from_image(config, image);
                trial.crash(crash_seed(image.op_count, sample));
                assert_eq!(read_file(&trial, "to").unwrap(), b"hello");
                checked += 1;
            }
        }
        assert_eq!(checked, 8);
    }

    #[test]
    fn rename_without_sync_dir_can_lose_the_new_name() {
        let mut lost = false;
        for_each_crash(
            config(PersistMode::Random),
            8,
            |vfs| {
                let mut file = vfs
                    .open(
                        Path::new("from"),
                        FileRole::Main,
                        &OpenOptions::new().create(true),
                    )
                    .unwrap();
                file.write_all(0, b"hello").unwrap();
                file.sync().unwrap();
                drop(file);
                vfs.sync_dir(Path::new(".")).unwrap();
                vfs.rename(Path::new("from"), Path::new("to"), FileRole::Main)
                    .unwrap();
            },
            |_op, _seed, vfs| {
                if read_file(vfs, "to").ok().as_deref() != Some(b"hello".as_slice()) {
                    lost = true;
                }
            },
        );
        assert!(lost, "an unsynced rename was always durable");
    }

    #[test]
    fn the_same_seed_replays_the_same_image() {
        let config = config(PersistMode::Random);
        let vfs = CrashSimVfs::capturing(config);
        let mut file = vfs
            .open(
                Path::new("db"),
                FileRole::Main,
                &OpenOptions::new().create(true),
            )
            .unwrap();
        file.write_all(0, b"abcdef").unwrap();
        drop(file);
        vfs.sync_dir(Path::new(".")).unwrap();
        let image = vfs
            .inner
            .lock()
            .expect("crash vfs")
            .images
            .last()
            .cloned()
            .unwrap();
        let seed = crash_seed(image.op_count, 3);
        let left = CrashSimVfs::from_image(config, &image);
        let right = CrashSimVfs::from_image(config, &image);
        left.crash(seed);
        right.crash(seed);
        assert_eq!(read_file(&left, "db").ok(), read_file(&right, "db").ok());
    }

    #[test]
    fn drop_unsynced_keeps_only_the_synced_prefix() {
        let vfs = CrashSimVfs::new(config(PersistMode::DropUnsynced));
        let mut file = vfs
            .open(
                Path::new("db"),
                FileRole::Main,
                &OpenOptions::new().create(true),
            )
            .unwrap();
        file.write_all(0, b"hello").unwrap();
        file.sync().unwrap();
        file.write_all(5, b" world").unwrap();
        drop(file);
        vfs.sync_dir(Path::new(".")).unwrap();
        vfs.crash(1);
        assert_eq!(read_file(&vfs, "db").unwrap(), b"hello");
    }
}
