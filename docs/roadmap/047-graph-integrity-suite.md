# 047: Graph integrity and model-based suite (exit gate for Phases 1B and 1C)

| Field | Value |
| --- | --- |
| Phase | 1C Graph model completion |
| Priority | Must (milestone gate) |
| Size | L |
| Crates | `pizzlydb-graph`, `pizzlydb-testkit`, `tests/model-based/` |
| Type | Verification |

## Summary

This is the permanent graph-level correctness suite that defines Milestone M2, "Serializable transactional graph core". It runs randomized concurrent graph histories against the reference model. The histories mix every graph operation, including detach delete, schema changes, online index builds, and namespace drops, and they include crashes and restarts. After every step the suite checks the integrity invariants, the mutation-log bijection (S2), and that every index is exact (C4).

## Motivation

Phases 1B and 1C change how every graph write works. Each issue tests its own feature; this gate tests the combinations, under concurrency and crashes, against one oracle.

## Current State

- The graph crate has about 44 example-based integration tests and 63 doctests.
- `verify_full` exists but collects every id into memory (`crates/pizzlydb-graph/src/verify.rs`, `scan_ids`).

## Scope

### In scope

- **Operations:** every operation from the graph API and the issues before this one, all run through the testkit harness ([003](003-testkit-reference-model-and-property-testing.md)).
- **Invariants checked after each step:**
  - no dangling edges
  - adjacency is symmetric
  - label and type membership is exact
  - property and unique indexes are exact
  - constraints hold
  - the schema version is monotonic
  - counters ([046](046-graph-statistics.md)) are exact
  - replaying the log onto an empty model equals the engine state (S2)
- **Concurrency:** histories run on several threads under optimistic transactions, and committed transactions are applied to the model in `CommitSeq` order.
- **Crashes:** crash and reopen at random points using the crash simulator ([004](004-crash-consistency-simulation-vfs.md)).
- **Verification cost:** `verify_full` streams in bounded memory, with no `Vec` of every id. This also serves [107](107-full-verification-and-online-scrubbing.md).
- A milestone report in `docs/milestones/M2.md`.

### Out of scope

- Query semantics ([059](059-query-correctness-suite.md)).

## Dependencies

- Transactions: [036](036-isolation-verification-suite.md).
- Graph features: [041](041-schema-constraints-at-commit.md), [042](042-detach-delete.md), [043](043-schema-and-catalog-lifecycle.md), [044](044-online-index-build-and-drop.md), [045](045-namespace-registry-and-lifecycle.md), [046](046-graph-statistics.md).
- Storage gate: [025](025-storage-model-based-and-crash-suite.md).

## Acceptance Criteria

- [ ] Each PR runs 1,000 or more concurrent histories, and the nightly job runs 100,000 or more, with zero divergences for 14 consecutive nights.
- [ ] The S2 bijection holds after every crash and reopen.
- [ ] The memory used by `verify_full` is independent of graph size (benchmark assertion).
- [ ] The M2 milestone report is published, together with the isolation-suite results ([036](036-isolation-verification-suite.md)).

## Testing and Validation

- Meta-tests: an injected missing index-entry write, or a missing log record, is detected within the PR budget.

## References

- [Guide §13 M1/M2](../architecture/implementation-guide.md#m2--transactional-graph-core), [§12.2](../architecture/implementation-guide.md#122-mandatory-correctness-properties)
- `crates/pizzlydb-graph/src/verify.rs`
