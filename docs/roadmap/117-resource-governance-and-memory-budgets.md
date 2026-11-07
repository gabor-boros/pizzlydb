# 117: Resource governance: memory budgets, admission control, and limits

| Field | Value |
| --- | --- |
| Phase | 2 Operational layer |
| Priority | Must (critical path) |
| Size | L |
| Crates | `pizzlydb-engine`, every engine crate (accounting hooks) |
| Type | Feature, robustness |

## Summary

Introduce global resource governance. A memory manager assigns separate, configurable budgets to each subsystem:

- the buffer pool and the WAL index
- transaction write sets and read sets
- query working memory
- incremental arrangements
- subscription queues and output buffers
- maintenance jobs

Admission control limits the number of concurrent transactions, queries, subscriptions, and plan installations. Exceeding a budget produces controlled `RESOURCE_EXHAUSTED` errors or throttling, never a process-wide out-of-memory kill. The issue also covers file-descriptor and thread limits.

## Motivation

Guide §10.4 requires separate budgets for these resources and requires that "resource exhaustion MUST produce controlled errors or throttling instead of process-wide OOM wherever practical." Invariant C9 states that a slow query or subscriber cannot create unbounded growth in shared resources. Phase 1 adds individual limits; this issue makes them add up to a guaranteed process bound.

## Current State

- Individual limits exist after Phase 1: batch limits ([019](019-bounded-write-batches.md)), query memory ([055](055-physical-operators-and-executor.md)), arrangement limits ([066](066-maintenance-scheduler-and-lag.md)), and subscription queues ([069](069-subscription-bootstrap-and-event-stream.md)).
- No global accounting exists.

## Scope

### In scope

- A `MemoryBudget` hierarchy: a process total that is divided into subsystem budgets, with optional per-namespace sub-budgets (a hook for [208](208-multi-tenancy-and-quotas.md)). Allocation-heavy paths reserve memory against it, at a granularity coarse enough to be cheap.
- Admission controllers with queues and timeouts for each work class.
- Startup validation that the sum of budgets and the fixed overheads fit the configured process limit, with a warning about the cgroup limit when running in a container.
- Metrics for usage per budget, rejections, and queue waits.
- Documentation of a sizing formula and examples.

### Out of scope

- Per-tenant quota policy ([208](208-multi-tenancy-and-quotas.md)).

## Dependencies

- Subsystem limits: [019](019-bounded-write-batches.md), [055](055-physical-operators-and-executor.md), [066](066-maintenance-scheduler-and-lag.md), [069](069-subscription-bootstrap-and-event-stream.md).
- [102](102-metrics-foundation.md): metrics.

## Acceptance Criteria

- [ ] An adversarial workload test runs huge queries, slow subscribers, large transactions, and plan-install storms concurrently. RSS stays below the configured process limit plus a documented margin, and every overload surfaces as a typed error or throttling.
- [ ] Peak RSS in benchmarks matches the sizing formula within 20%.
- [ ] No unaccounted allocation path larger than 1 MiB exists on any request-driven code path (audit, plus allocation profiling in a benchmark).

## Testing and Validation

- Adversarial load tests, benchmarks with RSS tracking, and a soak test.

## References

- [Guide §10.4](../architecture/implementation-guide.md#104-resource-control), [§12.2 C9](../architecture/implementation-guide.md#122-mandatory-correctness-properties)
