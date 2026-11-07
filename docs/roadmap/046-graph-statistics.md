# 046: Graph statistics for planning and quotas

| Field | Value |
| --- | --- |
| Phase | 1C Graph model completion |
| Priority | Must |
| Size | M |
| Crates | `pizzlydb-graph`, `pizzlydb-txn` (`merge_add`) |
| Type | Feature |

## Summary

Maintain the per-namespace statistics that cost-based planning ([056](056-planner-cost-model-and-explain.md)) and quota accounting ([208](208-multi-tenancy-and-quotas.md)) need:

- node count per label and edge count per type
- total nodes and edges
- an approximate out-degree and in-degree distribution per edge type, as a histogram or sketch
- the number of distinct values per index (approximate)
- bytes per namespace

Updates must never create optimistic-concurrency conflicts.

## Motivation

Guide §5.4 requires the planner to estimate cost and to warn about or reject expensive queries, which needs cardinalities. Exact counters maintained with read-modify-write inside transactions would make every write in a namespace conflict with every other write. The commutative update mechanism from [033](033-optimistic-transaction-manager.md) (`merge_add` applied in the commit section) avoids that conflict for counts. Degree histograms and distinct-value estimates can be refreshed by sampling.

## Current State

- No statistics exist.
- `estimate_range` from [018](018-storage-range-operations.md) will provide a cheap range-size estimate.

## Scope

### In scope

- **Exact commutative counters,** under system keys: label member count, type member count, total nodes and total edges, and entity count per index. They are updated through `merge_add` in the same commit.
- **Sampled statistics:** degree histogram per edge type (and per label, optionally) and approximate distinct values per index. They are refreshed by a maintenance job (later folded into [115](115-background-maintenance-scheduler.md)) and are versioned with their sampling time.
- **API:** `GraphStats::label_count`, `type_count`, `degree_estimate(type, direction, quantile)`, and `index_ndv`. Each statistic documents whether it is exact or approximate, and what "as of" means.
- **Namespace size:** byte accounting per namespace, using estimates or counters.

### Out of scope

- Planner use ([056](056-planner-cost-model-and-explain.md)) and quota enforcement ([208](208-multi-tenancy-and-quotas.md)).

## Dependencies

- [033](033-optimistic-transaction-manager.md): `merge_add`.
- [035](035-graph-on-transactions.md): graph on transactions.

## Acceptance Criteria

- [ ] After random histories, exact counters equal the counts computed from the reference model (property test), including across crashes.
- [ ] Concurrent writers never conflict because of statistics updates (stress test).
- [ ] Sampled degree estimates fall within a documented error on power-law test graphs.

## Testing and Validation

- Property tests and a power-law dataset check from testkit ([003](003-testkit-reference-model-and-property-testing.md)).

## References

- [Guide §5.4 cost-based safeguards](../architecture/implementation-guide.md#54-cost-based-safeguards)
