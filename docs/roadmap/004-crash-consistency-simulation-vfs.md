# 004: Crash-consistency simulation VFS with partial writeback, reordering, and fsync failure

| Field | Value |
| --- | --- |
| Phase | 1.0 Foundations |
| Priority | Must (critical path) |
| Size | M |
| Crates | `pizzlydb-storage` (test support), `pizzlydb-testkit` |
| Type | Test infrastructure |

## Summary

Add a simulated file system that models what real kernels and disks do on power loss. The current `MemoryVfs` crash model is all-or-nothing, and the new simulator must cover these behaviors:

- **Partial writeback:** an arbitrary subset of unsynced writes persists.
- **Reordering:** unsynced writes can reach disk in any order.
- **Torn sectors:** a write can persist only partially within a sector.
- **Directory operations:** a rename or create can be lost before `sync_dir`.
- **fsync failure:** a failed `sync` drops the dirty data, as Linux does.

## Motivation

`MemoryVfs::crash` drops every unsynced byte and keeps every synced byte. Under that model, an algorithm that overwrites live data in place and syncs only at the end looks safe, even though it can lose committed data when the kernel writes back part of the dirty pages. `Wal::prune` is exactly that algorithm (see [012](012-crash-safe-wal-reset.md)), and no existing test can find it. Every durability claim in the roadmap needs a crash model at least as hostile as real hardware.

## Current State

- `crates/pizzlydb-storage/src/vfs_memory.rs` (`MemoryVfs::crash`, around lines 152–176) restores each file's `durable` buffer and undoes pending renames.
- `FaultInjecting` in `crates/pizzlydb-storage/src/vfs.rs` can fail the n-th operation and tear a write, but it cannot persist a random subset of earlier unsynced writes.
- `crates/pizzlydb-storage/tests/engine_crash.rs` (8 tests, about 53 seconds in debug) crashes at chosen points under the all-or-nothing model.

## Scope

### In scope

- A `CrashSimVfs` that records each unsynced write per file as an ordered list of `(offset, bytes)` and keeps directory mutations pending until `sync_dir`.
- `crash(seed)`: for every file, choose a random subset of the unsynced writes (each sector independently, with a configurable torn-sector probability), apply them in a random order on top of the durable image, and resolve pending directory operations in the same random way.
- **fsync failure modes:** after a failed `sync`, the dirty data either survives or is discarded, and later syncs succeed while reporting nothing (the Linux "fsyncgate" behavior). The engine must treat any sync failure as fatal ([022](022-disk-full-and-io-error-safety.md)).
- An exhaustive driver: run workload W, crash after each VFS operation k, enumerate N persistence subsets per crash point, reopen, and check the invariants. The driver must be deterministic and reproducible from the seed.
- Models for the capabilities added by [011](011-vfs-capabilities.md) (`set_len`, preallocation, data-only sync, locks) as they land.

### Out of scope

- Real-device testing with dm-flakey or LazyFS, which belongs to [119](119-operational-fault-injection-suite.md).

## Dependencies

- None. This is foundation work, and it must land before [012](012-crash-safe-wal-reset.md) so that the WAL fix is test-driven.

## Acceptance Criteria

- [x] `CrashSimVfs` implements `Vfs`, is documented, and is shared through `pizzlydb-testkit` or a `test-support` feature.
- [x] A regression test reproduces the `Wal::prune` hazard: it finds a crash point and persistence subset after which either a commit acknowledged with `Durability::Full` is missing or `Storage::open` returns `Corrupt`. The test is committed as `#[ignore]` or as an expected failure until [012](012-crash-safe-wal-reset.md) fixes the bug.
- [x] All existing `engine_crash.rs` scenarios pass under the new model or have a documented reason why not.
- [x] Each crash enumeration is reproducible from its seed.

## Testing and Validation

- Meta-tests: a toy "write then rename without sync" protocol must be flagged as unsafe, and a correct write, sync, rename, `sync_dir` protocol must pass.
- Track CI runtime: a fast seed budget runs per PR, and a large budget runs nightly.

## References

- Pillai et al., "All File Systems Are Not Created Equal" (OSDI 2014), the ALICE crash-vulnerability study
- [PostgreSQL fsync errors (fsyncgate)](https://wiki.postgresql.org/wiki/Fsync_Errors)
- `crates/pizzlydb-storage/src/vfs_memory.rs`, `crates/pizzlydb-storage/src/vfs.rs`
