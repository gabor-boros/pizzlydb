# pizzlydb-storage

Byte-oriented storage for pizzlydb.

`Storage` creates and opens a database, reads a snapshot, and commits a batch of puts and deletes. `Durability::Full` syncs the commit record before it returns. Dropping a batch without `commit` publishes nothing. `checkpoint` reclaims pages that no open snapshot can still read, copies the oldest visible sequence into the main file, and advances the recorded checkpoint. A page retired at that horizon stays allocated.

`backup` copies one snapshot to a new database and an empty log. `restore` checks that copy with `verify_quick`, then `publish` renames it into place. `inspect` reads the main file and reports the first damaged page. `export_checkpoint` copies a main file whose log cannot be opened.

Callers pass a `Vfs`. `MemoryVfs` and, on Unix, `PosixVfs` are included. The log path is the database path with `-wal` appended.

One write batch may be open. A second `begin_write` returns `StorageError::Busy`.

The index, the log record layout, and the file format stay inside this crate. Higher-level records, multi-version transactions, and the logical commit log belong to other crates. This crate does not depend on `pizzlydb-core`.

## Public types

`Storage`, `StorageEngine`, `ReadView`, `WriteBatch`, `Scan`, `StorageError`, `Durability`, `Lsn`, `PageId`, `BackupReport`, `InspectReport`, `Damage`, `DamageReason`, and the `Vfs` traits and backends. `backup`, `restore`, `publish`, `export_checkpoint`, `verify_quick`, and `inspect` are the file operations.

`MAX_KEY_LEN` (512 bytes) and `MAX_VALUE_LEN` are the largest key and value a batch accepts. A larger key or value returns `StorageError::Full`. A value too large for one leaf page is stored on overflow pages.

## License

Licensed under the [Apache License 2.0](../../LICENSE).
