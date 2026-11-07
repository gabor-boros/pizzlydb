# ADR-011: Storage concurrency and lock order

- Status: Accepted
- Date: 2026-11-07
- Owner issue: [016](../roadmap/016-thread-safe-storage-engine.md)
- Guide: [§2.1](../architecture/implementation-guide.md#21-execution-planes)
- Supersedes: none

## Context

`Storage` has to be shared by reader threads, one writer, and a maintenance thread. The workspace forbids unsafe code, so the sharing uses `Arc` and `std::sync::Mutex`. `parking_lot` and `arc-swap` were not added: a poisoned standard mutex matches a failed critical section, and the published sequence is the committed LSN inside the pager mutex.

## Decision

1. `Vfs` is `Send + Sync`. `VfsFile` is `Send`.
2. `MemoryVfs`, `CrashSimVfs`, and `FaultInjecting` share state with `Arc<Mutex<_>>`.
3. `Pager` keeps frame images and pager state in separate mutexes. The state mutex is acquired before a frame mutex. A frame mutex is held only while copying or replacing that frame, which is the per-frame I/O latch. No other lock is taken while a frame mutex is held.
4. `Storage` is `Arc` of the pager plus a mutex around the catalog (`DbState`). Readers call `snapshot` and then lock only for the duration of each page read. One `WriteBatch` may be open. `WriteBatch` is `Send`.
5. The WAL index is an in-memory map of `(LSN, file offset)` and is read and updated under the pager state mutex. Frame bytes are read from the file under that same mutex for the length of one `read_exact`.
6. Reclaim of retired pages runs when the last snapshot guard drops and when `checkpoint` runs. A later maintenance thread can call the same path.
7. `Durability::Full` and `Durability::Grouped` sync before acknowledgement. `Durability::Deferred` and `Durability::None` do not. `Storage::sync` forces the log barrier. A failed sync closes the handle.

## Consequences

- Concurrent readers share the database. File reads inside one pager call are serialized by the state mutex.
- Sharded page-table lookup and lock-free publication stay with [017](../roadmap/017-concurrent-page-table-and-zero-copy-reads.md).
- Group commit of many open batches stays with the single-writer rule until [033](../roadmap/033-optimistic-transaction-manager.md) stages without that lock.
- There is still no `unsafe` in the workspace.

## Alternatives considered

- One mutex around the whole engine. Rejected because a pin would then hold the only lock across a frame copy, so a reader could not drop a snapshot while another thread copied a frame.
- `parking_lot` locks. Rejected to avoid a dependency; poisoning is the signal that a critical section panicked.

## Verification

`Storage<MemoryVfs>` and `Storage<PosixVfs>` are `Send + Sync`. `cargo test -p pizzlydb-storage --test concurrent` runs readers beside a writer. `rg unsafe crates` stays empty of workspace `unsafe` blocks.

## References

- [016](../roadmap/016-thread-safe-storage-engine.md)
- [013](../roadmap/013-disk-resident-wal-index.md)
- [020](../roadmap/020-group-commit-and-durability-modes.md)
