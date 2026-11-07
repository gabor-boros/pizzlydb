# 016: Thread-safe storage engine: `Send + Sync`, concurrent readers, one writer

| Field | Value |
| --- | --- |
| Phase | 1A Storage engine hardening |
| Priority | Must (critical path) |
| Size | XL |
| Crates | `pizzlydb-storage` (`engine`, `pager`, `wal`, `space`, `vfs*`), `pizzlydb-graph` (API signatures) |
| Type | Architecture |

## Summary

Redesign the engine's shared state so that `Storage` is `Send + Sync`. The target concurrency model:

- Many threads read independent snapshots concurrently.
- One write batch at a time is staged and committed. [033](033-optimistic-transaction-manager.md) changes this so that staging happens without the writer lock and only commit is serialized.
- Background maintenance runs on its own threads: checkpointing ([021](021-automatic-checkpointing-and-wal-bounds.md)) and, later, the scrubber and incremental workers.

The redesign stays in safe Rust, consistent with the workspace `unsafe_code = "forbid"`.

## Motivation

`Storage` wraps `Rc<Inner>` with `RefCell` state, and the pager states "Pager is not Sync. One thread mutates it". The engine therefore cannot be shared between threads. This blocks:

- a multi-threaded server (guide §2.1 execution planes)
- concurrent query execution
- background checkpointing and incremental maintenance workers
- group commit ([020](020-group-commit-and-durability-modes.md))

Every layer above storage would otherwise have to funnel through one thread, which caps throughput at single-core speed and couples query latency to write latency. This is the most consequential architectural gap after durability.

## Current State

- `crates/pizzlydb-storage/src/engine.rs`: `Storage<V> { inner: Rc<Inner<V>> }`, and `Inner` holds `pager: Pager`, `db: RefCell<DbState>`, and `durability` (around lines 194–211). `ReadView` and `WriteBatch` hold `Rc` clones.
- `crates/pizzlydb-storage/src/pager.rs`: `frames: Vec<RefCell<[u8; PAGE_SIZE]>>` and `state: RefCell<State>` (around lines 219–222). The module docs (around lines 27–29) declare it single-threaded.
- `crates/pizzlydb-storage/src/vfs.rs` and `vfs_memory.rs` use `Rc<RefCell<...>>`, and the `Vfs` trait is not `Send + Sync`-bounded.
- `ReadView::drop` calls `reclaim_to_horizon`, which mutates allocator state from the reader's thread.

## Scope

### In scope

- **Concurrency ADR** (in `docs/adr/`): the lock hierarchy and latch ordering, with no lock held across I/O except per-frame I/O latches. The ADR also chooses the primitives, for example `parking_lot` mutexes and RW-locks, `arc-swap` for the published root and state, and atomics for pin counts.
- **Published state:** the committed root, the high-water mark, and the space-state version are published atomically, so a snapshot is an `Arc` of an immutable descriptor plus a registration in a concurrent snapshot registry, and horizon computation is lock-free or done under a short lock.
- **Pager:** a sharded page table (see [017](017-concurrent-page-table-and-zero-copy-reads.md) for the lookup structure). Each frame has its own RW latch, an atomic pin count, and a CLOCK reference bit, and eviction stays safe under concurrency.
- **WAL:** an append path serialized under the commit lock, and an index ([013](013-disk-resident-wal-index.md)) readable concurrently.
- **Reclaim:** moved off reader drop. Readers only unregister, and the writer or a maintenance task reclaims.
- **API:** `Vfs: Send + Sync` and `VfsFile: Send`; `Storage`, `ReadView`, and the snapshot types are `Send + Sync`. `WriteBatch` is `Send`. `MemoryVfs` and the crash simulator become thread-safe.
- **Graph:** the crate adapts to the new bounds, which may mean making `KeyRead` and `GraphBatch` generic over `Send`.

### Out of scope

- Optimistic multi-writer transactions ([033](033-optimistic-transaction-manager.md)).
- Group commit ([020](020-group-commit-and-durability-modes.md)).
- Async I/O; storage stays synchronous with blocking pools at the server ([202](202-server-runtime.md)).

## Dependencies

- [013](013-disk-resident-wal-index.md): the WAL index must be designed for concurrent readers.
- [010](010-storage-error-taxonomy-fidelity.md): error taxonomy.

## Acceptance Criteria

- [ ] `static_assertions::assert_impl_all!(Storage<PosixVfs>: Send, Sync)`, and the same for `ReadView`.
- [ ] Stress test: 16 reader threads that verify snapshot invariants (every key set at commit N is visible in full, or not at all) while one writer commits 100,000 batches and a third thread checkpoints. The test finds no violations, panics, or deadlocks.
- [ ] Model checking with `loom` (or `shuttle`) of the snapshot registry, the pin and evict protocol, and the publish and reclaim protocol, at small bounds.
- [ ] Single-thread throughput does not regress by more than 10% against the [005](005-benchmark-harness-and-baselines.md) baseline, and read throughput scales at least 6x from 1 to 8 threads on an 8-core machine.
- [ ] The ADR is merged and still holds: there is still no `unsafe`.

## Testing and Validation

- `loom` tests under a `cfg(loom)` feature, and a ThreadSanitizer CI job (nightly toolchain, `-Zsanitizer=thread`) running the stress suite.
- All existing storage and graph suites keep passing.

## References

- [Guide §2.1 execution planes](../architecture/implementation-guide.md#21-execution-planes)
- `crates/pizzlydb-storage/src/{engine,pager,wal,vfs,vfs_memory}.rs`
- [`loom`](https://docs.rs/loom)
