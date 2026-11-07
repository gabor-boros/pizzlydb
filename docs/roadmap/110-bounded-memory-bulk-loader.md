# 110: Bounded-memory bulk loader with a bottom-up B-tree builder

| Field | Value |
| --- | --- |
| Phase | 2 Operational layer |
| Priority | Must |
| Size | L |
| Crates | `pizzlydb-storage` (new `BulkBuilder`), `pizzlydb-graph` (`bulk`) |
| Type | Feature, performance |

## Summary

Replace the single-batch loader with a streaming loader that builds a new database at near-sequential I/O speed. The pipeline:

1. Read the input as a stream (`impl BufRead`, never a `&str` holding the whole input).
2. Assign ids in the documented phase order.
3. Generate every key (records, both adjacency directions, membership, property cells, index entries).
4. Sort the keys externally, spilling to temporary files under a memory budget.
5. Build the B-tree bottom-up from the sorted key stream, directly into an unpublished database file, with no WAL.
6. Write the namespace registry, statistics, and a bulk-load commit record.
7. Run `verify_full`.
8. Publish atomically.

The loader supports multiple namespaces.

## Motivation

The current `bulk_load` has three problems, all contrary to the goal of a bounded-memory import:

- It takes the whole script as `&str`.
- It applies everything in one `GraphBatch`, so memory grows with the dataset ([019](019-bounded-write-batches.md)).
- It inserts through the normal B-tree path, with random writes and full WAL images.

Initial data loading is the first thing a new production user does.

## Current State

- `crates/pizzlydb-graph/src/bulk.rs`: `bulk_load(vfs, script: &str, destination, namespace, LoadOptions { temp_budget })` runs `validate` and `collect_runs` (spill runs) and then applies the runs inside a single `GraphBatch`. [002](002-offline-tool-identity-and-limits.md) fixes its file identity.
- No bottom-up builder exists in storage.

## Scope

### In scope

- **`storage::BulkBuilder`.** It accepts keys in strictly ascending order and writes packed leaves, then the internal levels, then the allocator state and the superblock, all with no WAL. The fill factor is configurable. It is crash-safe by construction, because the output stays unpublished until it is complete.
- **External sort** with k-way merge over runs, under a configurable memory and temporary-disk budget.
- **Streaming input.** The existing command-script parser is kept as the engine-native format. CSV and JSONL formats come in [215](215-csv-jsonl-import-export.md).
- **System state.** The namespace registry, the counters ([046](046-graph-statistics.md)), and the id allocator high-water marks are written for each namespace. One synthetic commit record marks the load. The ADR in this issue documents that the load has no per-mutation log records, so plans bootstrap from the snapshot.
- **Equivalence test** between transactional loading and bulk loading, comparing all keys.

### Out of scope

- Loading into a non-empty live database. That stays a non-goal unless an RFC is written.

## Dependencies

- [024](024-storage-format-versioning-and-golden-fixtures.md): the format the builder writes.
- [045](045-namespace-registry-and-lifecycle.md): the registry.
- [032](032-logical-mutation-log.md): the commit-record semantics.
- [040](040-type-system-and-value-model.md): value encodings.

## Acceptance Criteria

- [ ] Loading 100 million edges uses at most the configured memory, plus a constant (benchmark with RSS tracking).
- [ ] Load throughput is at least 5x that of a transactional load (benchmark, figure published).
- [ ] The bulk-loaded and transactionally loaded databases have identical key sets (property test on small inputs).
- [ ] A crash at any point leaves no published database (crash simulation).

## Testing and Validation

- Property tests, benchmarks, and crash simulation.

## References

- `crates/pizzlydb-graph/src/bulk.rs`
