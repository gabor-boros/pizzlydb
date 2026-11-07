# 022: Disk-full and I/O-error safety

| Field | Value |
| --- | --- |
| Phase | 1A Storage engine hardening |
| Priority | Must |
| Size | M |
| Crates | `pizzlydb-storage` |
| Type | Robustness, durability |

## Summary

Specify and test engine behavior when the disk fills up (`ENOSPC`, `EDQUOT`) and when writes or syncs fail:

- No acknowledged commit is lost, and the files are never left in a state that fails to reopen.
- A checkpoint must always be able to make progress, using reserved space.
- A failed sync is fatal and poisons the handle (Linux fsync semantics).
- The engine enters a defined *read-only degraded* state that serves snapshots and rejects writes with a typed error until space is restored and the handle is reopened.

## Motivation

Disk-full is one of the most common production incidents. Several paths in the current code can wedge the engine, because a checkpoint that cannot write the main file cannot reset the WAL either:

- `WriteBatch::publish` sets `closed = true` after a failed commit, so every subsequent operation fails. That is safe, but it gives no diagnosis and no recovery path.
- A failed `prune` closes the handle.
- File extension (the allocator grows by extents) and WAL appends have no space reservation.

## Current State

- `crates/pizzlydb-storage/src/engine.rs`: commit failure sets `DbState::closed`, and a failed `checkpoint` prune closes the handle.
- `crates/pizzlydb-storage/src/vfs_posix.rs`: only `EINTR` is retried, and every other error becomes `VfsError::Io` with no errno classification visible to callers.
- There is no preallocation (added in [011](011-vfs-capabilities.md)) and no reserved space.

## Scope

### In scope

- Classify `ENOSPC` and `EDQUOT` as `VfsError::NoSpace` and surface them as `StorageError::Full { kind: Disk }`.
- **WAL space reservation:** preallocate WAL capacity for at least one maximum-size batch ([019](019-bounded-write-batches.md)) plus checkpoint headroom, so that a commit cannot fail halfway through writing its frames.
- **Main-file reservation:** pre-extend the main file by extents ahead of allocation, so checkpoint copies never need new space.
- **Degraded mode:** after `NoSpace` during commit, the handle rejects writes with `Full`, keeps serving existing snapshots, and allows `checkpoint` (which needs no new space) and `close`. Recovery is a documented procedure: free space, then reopen.
- **Sync failure:** every `sync` or `sync_data` failure poisons the handle permanently, and the next open re-runs recovery from durable state. Document why a retry is unsafe.
- An operator-facing error message and a stats flag (`degraded_reason`).

### Out of scope

- The quota system ([208](208-multi-tenancy-and-quotas.md)) and disk-usage alerts ([102](102-metrics-foundation.md)).

## Dependencies

- [011](011-vfs-capabilities.md): preallocation.
- [013](013-disk-resident-wal-index.md): WAL append path.
- [021](021-automatic-checkpointing-and-wal-bounds.md): checkpoint behavior when space is short.

## Acceptance Criteria

- [ ] A fault matrix injects `NoSpace` into every write call site (WAL append, WAL header, main extend, main page write, superblock write) and every sync call site. After each injection, the next open succeeds, every acknowledged commit is present, and `verify_quick` passes.
- [ ] Degraded mode: reads succeed and writes fail with `Full` until reopen (integration test).
- [ ] A real-filesystem test on a small loop or tmpfs mount with a size limit passes in a Linux CI job (nightly).
- [ ] A runbook entry describes recovery from disk-full ([221](221-documentation-set.md) collects it).

## Testing and Validation

- Fault-injection and crash-simulation combinations.
- A Linux-only integration test using `tmpfs` with `size=`.

## References

- [Guide §10.4 resource control](../architecture/implementation-guide.md#104-resource-control)
- [PostgreSQL fsync errors (fsyncgate)](https://wiki.postgresql.org/wiki/Fsync_Errors)
- `crates/pizzlydb-storage/src/{engine,wal,vfs_posix}.rs`
