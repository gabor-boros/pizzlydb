# 903: Incremental maintenance of bounded variable-length paths (RFC-gated)

| Field | Value |
| --- | --- |
| Phase | Future (post-GA expansion track) |
| Priority | Could |
| Size | XL |
| Crates | `pizzlydb-incremental`, `pizzlydb-query` |
| Type | RFC, feature |

## Summary

Write an RFC and then implement incremental maintenance for bounded variable-length path patterns (`-[:T*1..k]->`), with exact multiplicity under insertions and deletions. Alternate paths and cycles must be handled correctly. Delta amplification must be bounded, or the planner must reject the pattern because of its estimated cost.

## Motivation

Guide §6.5 calls out alternate paths and deletions as the main graph-specific correctness traps, and guide §13 lists bounded path-maintenance algorithms as an expansion track. Phase 1 supports bounded variable-length paths in snapshot queries ([057](057-bounded-variable-length-paths.md)) but rejects them for subscriptions ([062](062-incremental-admissibility-and-compiler.md)).

## Current State

- Snapshot-only support ([057](057-bounded-variable-length-paths.md)). The admissibility check rejects these patterns with a reason.

## Scope

### In scope

- **The RFC.** It covers the algorithm choice (for example, k-bounded join unrolling with shared arrangements, or counting-based maintenance), a worst-case amplification analysis, resource limits, and a test oracle.
- **Implementation and admissibility update.**

### Out of scope

- Unbounded reachability.

## Dependencies

- [067](067-incremental-correctness-suite.md): the incremental correctness suite to extend.
- [057](057-bounded-variable-length-paths.md): snapshot semantics.

## Acceptance Criteria

- [ ] The RFC is accepted.
- [ ] Incremental results equal recomputation after every commit for generated workloads that include cycles, parallel edges, and alternate paths (C5).
- [ ] The amplification bound is enforced, and patterns over the bound are rejected at installation.

## Testing and Validation

- Differential testing against the reference evaluator.

## References

- [Guide §6.5](../architecture/implementation-guide.md#65-graph-specific-correctness-traps), [§13](../architecture/implementation-guide.md#13-implementation-roadmap-dependency-ordered)
