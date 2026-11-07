# 113: Space reclamation, B-tree compaction, file shrinking, and tombstone GC

| Field | Value |
| --- | --- |
| Phase | 2 Operational layer |
| Priority | Must |
| Size | L |
| Crates | `pizzlydb-storage` (`btree`, `space`, `engine`), `pizzlydb-graph` (tombstones) |
| Type | Feature, maintenance |

## Summary

Make long-running databases sustainable:

- **Compaction.** Merge underfilled B-tree pages beyond what delete rebalancing does, online and within a budget.
- **Relocation and shrinking.** Move pages from the end of the file into free space, then truncate the file with `set_len` from [011](011-vfs-capabilities.md).
- **WAL truncation.** Shrink the WAL file back to its configured size after a reset.
- **Tombstone GC.** Collect graph tombstones under the policy decided in [034](034-id-allocation-and-identity-policy.md).
- **Metrics.** Expose fragmentation and space-amplification measures.

## Motivation

Only page reclaim at the snapshot horizon exists. There is no tombstone GC, vacuum, or compaction. Without the work in this issue:

- a database that once grew never shrinks
- delete-heavy workloads leave sparse trees, which increases scan cost
- permanent tombstones grow without bound

Guide §10.3 requires metrics for compaction backlog and write amplification.

## Current State

- `crates/pizzlydb-storage/src/maintain.rs` is a thin wrapper around `reclaim_to_horizon`.
- The B-tree rebalances on delete (`rebalance`, `propagate_min` in `btree.rs`). No compaction pass exists.
- The VFS cannot truncate until [011](011-vfs-capabilities.md) lands, and the WAL file never shrinks.
- `crates/pizzlydb-graph/src/record_store.rs` keeps tombstones forever.

## Scope

### In scope

- **Compaction pass.** It walks key ranges and merges adjacent leaves below a fill threshold, as normal transactional B-tree mutations in bounded batches.
- **Tail relocation.** It copies pages that live above a target boundary into lower free pages and rewrites the parent child pointer (and the sibling links), then truncates once the tail is entirely free. It is safe under concurrent snapshots, because retirement goes through the horizon.
- **WAL truncation** to the configured size after a reset, when the WAL exceeds it.
- **Tombstone GC** per policy: removal after a retention period while id non-reuse is preserved, through the allocator high-water mark.
- **Metrics:** file bytes, live bytes, free pages, fill-factor histogram, and compaction backlog.
- **Scheduling hooks.** Each operation is exposed as a job for the scheduler in [115](115-background-maintenance-scheduler.md).

### Out of scope

- A copy-build-publish full rewrite. Online compaction is preferred, and a full rewrite is available through export and import.

## Dependencies

- [011](011-vfs-capabilities.md): truncate.
- [014](014-scalable-space-allocator.md): the allocator.
- [021](021-automatic-checkpointing-and-wal-bounds.md): WAL reset.
- [034](034-id-allocation-and-identity-policy.md): the tombstone policy.

## Acceptance Criteria

- [ ] Insert 10 GiB, delete 90%, and compact: the file shrinks to within 1.5x of the live bytes, while concurrent reads and writes continue (soak test).
- [ ] Crash simulation during relocation and truncation loses no data.
- [ ] `verify_full` passes after every compaction step.
- [ ] Tombstone GC follows the policy, and `get_node` on a collected id still reports `Deleted` (test).

## Testing and Validation

- Soak tests, crash simulation, and benchmarks of compaction throughput and its impact on foreground p99.

## References

- [Guide §10.3](../architecture/implementation-guide.md#103-telemetry)
- `crates/pizzlydb-storage/src/{maintain,btree,space}.rs`
