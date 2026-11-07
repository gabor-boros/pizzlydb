# 035: Port the graph layer onto the transaction manager and emit mutation records

| Field | Value |
| --- | --- |
| Phase | 1B Transactions and logical commit log |
| Priority | Must (critical path) |
| Size | L |
| Crates | `pizzlydb-graph`, `pizzlydb-txn` |
| Type | Refactor, feature |

## Summary

Replace `GraphBatch`, which holds the storage writer lock and stages its own map, with a graph transaction built on `pizzlydb-txn::Txn`. Every graph write produces `GraphMutation` records ([032](032-logical-mutation-log.md)). All reads go through tracked transaction reads, ids come from the allocator in [034](034-id-allocation-and-identity-policy.md), and the graph crate stops calling `Storage::begin_write` directly.

## Motivation

This puts the dependency direction graph → txn → storage required by guide §11.1 into practice, and it makes graph operations serializable, concurrent, and logged. Without it the transaction manager exists but is unused, and invariant S2 cannot hold for graph data.

## Current State

- `crates/pizzlydb-graph/src/batch.rs`: `GraphBatch::begin(&Storage)` acquires the writer lock, keeps a staged `BTreeMap` and an undo log, and copies into a `WriteBatch` on commit.
- Every graph API takes `&mut GraphBatch` for writes and a `KeyRead` for reads (`crates/pizzlydb-graph/src/access.rs`).
- Tests: 5 unit tests, about 44 integration tests, and 63 doctests in the graph crate.

## Scope

### In scope

- `GraphTxn`, wrapping `Txn`, implements `KeyRead` with read tracking. Operation-level atomicity is kept: a failed operation leaves no partial staged state, using the existing undo log or a savepoint inside `Txn`.
- Each write operation appends the corresponding `GraphMutation`, with before-images where ADR-005 requires them.
- Unique-index and endpoint-existence checks read through tracked reads, so that concurrent violations are caught by validation.
- `bulk_load`, `salvage`, and `verify_full` adapt: verification uses read-only snapshots, and the bulk and salvage paths keep using dedicated builders where documented ([110](110-bounded-memory-bulk-loader.md)).
- All existing graph tests and doctests are ported, and new tests cover concurrent conflicting operations.
- `pizzlydb-graph` no longer depends on the storage write API, enforced by a `cargo deny` ban or a dependency check.

### Out of scope

- New graph semantics; issues 041–045 add them.

## Dependencies

- [033](033-optimistic-transaction-manager.md), [032](032-logical-mutation-log.md), and [034](034-id-allocation-and-identity-policy.md): the transaction manager, the mutation log, and the id allocator.
- [002](002-offline-tool-identity-and-limits.md): bulk load and salvage create fresh file identities before they move onto transactions.

## Acceptance Criteria

- [ ] Every graph mutation API produces exactly the expected `GraphMutation` sequence (table-driven tests for each operation).
- [ ] S2 bijection: a randomized history test replays the log onto an empty reference model and reaches state equal to the engine's (uses [003](003-testkit-reference-model-and-property-testing.md)).
- [ ] Two concurrent transactions that insert the same unique-index value: exactly one commits and the other gets `TxConflict` or `ConstraintViolation`.
- [ ] Concurrent edge creation against a node deleted by another transaction never produces a dangling edge.
- [ ] No graph-crate source references `begin_write` (grep check in CI).

## Testing and Validation

- The ported suites, new concurrency tests, and the S2 property test.

## References

- [Guide §11.1](../architecture/implementation-guide.md#111-dependency-discipline), [§3.2](../architecture/implementation-guide.md#32-integrity)
- `crates/pizzlydb-graph/src/{batch,access,record_store,index,property,catalog}.rs`
