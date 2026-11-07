# 018: Storage range operations: bounded and reverse scans, range delete, size estimates

| Field | Value |
| --- | --- |
| Phase | 1A Storage engine hardening |
| Priority | Must |
| Size | M |
| Crates | `pizzlydb-storage` (`btree`, `engine`, `StorageEngine` trait), `pizzlydb-graph` (`KeyRead`) |
| Type | Feature |

## Summary

Extend the key-value API with the range primitives that the transaction, graph, query, and maintenance layers need:

1. **Bounded scans:** scans with an explicit `[start, end)` and a reverse direction.
2. **Range deletes:** `delete_range(start, end)` within a write batch, usable in chunks for very large ranges.
3. **Size estimates:** cheap estimates of key count and bytes for a range, for planner statistics and quotas.

## Motivation

- **Read-set tracking.** Serializable validation ([033](033-optimistic-transaction-manager.md)) must record which key *ranges* a transaction read, to detect phantoms. Today `scan(from)` has no end bound; callers stop when the prefix ends, so the storage layer never learns the range that was actually read.
- **Descending order.** `ORDER BY ... DESC` and "latest" lookups (for example, the newest commit-log entry) need reverse iteration ([058](058-ordering-and-pagination.md), [032](032-logical-mutation-log.md)).
- **Range delete.** Dropping a namespace ([045](045-namespace-registry-and-lifecycle.md)) or an index ([044](044-online-index-build-and-drop.md)), and truncating logs ([114](114-retention-management-and-inspection.md)), all need it; deleting key by key through a staged map does not scale.
- **Estimates.** The planner ([056](056-planner-cost-model-and-explain.md)) and quotas ([208](208-multi-tenancy-and-quotas.md)) need range-size estimates.

## Current State

- `crates/pizzlydb-storage/src/btree.rs`: `scan_from` (forward only) and `next`; there is no reverse cursor and no range delete.
- `StorageEngine` trait and `ReadView::scan(from)` in `engine.rs`; `KeyRead::scan(from)` in `crates/pizzlydb-graph/src/access.rs`.
- `WriteBatch` stages a `BTreeMap<Vec<u8>, Option<Vec<u8>>>`, so a range delete today means reading every key and staging a tombstone for each one.

## Scope

### In scope

- `scan_range(range: impl RangeBounds<[u8]>, Direction)` on read views, write batches (merged with staged writes), and `KeyRead`.
- `WriteBatch::delete_range(start, end)`, represented in the stage as a range tombstone. Reads within the batch honor it, and commit applies it efficiently: subtree pages are retired wholesale where possible, or the deletion is chunked under the size limits of [019](019-bounded-write-batches.md).
- `estimate_range(range) -> RangeEstimate { keys_lower, keys_upper, bytes }` computed from the B-tree descent, without a full scan, and documented as an estimate.
- Rustdoc and doctests for each operation.

### Out of scope

- Read-set recording itself ([033](033-optimistic-transaction-manager.md)); this issue only makes the bounds explicit.

## Dependencies

- [010](010-storage-error-taxonomy-fidelity.md): error taxonomy.

## Acceptance Criteria

- [x] Model-based tests against a `BTreeMap` reference cover forward and reverse bounded scans over snapshots and over write batches with staged writes and range tombstones.
- [ ] `delete_range` over 1,000,000 keys commits in at most a documented number of pages written, and the freed pages are reclaimed (test plus benchmark).
- [x] The documented error bounds of `estimate_range` hold on uniform and skewed datasets (test).
- [x] The graph `KeyRead` exposes bounded scans, and existing graph callers migrate to them.

## Testing and Validation

- Property tests and crash-simulation runs that interleave range deletes with checkpoints.
- Benchmarks before and after for prefix scans.

## References

- `crates/pizzlydb-storage/src/{btree,engine}.rs`, `crates/pizzlydb-graph/src/access.rs`
