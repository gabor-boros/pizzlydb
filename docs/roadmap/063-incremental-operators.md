# 063: Incremental operator library: signed deltas, joins, distinct, and aggregates

| Field | Value |
| --- | --- |
| Phase | 1E Reactive core |
| Priority | Must (critical path) |
| Size | XL |
| Crates | `pizzlydb-incremental` (`delta`, `arrangement`) |
| Type | Feature |

## Summary

Implement the incremental operators from guide §6.2 over signed multiset deltas (`(tuple, i64 multiplicity)`):

| Operator | Notes |
| --- | --- |
| Filter and projection | |
| Fixed-pattern equality join | Uses arrangements indexed on the join keys |
| Bounded neighbor expansion | Delta-driven adjacency probes with fan-out budgeting |
| Distinct | Reference counting |
| `COUNT` and `SUM` | Per-group state that supports retraction |
| Consolidation | |

Mutation-log records are converted into input deltas: a property change becomes the retraction of the old value plus the insertion of the new one.

## Motivation

This is the algorithmic core of the product. Every graph-specific trap listed in guide §6.5 must be handled explicitly:

- parallel edges, self-loops, and cycles
- multiple paths that produce the same tuple
- alternate paths that keep a result alive after one path disappears
- group counts moving between zero and non-zero
- high-degree nodes that produce large deltas

## Current State

- `crates/pizzlydb-incremental/src/{delta,arrangement}.rs` are one-line stubs.

## Scope

### In scope

- Delta types and consolidation. Multiplicities are `i64`, with checked overflow that fails into a defined error state.
- An input adapter from `GraphMutation` to the base-relation deltas the plan uses: nodes by label, edges by type, and property values. Before-images are used where ADR-007 requires them.
- Arrangement storage behind a trait, with an in-memory implementation for tests and a storage-backed one in [064](064-maintained-state-and-output-log.md). Arrangement memory is accounted for.
- Each operator implementation, with batch processing that keeps commit boundaries intact.
- Fan-out budgeting: a single input mutation that would produce more than the budget allows is handled under policy, which [066](066-maintenance-scheduler-and-lag.md) defines. The plan may be paused and marked lagging, but the result is never silently truncated.

### Out of scope

- Durable state, checkpoints ([064](064-maintained-state-and-output-log.md)), and scheduling ([066](066-maintenance-scheduler-and-lag.md)).

## Dependencies

- [062](062-incremental-admissibility-and-compiler.md): the compiler.

## Acceptance Criteria

- [ ] A unit test for each trap in guide §6.5, with the expected deltas written by hand.
- [ ] The oracle property `apply(R(S), ΔR) == eval(graph_at(S+1))` holds over 10,000 random commit histories for each admissible conformance query (in-memory arrangements). This is the precursor to [067](067-incremental-correctness-suite.md).
- [ ] Per-mutation cost for a one-edge filter query is O(1) in the graph size (benchmark from 10K to 10M edges).

## Testing and Validation

- Property tests against the reference evaluator ([053](053-reference-query-evaluator.md)), plus benchmarks.

## References

- [Guide §6.1–§6.3, §6.5](../architecture/implementation-guide.md#6-incremental-query-maintenance)
- `crates/pizzlydb-incremental/src/{delta,arrangement}.rs`
