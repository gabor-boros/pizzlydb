# 067: Incremental correctness suite (C5–C7 at engine level)

| Field | Value |
| --- | --- |
| Phase | 1E Reactive core |
| Priority | Must (milestone gate, part 1; critical path) |
| Size | L |
| Crates | `pizzlydb-incremental`, `pizzlydb-testkit`, `tests/model-based/` |
| Type | Verification |

## Summary

This permanent suite proves that incremental maintenance matches recomputation from scratch after every commit, under concurrent writes, crashes, plan installation during load, schema revisions during replay, and every graph-specific trap. It uses the reference evaluator ([053](053-reference-query-evaluator.md)) as the oracle.

## Motivation

The central contract of the product is guide §6.1: `apply(R(Q,S), ΔR) == evaluate_from_scratch(Q, graph_at(S+1))`. The guide lists it as mandatory property-based and differential testing. It is invariant C5 (delta correctness). The same suite covers C6 (bootstrap correctness) at the level of plan installation, and the engine-side half of C7 (replay correctness): no double-apply, and explicit errors for retention gaps or incompatible plans. Each feature issue covers part of the contract. This gate checks all of it together.

## Current State

- Not started. The prerequisites are issues 060–066.

## Scope

### In scope

- Generated commit histories (from [003](003-testkit-reference-model-and-property-testing.md)), biased toward the traps in guide §6.5:
  - property flips
  - parallel edges, self-loops, and cycles
  - alternate paths
  - hub fan-out
  - group counts crossing zero
  - delete and recreate
- For each admissible corpus query and each randomly generated admissible query, an oracle check after every commit.
- Crash-and-replay runs ([004](004-crash-consistency-simulation-vfs.md)) that compare state and output logs against an uninterrupted run.
- Plan installation in the middle of a history, and schema changes in the middle of replay (revision semantics from [065](065-plan-installation-and-lifecycle.md)).
- Multiple concurrent plans with sharing.
- A failure minimizer that shrinks the history and the query.

### Out of scope

- Subscriber delivery semantics R1 and R2 ([069](069-subscription-bootstrap-and-event-stream.md), [071](071-embedded-engine-composition.md)).

## Dependencies

- [066](066-maintenance-scheduler-and-lag.md): the full maintenance pipeline.
- [053](053-reference-query-evaluator.md): the oracle.
- [004](004-crash-consistency-simulation-vfs.md): crash simulation.

## Acceptance Criteria

- [ ] Each PR runs 5,000 histories across every admissible corpus query, and the nightly job runs 500,000. Both find zero divergences, and the nightly job has stayed clean for 14 nights.
- [ ] Crash and replay equivalence holds for every crash point within the PR budget.
- [ ] Meta-tests: an injected bug in distinct reference counting, or a dropped retraction, is detected.

## Testing and Validation

- This issue is the test.

## References

- [Guide §6.1, §6.5](../architecture/implementation-guide.md#61-core-contract), [§12.2 C5–C7](../architecture/implementation-guide.md#122-mandatory-correctness-properties), [§13 M4](../architecture/implementation-guide.md#m4--incremental-operator-kernel)
