# 044: Online, multi-transaction index build

| Field | Value |
| --- | --- |
| Phase | 1C Graph model completion |
| Priority | Must |
| Size | L |
| Crates | `pizzlydb-graph` (`index`), `pizzlydb-txn` |
| Type | Feature |

## Summary

Build a new property index (unique or non-unique) on existing data without one giant transaction and without blocking writers. The build proceeds in four steps:

1. Register the index as `Building`. From then on, every write maintains its entries.
2. Back-fill the existing entities in bounded chunks, each chunk committed as its own transaction.
3. Validate uniqueness.
4. Mark the index `Ready` in a final transaction that bumps the schema version.

A build is resumable after a crash, cancellable, and reports its progress.

## Motivation

`create_index` builds the whole index inside one batch, so memory and WAL usage grow with the size of the dataset ([019](019-bounded-write-batches.md)), and the build holds the writer for the whole duration. Production systems must be able to add indexes to large, live graphs. The query planner must use an index only once it is `Ready`.

## Current State

- `crates/pizzlydb-graph/src/index.rs`: `create_index` (around lines 270–307) scans the scope and writes every entry in the caller's batch.
- Index definitions have no lifecycle state.

## Scope

### In scope

- Index states `Building { progress_key }`, `Ready`, and `Dropping`, stored in the index definition under a versioned format.
- **Double-write rule.** While the index is `Building`, the normal write path maintains entries for entities it touches. Because the back-fill reads with tracked reads, concurrent modifications either conflict or are handled idempotently. The ADR in this issue must state the correctness argument.
- **Back-fill.** It runs in chunks of N entities in scope order and records the resume key in the definition, so a crashed build continues where it stopped.
- **Unique indexes.** Duplicates are detected during the back-fill and against concurrent writes, and the build fails with a report of the conflicting entities.
- **Control and planner use.** Cancellation transitions the index to `Dropping`, which reuses the drop path from [043](043-schema-and-catalog-lifecycle.md). Progress is reported through stats. Planner and query visibility is limited to `Ready` indexes.

### Out of scope

- Composite indexes ([904](904-composite-and-specialized-indexes.md)).

## Dependencies

- [043](043-schema-and-catalog-lifecycle.md): drop and the schema version.
- [019](019-bounded-write-batches.md): chunk sizing.
- [033](033-optimistic-transaction-manager.md): concurrency semantics.

## Acceptance Criteria

- [ ] Building an index over 10 million nodes keeps peak memory and the WAL size per commit bounded by the chunk configuration (benchmark), while a concurrent write workload keeps running with p99 latency within 3x of the baseline.
- [ ] A crash at any point resumes the build, and the final index is exact: `verify_full` index exactness, guide invariant C4.
- [ ] A unique build over data containing duplicates fails, and the report lists the duplicates.
- [ ] Concurrent writes during the build are reflected exactly once (property test).

## Testing and Validation

- Model-based tests with concurrent writers, crash simulation, and a benchmark.

## References

- [Guide §3.4](../architecture/implementation-guide.md#34-initial-physical-index-set), [§12.2 C4](../architecture/implementation-guide.md#122-mandatory-correctness-properties)
- `crates/pizzlydb-graph/src/index.rs`
