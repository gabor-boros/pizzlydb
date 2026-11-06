//! In-house page engine.
//!
//! This crate is a VFS, pager, WAL, page allocator, B-tree, checksums, and
//! superblock. It exposes byte-oriented persistence and does not define graph
//! records, MVCC, or the logical commit log.

mod backup;
mod btree;
// The engine does not call the pager yet, so a non-test build does not see
// these uses.
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
// The storage engine does not call the pager yet.
#[cfg_attr(not(test), allow(dead_code))]
mod pager;
mod salvage;
mod space;
// The pager does not call the superblock.
#[cfg_attr(not(test), allow(dead_code))]
mod superblock;
mod verify;
// The engine does not call the pager yet, so a non-test build does not see
// these uses.
#[cfg_attr(not(test), allow(dead_code))]
mod vfs;
#[cfg_attr(not(test), allow(dead_code))]
mod vfs_memory;
#[cfg(unix)]
#[cfg_attr(not(test), allow(dead_code))]
mod vfs_posix;
// The engine does not call the pager yet, so a non-test build does not see
// these uses.
#[cfg_attr(not(test), allow(dead_code))]
mod wal;
