# WAL index

Committed page images stay in the WAL file. The in-memory index stores one record per version:

`versions * 32` bytes, plus one `u32` slot per version in that page's ordered vector. Hash-map buckets are extra and grow with the number of distinct page ids, not with the file size.

Lookup of the newest version at or below a snapshot is a binary search of that page's vector. It does not walk other pages' versions.

[`Wal::open`](../../crates/pizzlydb-storage/src/wal.rs) reads one frame at a time. Appending a version past [`Wal::version_limit`](../../crates/pizzlydb-storage/src/wal.rs) returns `WalError::Full`, which storage reports as `StorageError::WalFull`.

The default limit is 1,048,576 versions.

## Generation reset

The header is version 2. Bytes 32..40 are the generation. Frame checksums cover that generation, so a reset can publish a new header without moving the old frames. `Wal::prune` does this only when every indexed frame is at or below the horizon. A newer frame leaves the file untouched. Recovery of the new generation stops at the first old frame because its checksum does not match.

`CheckpointPolicy` checkpoints when the appended log (header plus accepted frames, not the allocated file size) reaches `wal_soft_bytes`. While a snapshot is held and the appended log is at least `wal_hard_bytes`, `begin_write` returns `ResourceExhausted { resource: "wal" }`. `poll_checkpoint` runs one check on the caller thread. `set_checkpoint_policy` also starts a background task. `close` checkpoints when a policy is set and no batch is open.
