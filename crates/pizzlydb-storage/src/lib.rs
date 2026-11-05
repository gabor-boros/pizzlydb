//! In-house page engine.
//!
//! This crate is a VFS, pager, WAL, page allocator, B-tree, checksums, and
//! superblock. It exposes byte-oriented persistence and does not define graph
//! records, MVCC, or the logical commit log.

mod backup;
mod btree;
// The pager and the write-ahead log are not implemented yet, so these modules
// have no uses outside their tests.
#[cfg_attr(not(test), allow(dead_code))]
mod checksum;
#[cfg_attr(not(test), allow(dead_code))]
mod codec;
mod engine;
mod inspect;
mod maintain;
mod meta;
#[cfg_attr(not(test), allow(dead_code))]
mod page;
mod pager;
mod salvage;
mod space;
mod superblock;
mod verify;
// The page engine is the caller. It is not implemented yet, so these modules
// have no uses outside their tests.
#[cfg_attr(not(test), allow(dead_code))]
mod vfs;
#[cfg_attr(not(test), allow(dead_code))]
mod vfs_memory;
#[cfg(unix)]
#[cfg_attr(not(test), allow(dead_code))]
mod vfs_posix;
mod wal;
