//! Ordered byte keys with snapshot reads and atomic batches.
//!
//! [`Storage`] creates and opens a database. [`Storage::snapshot`] reads one
//! committed sequence. [`Storage::begin_write`] stages puts and deletes.
//! [`WriteBatch::commit`] with [`Durability::Full`] returns after the commit
//! record is synced. Dropping a batch without `commit` publishes nothing.
//! [`Storage::checkpoint`] copies committed bytes into the main file and
//! reclaims pages that no open snapshot can still read.
//! [`backup`] copies one snapshot to a new database. [`restore`] replaces a
//! path from that copy. [`verify_quick`] checks page headers and the tree.
//! [`inspect`] reports the first damaged page in the main file.
//!
//! One write batch may be open. A second [`Storage::begin_write`] returns
//! [`StorageError::Busy`].
//!
//! Callers pass a [`Vfs`]. [`MemoryVfs`] keeps bytes in memory. On Unix,
//! [`PosixVfs`] uses the filesystem. The log path is the database path with
//! `-wal` appended.
//!
//! This crate does not define higher-level records, multi-version transactions,
//! or a logical commit log, and it does not depend on `pizzlydb-core`.

mod backup;
#[cfg_attr(not(test), allow(dead_code))]
mod btree;
#[cfg_attr(not(test), allow(dead_code))]
mod checksum;
mod codec;
mod engine;
#[cfg(feature = "fuzzing")]
pub mod fuzz;
mod inspect;
mod maintain;
mod page;
#[cfg_attr(not(test), allow(dead_code))]
mod pager;
#[cfg_attr(not(test), allow(dead_code))]
mod space;
#[cfg_attr(not(test), allow(dead_code))]
mod superblock;
mod verify;
pub mod vfs;
pub mod vfs_crash;
pub mod vfs_memory;
#[cfg(unix)]
pub mod vfs_posix;
#[cfg_attr(not(test), allow(dead_code))]
mod wal;

pub use backup::{BackupReport, backup, export_checkpoint, publish, restore};
pub use btree::{MAX_KEY_LEN, MAX_VALUE_LEN};
pub use engine::{
    BatchLimits, CheckpointPolicy, Direction, Durability, LimitKind, RangeEstimate, ReadView, Scan,
    Storage, StorageEngine, StorageError, WriteBatch, random_file_id, wal_path,
};
pub use inspect::{Damage, DamageReason, InspectReport, inspect};
pub use page::{Lsn, PageId};
pub use pager::PagerStats;
pub use verify::verify_quick;
pub use vfs::{
    Fault, FaultInjecting, FaultKind, FileRole, LockMode, OpenOptions, Vfs, VfsError, VfsFile,
};
pub use vfs_crash::{
    CrashSimConfig, CrashSimVfs, FsyncFault, PersistMode, crash_seed, for_each_crash,
};
pub use vfs_memory::MemoryVfs;
#[cfg(unix)]
pub use vfs_posix::PosixVfs;
