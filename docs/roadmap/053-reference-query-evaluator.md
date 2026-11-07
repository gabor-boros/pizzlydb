# 053: Reference query evaluator, used as the oracle

| Field | Value |
| --- | --- |
| Phase | 1D Query engine |
| Priority | Must |
| Size | M |
| Crates | `pizzlydb-testkit` (reference evaluator) |
| Type | Test infrastructure |

## Summary

Implement a deliberately naive evaluator. It executes the typed IR directly over the testkit's `ReferenceGraph` by enumerating every candidate binding. It is the oracle for snapshot query correctness ([059](059-query-correctness-suite.md)) and for incremental maintenance, via the equation `apply(R(S), ΔR) == evaluate_from_scratch(graph_at(S+1))` ([067](067-incremental-correctness-suite.md)).

## Motivation

Guide WP-11 asks for a reference evaluator, and the incremental contract in §6.1 is defined relative to evaluation from scratch. An oracle that is independent of the optimized engine is what makes differential testing meaningful. Its implementation must be simple enough to audit, with no indexes, no cost model, and no shared code with the executor beyond the IR and the value semantics.

## Current State

- `crates/pizzlydb-testkit/src/reference.rs` is a stub.
- The reference graph model comes from [003](003-testkit-reference-model-and-property-testing.md).

## Scope

### In scope

- Evaluation of every IR construct supported in v1: pattern enumeration with the correct path-mode semantics, three-valued filters, projections, `DISTINCT`, `COUNT`, `SUM`, and `ORDER BY`, `SKIP`, and `LIMIT`. Ties are made deterministic with a documented total order that is used only for comparison.
- Multiset result comparison utilities, with an order-sensitive mode when the query has `ORDER BY`.
- Execution of the conformance corpus from [050](050-query-language-specification.md). Every case must pass, which validates the corpus itself.

### Out of scope

- Performance. This evaluator is for testing only.

## Dependencies

- [052](052-binder-type-checker-and-ir.md): the IR.
- [003](003-testkit-reference-model-and-property-testing.md): the reference graph.

## Acceptance Criteria

- [ ] 100% of the conformance corpus passes on the reference evaluator.
- [ ] The code is under about 1,500 lines, has no dependency on `pizzlydb-query` internals beyond the IR and the value semantics, and contains no optimization (review checklist).
- [ ] Rustdoc explains the enumeration semantics of each path mode.

## Testing and Validation

- The conformance corpus, plus hand-checked small graphs for each path mode.

## References

- [Guide §6.1](../architecture/implementation-guide.md#61-core-contract), [§16 WP-11](../architecture/implementation-guide.md#16-first-implementation-slice--concrete-work-packages)
