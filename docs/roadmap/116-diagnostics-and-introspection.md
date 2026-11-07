# 116: Diagnostics and introspection, `EXPLAIN ANALYZE`, and diagnostic bundles

| Field | Value |
| --- | --- |
| Phase | 2 Operational layer |
| Priority | Must |
| Size | M |
| Crates | `pizzlydb-storage` (`inspect`), `pizzlydb-txn`, `pizzlydb-query`, `pizzlydb-incremental`, `pizzlydb-engine` |
| Type | Feature, operability |

## Summary

Give operators and developers the introspection needed to diagnose problems without attaching a debugger:

- **Storage:** page counts by kind, B-tree depth and fill per key prefix (per namespace and kind), WAL statistics, and retained versions.
- **Transactions:** active transactions, the oldest snapshots, and conflict hot spots, meaning conflicts grouped by key prefix.
- **Queries:** `EXPLAIN ANALYZE`, which compares actual and estimated rows per operator, and a slow-query log with plan summaries.
- **Incremental plans:** state size, lag, delta amplification, and the most expensive input mutations.
- **Subscriptions:** backlog, ack latency, and state.
- **Diagnostic bundle:** a command that collects configuration (redacted), metrics snapshots, inspect output, and recent logs, for support.

## Motivation

Guide §8.2 lists `explain`, `health`, `metrics`, and `inspect_retention` as API surface. Without introspection, every performance incident means guessing. `inspect.rs` currently reports only the first damaged page.

## Current State

- `crates/pizzlydb-storage/src/inspect.rs`: a read-only header inspection that reports the first damaged page.
- Per-operator execution statistics come from [055](055-physical-operators-and-executor.md), and lag statistics from [066](066-maintenance-scheduler-and-lag.md).

## Scope

### In scope

- A `Database::inspect()` family of APIs returning structured data. All of them are safe on a live engine, read from a snapshot, and stay within a budget.
- **Storage inspection:** a per-prefix B-tree histogram produced by a sampled or full walk.
- **Conflict tracking:** a bounded top-k of key prefixes involved in aborts.
- **`EXPLAIN ANALYZE`:** it runs the query with instrumentation and returns the plan with actual and estimated values.
- **Diagnostic bundle:** a tar of JSON files with redaction that is verified by a sentinel test.

### Out of scope

- Admin API transport ([211](211-admin-api.md)) and CLI formatting ([213](213-cli.md)).

## Dependencies

- [102](102-metrics-foundation.md): metrics.
- [056](056-planner-cost-model-and-explain.md): `EXPLAIN`.
- [066](066-maintenance-scheduler-and-lag.md): lag.

## Acceptance Criteria

- [ ] Each inspect API returns correct values on a known dataset (tests).
- [ ] `EXPLAIN ANALYZE` actual row counts equal the true per-operator counts (test).
- [ ] The diagnostic bundle contains no property values, parameters, or secrets (sentinel test).
- [ ] Running inspect concurrently with load affects foreground p99 by less than a documented bound.

## Testing and Validation

- Unit and integration tests, plus the redaction test.

## References

- [Guide §8.2](../architecture/implementation-guide.md#82-initial-api-surface), [§10.3](../architecture/implementation-guide.md#103-telemetry)
- `crates/pizzlydb-storage/src/inspect.rs`
