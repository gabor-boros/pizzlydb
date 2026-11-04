# pizzlydb-storage

In-house page engine and the byte-oriented `StorageEngine` boundary.

This crate is a skeleton. The sections below describe the intended contract. The crate compiles and exports no public API.

## Purpose

`pizzlydb-storage` will persist opaque bytes. The page engine includes:

- a virtual file system (VFS), with memory and POSIX backends
- a pager and physical snapshots
- a write-ahead log (WAL) and a durability barrier
- a page allocator, checksums, and a superblock
- a B-tree with opaque byte keys
- backup, restore, verification, inspection, salvage, and page maintenance

Callers will use `StorageEngine` for atomic synced batches, ordered scans, point reads, snapshots, checkpoints, and durability settings. Page types, WAL frames, and B-tree layout stay in this crate.

Graph records, multi-version concurrency control (MVCC), and the logical commit log belong to `pizzlydb-graph` and `pizzlydb-txn`. This crate does not depend on `pizzlydb-core`.

## Modules

`codec`, `page`, `superblock`, `meta`, `vfs`, `vfs_memory`, `vfs_posix`, `pager`, `space`, `wal`, `btree`, `maintain`, `backup`, `verify`, `inspect`, `salvage`, and `engine`.

`space` is the extent bitmap and free-space map. `wal` is the physical log, not the logical commit log.

## License

Licensed under the [Apache License 2.0](../../LICENSE).
