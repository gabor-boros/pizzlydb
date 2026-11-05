# pizzlydb-storage

In-house page engine and the byte-oriented `StorageEngine` boundary.

The virtual file system, page headers, CRC-32C checksums, write-ahead log, and superblock are implemented and crate-private. The pager, page allocator, B-tree, and `StorageEngine` boundary are not implemented. The crate exports no public API.

## Purpose

`pizzlydb-storage` persists opaque bytes. The page engine includes:

- a virtual file system (VFS), with memory and POSIX backends
- a pager and physical snapshots
- a write-ahead log (WAL) and a durability barrier
- a page allocator, checksums, and a superblock
- a B-tree with opaque byte keys
- backup, restore, verification, inspection, salvage, and page maintenance

Callers will use `StorageEngine` for atomic synced batches, ordered scans, point reads, snapshots, checkpoints, and durability settings. Page types, WAL frames, and B-tree layout stay in this crate.

Graph records, multi-version concurrency control (MVCC), and the logical commit log belong to `pizzlydb-graph` and `pizzlydb-txn`. This crate does not depend on `pizzlydb-core`.

## Modules

`checksum`, `codec`, `page`, `superblock`, `meta`, `vfs`, `vfs_memory`, `vfs_posix`, `pager`, `space`, `wal`, `btree`, `maintain`, `backup`, `verify`, `inspect`, `salvage`, and `engine`.

`checksum` is the CRC-32C used by page images and the write-ahead log. `codec` is the little-endian integer encoding for those records. `page` is the fixed-size image and its header. `space` is the extent bitmap and free-space map. `wal` is the physical page-image log, not the logical commit log. `superblock` stores two checksummed copies of the file identity, format version, generation, checkpoint LSN, logical page count, and the allocator, free-space map, and B-tree root page ids.

## License

Licensed under the [Apache License 2.0](../../LICENSE).
