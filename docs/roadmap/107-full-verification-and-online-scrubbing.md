# 107: Multi-namespace full verification and online scrubbing

| Field | Value |
| --- | --- |
| Phase | 2 Operational layer |
| Priority | Must |
| Size | L |
| Crates | `pizzlydb-storage` (`verify`), `pizzlydb-graph` (`verify`), `pizzlydb-incremental`, `pizzlydb-engine` |
| Type | Feature, integrity |

## Summary

Turn verification into a production capability with two parts.

1. **Database-wide `verify_full`.** It covers every namespace, the system key space, and maintained state, in bounded memory, against a snapshot of a live database or a read-only offline open. It checks:
   - the storage structure
   - graph integrity
   - index exactness
   - commit-log continuity: commit records dense from the retention floor, and log records matching their commit records
   - subscription and plan metadata consistency
   - optionally, recomputation of each maintained plan at its `maintained_seq`
2. **A background scrubber.** It re-reads every allocated page at a throttled rate, verifies checksums so that latent corruption is detected early, and reports progress and findings.

## Motivation

Guide §3.4 requires secondary indexes to be reconstructible and to have documented repair. Guide §10.3 requires corruption detection to be measurable. Today `verify_full` covers one namespace and collects every id in memory (`scan_ids`). Nothing checks pages that are rarely read, so a bad sector can sit unnoticed until a restore is needed.

## Current State

- `crates/pizzlydb-storage/src/verify.rs`: `verify_quick` checks page headers, allocation, and the tree, and opens through `Storage::open`.
- `crates/pizzlydb-graph/src/verify.rs`: `verify_full(graph, view)` checks a single namespace. `scan_ids` collects a `Vec` of all node and edge ids.

## Scope

### In scope

- **Verification of all namespaces** from the registry ([045](045-namespace-registry-and-lifecycle.md)), plus the system key space. It streams in bounded memory by using merge-style comparisons of sorted key ranges instead of in-memory sets.
- **Commit-log checks:** density from the floor, commit-record and log-record pairing, and monotonic timestamps (warn only).
- **Plan checks:** the state version is valid, and optionally the plan result is recomputed at `maintained_seq` through an as-of snapshot or after quiescing, then compared with the maintained state, within a budget.
- **Report format.** A structured report listing the findings with severity, location, and the suggested repair ([109](109-repair-procedures-and-salvage-hardening.md)).
- **Scrubber.** Throttled, resumable (its progress is persisted), and covering both the main file and the WAL frames. It feeds corruption metrics and alerts ([102](102-metrics-foundation.md)).

### Out of scope

- Automatic repair ([109](109-repair-procedures-and-salvage-hardening.md)).

## Dependencies

- [045](045-namespace-registry-and-lifecycle.md): the namespace registry.
- [032](032-logical-mutation-log.md): the commit log.
- [064](064-maintained-state-and-output-log.md): maintained state.
- [016](016-thread-safe-storage-engine.md): verification against a live snapshot.

## Acceptance Criteria

- [ ] Peak memory of `verify_full` on a 100 million edge database is bounded by the configuration and independent of size (benchmark).
- [ ] Each injected corruption class is detected and classified correctly: a missing index entry, a missing reverse adjacency key, a commit-record gap, a flipped bit in a cold page, and a mismatch between plan state and recomputation (fault tests).
- [ ] The scrubber covers the whole file within its configured period under load and stays within its I/O budget (soak test).

## Testing and Validation

- Fault-injection tests, benchmarks, and a soak test.

## References

- [Guide §3.4](../architecture/implementation-guide.md#34-initial-physical-index-set), [§10.3](../architecture/implementation-guide.md#103-telemetry)
- `crates/pizzlydb-storage/src/verify.rs`, `crates/pizzlydb-graph/src/verify.rs`
