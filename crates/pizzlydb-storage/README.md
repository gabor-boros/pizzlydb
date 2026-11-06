# pizzlydb-storage

Byte-oriented storage for pizzlydb.

`Storage` creates and opens a database, reads a snapshot, and commits a batch of puts and deletes. `Durability::Full` syncs the commit record before it returns. Dropping a batch without `commit` publishes nothing. `checkpoint` copies committed bytes into the main file and advances the recorded checkpoint.

Callers pass a `Vfs`. `MemoryVfs` and, on Unix, `PosixVfs` are included. The log path is the database path with `-wal` appended.

One write batch may be open. A second `begin_write` returns `StorageError::Busy`.

The index, the log record layout, and the file format stay inside this crate. Higher-level records, multi-version transactions, and the logical commit log belong to other crates. This crate does not depend on `pizzlydb-core`.

## Public types

`Storage`, `StorageEngine`, `ReadView`, `WriteBatch`, `Scan`, `StorageError`, `Durability`, `Lsn`, and the `Vfs` traits and backends.

## License

Licensed under the [Apache License 2.0](../../LICENSE).
