//! Ordered byte keys with snapshot reads and atomic batches.
//!
//! [`Storage`] creates and opens a database. [`Storage::snapshot`] reads one
//! committed sequence. [`Storage::begin_write`] stages puts and deletes.
//! [`WriteBatch::commit`] with [`Durability::Full`] returns after the commit
//! record is synced. Dropping a batch without `commit` publishes nothing.
//! [`Storage::checkpoint`] copies committed bytes into the main file.
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
mod inspect;
mod maintain;
mod meta;
mod page;
#[cfg_attr(not(test), allow(dead_code))]
mod pager;
mod salvage;
#[cfg_attr(not(test), allow(dead_code))]
mod space;
#[cfg_attr(not(test), allow(dead_code))]
mod superblock;
mod verify;
pub mod vfs;
pub mod vfs_memory;
#[cfg(unix)]
pub mod vfs_posix;
#[cfg_attr(not(test), allow(dead_code))]
mod wal;

pub use engine::{Durability, ReadView, Scan, Storage, StorageEngine, StorageError, WriteBatch};
pub use page::Lsn;
pub use vfs::{Fault, FaultInjecting, FaultKind, FileRole, OpenOptions, Vfs, VfsError, VfsFile};
pub use vfs_memory::MemoryVfs;
#[cfg(unix)]
pub use vfs_posix::PosixVfs;
