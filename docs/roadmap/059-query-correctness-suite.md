# 059: Query correctness suite (exit gate for Phase 1D)

| Field | Value |
| --- | --- |
| Phase | 1D Query engine |
| Priority | Must (milestone gate) |
| Size | M |
| Crates | `pizzlydb-query`, `pizzlydb-testkit`, `tests/query-conformance/` |
| Type | Verification |

## Summary

This permanent suite defines Milestone M3, "Snapshot query engine". It combines five kinds of checks:

1. The conformance corpus.
2. Differential testing of the optimized engine against the reference evaluator, on seeded random graphs and random queries generated from the grammar.
3. Equivalence of plans that use an index with plans that do not (C4).
4. Fuzzing across parse, bind, and plan.
5. Resource-limit enforcement.

## Motivation

The query engine has many interacting parts. Only randomized differential testing against an independent oracle gives confidence that optimizations preserve semantics across nulls, mixed numerics, parallel edges, self-loops, and cycles.

## Current State

- Not started. The prerequisites are issues 050–058.

## Scope

### In scope

- A random query generator constrained to the v1 grammar, with a controllable size. It is paired with random graphs that include adversarial shapes: hubs, cycles, parallel edges, and self-loops.
- A differential runner: engine results compared with reference results as multisets, or in order when the query has `ORDER BY`. Divergences are minimized, both the graph and the query.
- A forced-plan mode that runs each query under every legal access path and join order.
- A fuzz target over parse, bind, and plan on arbitrary text, asserting no panic.
- A resource-limit suite: deadline, memory, visited edges, and output limits.
- A milestone report in `docs/milestones/M3.md`.

### Out of scope

- Server-level query testing ([203](203-transaction-and-query-service.md)).

## Dependencies

- Oracle and plan components: [053](053-reference-query-evaluator.md), [056](056-planner-cost-model-and-explain.md).
- Feature components: [057](057-bounded-variable-length-paths.md), [058](058-ordering-and-pagination.md).
- Graph gate: [047](047-graph-integrity-suite.md).

## Acceptance Criteria

- [ ] 100% of the conformance corpus passes on the engine.
- [ ] Each PR runs 10,000 random query and graph pairs with zero divergences, and the nightly job runs 1,000,000, with zero divergences for 14 nights.
- [ ] Forced-plan equivalence holds for every generated query.
- [ ] The milestone report is published, including baseline query latencies from [005](005-benchmark-harness-and-baselines.md).

## Testing and Validation

- Meta-test: an injected bug, such as wrong null handling in a filter or a dropped parallel edge in expand, is detected within the PR budget.

## References

- [Guide §13 M3](../architecture/implementation-guide.md#m3--graph-query-engine), [§12.2 C4](../architecture/implementation-guide.md#122-mandatory-correctness-properties)
