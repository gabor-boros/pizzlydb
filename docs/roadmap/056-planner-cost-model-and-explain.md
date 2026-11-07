# 056: Planner, cost model, resource safeguards, EXPLAIN, and the plan cache

| Field | Value |
| --- | --- |
| Phase | 1D Query engine |
| Priority | Must (critical path) |
| Size | XL |
| Crates | `pizzlydb-query` (`planner`) |
| Type | Feature |

## Summary

Turn the typed IR into a physical plan. The planner:

- chooses access paths (an id seek, an index seek or range, or a label scan)
- orders pattern expansion and joins using cardinality estimates from [046](046-graph-statistics.md)
- computes a cost and resource bound for every plan
- rejects plans that exceed policy limits, or whose resource use it cannot bound
- produces `EXPLAIN` output describing the plan
- caches parameterized plans, keyed by schema version and statistics epoch

## Motivation

The guide (§5.4) requires plans to be rejected when they use unsupported recursion or have unverifiable resource bounds. It also requires policy limits on depth, visited edges, join state, concurrent scans, and output, plus `EXPLAIN` output with access paths, cardinality assumptions, and cost caps (§5.2), and version-aware plan caching. A poor join order on a graph with hubs can turn a millisecond query into a query that never finishes.

## Current State

- `crates/pizzlydb-query/src/planner.rs` is a one-line stub.

## Scope

### In scope

- **Access paths.** Rule-based choice of access path, with cost-based ordering of expansion and joins for fixed patterns (greedy or dynamic programming for small patterns). Index use is restricted to `Ready` indexes ([044](044-online-index-build-and-drop.md)).
- **Cardinality estimation.** Based on label and type counts, degree estimates, and index selectivity, with explicit default selectivities where no statistics exist.
- **Resource-bound computation.** An upper bound on visited edges and on intermediate rows, computed from degree quantiles and limits. Plans with unbounded or unverifiable cost are rejected with `QUERY_TOO_EXPENSIVE` or `QUERY_UNSUPPORTED`, and the error carries a reason.
- **Policy limits.** Configurable per namespace and per principal, with defaults.
- **`EXPLAIN`.** The plan tree with estimated rows, cost, the chosen and rejected access paths, and warnings. The output is both human-readable and structured.
- **Plan cache.** Keyed by `(IR fingerprint, parameter types, schema_version, stats_epoch)`, and bounded with LRU eviction.

### Out of scope

- Incremental plans ([062](062-incremental-admissibility-and-compiler.md)) and adaptive re-optimization.

## Dependencies

- [055](055-physical-operators-and-executor.md): the operators.
- [046](046-graph-statistics.md): statistics.

## Acceptance Criteria

- [ ] For every conformance query, the chosen plan's results equal the reference results, and plan selection is deterministic.
- [ ] *Plan equivalence:* forcing every alternative access path (an index, a label scan, a different join order) gives identical results. This is guide invariant C4 at the query level and is checked by a property test.
- [ ] On a power-law benchmark graph, the chosen plan is within 2x of the best plan found by exhaustive search, for at least 90% of a benchmark query set.
- [ ] A query that would expand a 1-million-degree hub without a limit is rejected, or bounded with a warning, according to policy (test).
- [ ] `EXPLAIN` output is stable and covered by snapshot tests.

## Testing and Validation

- Property tests, plan snapshot tests, and benchmarks.

## References

- [Guide §5.2](../architecture/implementation-guide.md#52-supported-core-snapshot-operations), [§5.4](../architecture/implementation-guide.md#54-cost-based-safeguards)
- `crates/pizzlydb-query/src/planner.rs`
