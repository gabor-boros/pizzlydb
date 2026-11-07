# 062: Reactive admissibility checker and incremental plan compiler

| Field | Value |
| --- | --- |
| Phase | 1E Reactive core |
| Priority | Must (critical path) |
| Size | L |
| Crates | `pizzlydb-incremental`, `pizzlydb-query` |
| Type | Feature |

## Summary

This issue has three parts:

1. **Admissibility check.** Decide whether a bound `SUBSCRIBE` query falls within the admissible incremental subset defined in [050](050-query-language-specification.md). An inadmissible query is rejected with precise, machine-readable reasons (`QUERY_UNSUPPORTED`).
2. **Compilation.** Compile an admissible query into a typed incremental operator graph, following the decision in ADR-007 ([060](060-adr-incremental-execution-engine.md)).
3. **Resource bounds.** Compute and enforce bounds on state size per input, fan-out per delta, and output amplification.

## Motivation

The guide forbids conforming implementations from recomputing the whole query per change (§6.2), and it requires plans to be rejected when their resource bounds cannot be verified (§5.4). The compiler is where these rules are enforced. The output plan must be serializable and versioned, because it persists ([064](064-maintained-state-and-output-log.md)) and is reloaded on restart.

## Current State

- Not implemented. The IR and the snapshot planner come from [052](052-binder-type-checker-and-ir.md) and [056](056-planner-cost-model-and-explain.md).

## Scope

### In scope

- Admissibility rules:
  - Fixed-length patterns, filters, projections, `DISTINCT`, and `COUNT`/`SUM` are admissible.
  - Bounded variable-length segments are admissible only if [903](903-incremental-variable-length-paths.md) is done. Until then they are rejected with a reason.
  - `ORDER BY` and `LIMIT` inside a subscription follow the RFC. The recommendation is to reject them or treat them as presentation only.
- Operator graph construction. It chooses arrangements (join keys, adjacency direction) and a join order that minimizes the expected delta work using the statistics from [046](046-graph-statistics.md).
- Resource-bound estimation, with policy limits on arrangement size, fan-out per input mutation, and output rows per commit.
- A plan fingerprint (IR fingerprint plus compiler version) and an explicit *plan revision* number. A versioned plan serialization.
- `EXPLAIN SUBSCRIBE` output.

### Out of scope

- Operator implementations ([063](063-incremental-operators.md)).

## Dependencies

- [060](060-adr-incremental-execution-engine.md): ADR-007.
- [056](056-planner-cost-model-and-explain.md): cost and statistics infrastructure.

## Acceptance Criteria

- [ ] Every admissible query in the conformance corpus compiles, and every inadmissible one is rejected with its documented reason code (corpus tests).
- [ ] The plan serialization round-trips, and a fuzz target exists for the decoder.
- [ ] `EXPLAIN SUBSCRIBE` shows the arrangements, the join order, and the estimated bounds (snapshot tests).

## Testing and Validation

- Corpus tests and property tests: compiling the same IR always produces the same plan and fingerprint.

## References

- [Guide §6.2](../architecture/implementation-guide.md#62-supported-first-class-operators), [§5.4](../architecture/implementation-guide.md#54-cost-based-safeguards)
