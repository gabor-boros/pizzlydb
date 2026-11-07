# 066: Maintenance scheduler, lag tracking, `wait_until_maintained`, and backpressure

| Field | Value |
| --- | --- |
| Phase | 1E Reactive core |
| Priority | Must (critical path) |
| Size | L |
| Crates | `pizzlydb-incremental` |
| Type | Feature |

## Summary

Run plan maintenance on worker threads. Each plan consumes the log strictly in `CommitSeq` order, and the engine tracks and exposes these frontiers:

- `commit_seq`, for the database
- `maintained_seq`, per plan
- the lag between them, in commits and in time

The engine also provides `wait_until_maintained(plan, seq, timeout)` and isolates slow or expensive plans so that they cannot starve others. Its policies for plans that exceed their budgets are throttle, pause, or mark lagging, and they never involve silent truncation.

## Motivation

Guide §7.6 requires `commit_seq` and `maintained_seq` to be distinguishable in APIs and metrics. It also requires a `wait_until_maintained` primitive in place of any pretence that subscriptions update synchronously inside the committing transaction. Guide §7.5 and invariant C9 require bounded resources. Without a scheduler, one plan with a hub-node fan-out would block every other plan.

## Current State

- Not implemented. The engine becomes thread-safe in [016](016-thread-safe-storage-engine.md).

## Scope

### In scope

- **Worker pool.** A configurable size. Each plan processes its commits in order. Different plans can run in parallel, and shared plans are processed once.
- **Batching.** Consecutive commits are processed together for efficiency, while their commit boundaries are preserved.
- **Budgets.** Each processing step has a CPU-time and fan-out budget. A plan that exceeds it is marked `Lagging` with a reason. Repeated overruns follow policy: the plan is paused, or invalidated with `RESOURCE_EXHAUSTED`.
- **Frontier API.** `maintained_seq(plan)`, `lag(plan)`, and `wait_until_maintained(plan, seq, timeout) -> Result<(), Timeout | PlanFailed>`.
- **Statistics hooks** for metrics ([102](102-metrics-foundation.md)): lag, processing time per commit, delta sizes, and state size.
- **Isolation of failures.** A plan whose state is corrupted or fails verification is isolated, is not allowed to crash the process, and is offered a rebuild ([109](109-repair-procedures-and-salvage-hardening.md)).

### Out of scope

- Subscriber delivery and backpressure ([069](069-subscription-bootstrap-and-event-stream.md)).

## Dependencies

- [065](065-plan-installation-and-lifecycle.md): plan lifecycle.
- [016](016-thread-safe-storage-engine.md): threading.

## Acceptance Criteria

- [ ] `wait_until_maintained` returns only after the plan's durable output includes the given commit (test against output-log reads).
- [ ] A plan that hits a hub (1 million fan-out) does not delay the other plans' p99 lag by more than a documented bound (benchmark).
- [ ] Lag metrics are correct under load, checked against ground truth.
- [ ] Commit ordering per plan is never violated (property test with a parallel pool).

## Testing and Validation

- Concurrency stress tests, property tests, and benchmarks.

## References

- [Guide §7.5, §7.6](../architecture/implementation-guide.md#75-backpressure-and-flow-control), [§12.2 C9](../architecture/implementation-guide.md#122-mandatory-correctness-properties)
