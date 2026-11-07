# 054: Graph read interfaces for query execution

| Field | Value |
| --- | --- |
| Phase | 1D Query engine |
| Priority | Must |
| Size | M |
| Crates | `pizzlydb-graph`, `pizzlydb-query` |
| Type | Feature |

## Summary

Provide a read API, bound to a snapshot, that a query executor can use efficiently:

- batched lookup of nodes and edges, with labels and selected properties
- adjacency expansion with type and direction filters, optionally bounded and reverse ordered
- label and type scans with resumable positions
- index point and range lookups with bounds and direction
- property projection for many entities
- cardinality estimates

Every method is streaming, has bounded memory, and is cancellable.

## Motivation

The current graph read API returns single records or iterators that each re-enter storage, and properties are fetched one key at a time. An executor needs to fetch the properties it actually uses in bulk, resume scans for pagination ([058](058-ordering-and-pagination.md)), and check cancellation between batches. Isolating this interface also lets the testkit's reference graph implement the same trait for differential tests.

## Current State

- `crates/pizzlydb-graph/src/adjacency.rs` has `neighbors` and `neighbors_of_type`.
- `index.rs` has `index_lookup`, `index_range`, `nodes_with_label`, and `edges_with_type`.
- `property.rs` has `get_property`, one key at a time.
- `record_store.rs` has `get_node` and `get_edge`.

## Scope

### In scope

- A `GraphRead` trait implemented by graph transactions and snapshots. Methods:
  - `nodes(ids, projection)` and `edges(ids, projection)`
  - `expand(node, dir, types, bounds)`, which yields `(edge, neighbor)`
  - `label_scan(label, from)` and `index_scan(index, range, dir, from)`
  - `estimate_*`
- A resumable position type for each scan, valid only within the same snapshot.
- A cancellation token that is checked at a documented batch granularity.
- Read-set tracking compatible with [033](033-optimistic-transaction-manager.md), so that a query running inside a read-write transaction is validated correctly.
- An implementation of the same trait for the testkit's `ReferenceGraph`.

### Out of scope

- Operators and planning ([055](055-physical-operators-and-executor.md), [056](056-planner-cost-model-and-explain.md)).

## Dependencies

- [035](035-graph-on-transactions.md): graph on transactions.
- [018](018-storage-range-operations.md): bounded and reverse scans.
- [046](046-graph-statistics.md): estimates.

## Acceptance Criteria

- [ ] Contract tests run the same scenarios against the engine and the reference implementations, with identical results.
- [ ] Batched property projection for 1,000 entities issues at most one storage descent per entity and property, and avoids redundant root descents through cursor reuse (benchmark).
- [ ] Cancellation is observed within one batch (test).

## Testing and Validation

- Contract tests, benchmarks, and property tests against the reference.

## References

- [Guide §16.1](../architecture/implementation-guide.md#161-minimal-internal-interface-boundaries)
- `crates/pizzlydb-graph/src/{adjacency,index,property,record_store}.rs`
