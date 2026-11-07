# 115: Unified background maintenance scheduler

| Field | Value |
| --- | --- |
| Phase | 2 Operational layer |
| Priority | Must |
| Size | M |
| Crates | `pizzlydb-engine`, all crates that contribute jobs |
| Type | Feature, operability |

## Summary

Run every background job under one scheduler. The jobs come from many issues:

- checkpoint steps
- page reclaim
- compaction and file shrinking
- scrubbing
- statistics refresh
- commit-log and output-log truncation
- expiry of snapshots, subscriptions, and transactions
- chunked drops of namespaces, indexes, and plan state
- online index-build back-fill
- restore drills

The scheduler enforces explicit budgets (I/O bytes per second, a CPU share, concurrency), priorities, and operator controls (pause, resume, run now). Long jobs persist their progress so they can resume after a restart.

## Motivation

Without coordination, each subsystem starts its own threads and competes for I/O, which hurts foreground latency unpredictably. Guide §10.4 requires a separate budget for background maintenance. Operators need a single place to see and control background work.

## Current State

- Phase 1 adds a dedicated checkpointer thread ([021](021-automatic-checkpointing-and-wal-bounds.md)) and maintenance workers ([066](066-maintenance-scheduler-and-lag.md)). Other jobs exist only as functions.

## Scope

### In scope

- A `Job` trait with `step(budget) -> Progress`, cancellation, and an optional persistent cursor.
- **Scheduler:** priority classes (critical: checkpoint and expiry; normal: compaction and statistics; low: scrub), token-bucket I/O budgets, and adaptive throttling when foreground p99 rises, if this can be measured.
- **Controls** through the engine API, which the admin API exposes later ([211](211-admin-api.md)): list jobs, pause, resume, run now, and change a budget.
- **Metrics:** job backlog, progress, time spent, and throttling events.

### Out of scope

- Incremental plan maintenance workers. They have their own scheduler ([066](066-maintenance-scheduler-and-lag.md)) but report into the same budget accounting.

## Dependencies

- [021](021-automatic-checkpointing-and-wal-bounds.md): the checkpointer.
- [113](113-space-reclamation-and-compaction.md): compaction jobs.
- [107](107-full-verification-and-online-scrubbing.md): the scrubber.
- [114](114-retention-management-and-inspection.md): retention jobs.
- [066](066-maintenance-scheduler-and-lag.md): budget coordination.

## Acceptance Criteria

- [ ] With all background jobs active, the foreground p99 commit and query latency stays within a configured factor of the idle baseline (benchmark).
- [ ] Pausing a job stops its I/O within one step, and resuming continues from the persisted progress after a restart (tests).
- [ ] Every background thread in the engine is owned by the scheduler or the maintenance workers (audit).

## Testing and Validation

- Benchmarks, integration tests, and a soak test.

## References

- [Guide §10.4](../architecture/implementation-guide.md#104-resource-control)
