# 020: Group commit and well-defined durability modes

| Field | Value |
| --- | --- |
| Phase | 1A Storage engine hardening |
| Priority | Must |
| Size | L |
| Crates | `pizzlydb-storage` (`wal`, `pager`, `engine`) |
| Type | Performance, durability semantics |

## Summary

Introduce a commit pipeline that amortizes one WAL sync over many concurrent commits (group commit) without weakening `Durability::Full`, which still acknowledges only after the data is durable. Define and implement exact semantics for each durability mode:

- **`Full`:** acknowledged only after the commit's WAL frames are durable (one shared sync for the group).
- **`Grouped { max_delay }`:** the same guarantee, but the pipeline waits up to `max_delay` to gather more commits before syncing.
- **`Deferred`** (today's `None`): acknowledged once the frames are written, not synced. A background flusher syncs at least every configured interval, which bounds the loss window. `Storage::sync()` lets callers force durability explicitly.

Use `fdatasync` ([011](011-vfs-capabilities.md)) where it is sufficient.

## Motivation

With one fsync per commit, throughput is capped at the device's sync rate (on the order of 100 to 10,000 per second). Group commit is a standard production requirement. Today `Durability::None` gives no bound on data loss and offers no explicit sync, so it is unsafe for any use case beyond tests. Durability guarantees must be explicit and tested (guide §1.4).

## Current State

- `crates/pizzlydb-storage/src/engine.rs`: `Durability::{Full, None}`. Durability is chosen per database at open, or per batch on `commit`.
- `crates/pizzlydb-storage/src/wal.rs`: `Wal::commit(durability, ...)` syncs inline when the mode is `Full`.
- `vfs_posix.rs`: every sync is `sync_all`.
- There is no concurrency yet; [016](016-thread-safe-storage-engine.md) provides it.

## Scope

### In scope

- A commit queue serialized by a commit lock. A "leader" thread writes the frames of all queued batches, issues one `sync_data`, and then wakes every waiter with its result. A failure poisons every waiter and the handle, consistent with [022](022-disk-full-and-io-error-safety.md).
- Durability modes as above, an explicit `Storage::sync()`, and a background flusher thread for `Deferred`.
- A documented loss window for each mode, verified by crash simulation.
- Metrics hooks: group size histogram, sync latency, commit queue wait ([102](102-metrics-foundation.md)).

### Out of scope

- Optimistic validation in the commit queue ([033](033-optimistic-transaction-manager.md)), but the pipeline must be designed so that validation can run inside the commit critical section.

## Dependencies

- [016](016-thread-safe-storage-engine.md): concurrency.
- [011](011-vfs-capabilities.md): `sync_data`.
- [013](013-disk-resident-wal-index.md): WAL append path.

## Acceptance Criteria

- [ ] Crash simulation: no commit acknowledged under `Full` or `Grouped` is ever lost. Under `Deferred`, loss is limited to commits newer than the last completed flusher sync, and the database is always consistent.
- [ ] Benchmark: 64 concurrent committers on a real SSD reach at least 5x the single-sync-per-commit throughput, while p99 latency for `Full` stays within 2x the device sync latency.
- [ ] A failed sync returns an error to every batch in the group, and no group member is acknowledged.
- [x] Rustdoc for `Durability` states the exact guarantee of each mode.

## Testing and Validation

- Crash-simulation enumeration with concurrent committers.
- A `loom` model of the leader/follower handoff.

## References

- [Guide §1.4 design principles](../architecture/implementation-guide.md#14-design-principles), [§4.3](../architecture/implementation-guide.md#43-atomic-commit-record)
- `crates/pizzlydb-storage/src/{engine,wal,vfs_posix}.rs`
