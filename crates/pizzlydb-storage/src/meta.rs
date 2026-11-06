//! Bitmap page id and B-tree root page id.
//!
//! Node and edge counters are not stored here. This is not a port of the Zig
//! record-metadata page. The bitmap page id is
//! [`BITMAP_PAGE_ID`](crate::page::BITMAP_PAGE_ID) in the allocator header.
//! The B-tree root page id is stored in the superblock.
