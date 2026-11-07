# 055: Physical operators and the pull-based executor

| Field | Value |
| --- | --- |
| Phase | 1D Query engine |
| Priority | Must (critical path) |
| Size | XL |
| Crates | `pizzlydb-query` (`executor`) |
| Type | Feature |

## Summary

Implement the physical operators and a streaming executor for snapshot queries. The operators are:

- **Access:** node by id, label scan, and index seek or range
- **Traversal:** expand in, out, or both, with a type filter
- **Joins:** an index nested-loop join and a hash join for fixed patterns
- **Relational:** filter, project, distinct, hash aggregate (`COUNT`, `SUM`), sort (bounded memory, top-k with `LIMIT`), and skip and limit

Every operator tracks its cardinality, fan-out, and memory, and it honors deadlines and cancellation.

## Motivation

The executor is the core of the snapshot query engine (guide §5.2). Guide §5.4 requires per-operator tracking of cardinality, expansion fan-out, intermediate memory, deadlines, and output limits, together with deterministic cancellation. An iterator-style pull model keeps memory bounded and lets results stream to clients.

## Current State

- `crates/pizzlydb-query/src/executor.rs` is a one-line stub.

## Scope

### In scope

- An operator trait with `open`, `next_batch`, and `close`. It uses batch-at-a-time pulls, with a configurable batch size, to amortize overhead.
- Each operator listed in the summary, with exact multiset semantics and the path modes ([050](050-query-language-specification.md)).
- A memory accountant passed into the operators. Hash join, distinct, aggregate, and sort charge their memory to it. Exceeding the budget fails with `QUERY_TOO_EXPENSIVE`. Spilling to disk is not part of v1 and is documented as such.
- An execution context with the deadline, the cancellation token, per-operator stats (rows in and out, time, memory peak), and warnings such as a high-degree node encountered.
- Results stream as typed rows with a schema.

### Out of scope

- Plan selection ([056](056-planner-cost-model-and-explain.md)) and variable-length paths ([057](057-bounded-variable-length-paths.md)).

## Dependencies

- [052](052-binder-type-checker-and-ir.md): the IR.
- [054](054-graph-read-interfaces-for-execution.md): graph reads.

## Acceptance Criteria

- [ ] A differential test against the reference evaluator, for hand-built physical plans covering each operator, finds zero divergences on 10,000 random graphs.
- [ ] A query whose hash-join state exceeds the budget fails cleanly with `QUERY_TOO_EXPENSIVE`. Memory is released afterwards (test with a counting allocator).
- [ ] Cancellation takes effect within one batch, and deadlines are enforced (tests).
- [ ] Per-operator stats are exposed for EXPLAIN ANALYZE ([116](116-diagnostics-and-introspection.md)).

## Testing and Validation

- Differential tests, plus benchmarks for one-hop, two-hop, and triangle patterns on power-law graphs.

## References

- [Guide §5.2](../architecture/implementation-guide.md#52-supported-core-snapshot-operations), [§5.4](../architecture/implementation-guide.md#54-cost-based-safeguards)
- Graefe, "Volcano: An Extensible and Parallel Query Evaluation System" (1994)
