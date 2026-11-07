# 102: Metrics foundation, reference dashboards, and alert rules

| Field | Value |
| --- | --- |
| Phase | 2 Operational layer |
| Priority | Must (critical path) |
| Size | L |
| Crates | all engine crates (instrumentation), `pizzlydb-engine`, `deploy/observability/` |
| Type | Feature, operability |

## Summary

Instrument the engine with Prometheus-compatible metrics that cover everything guide §10.3 lists. Define naming and cardinality rules. Expose a scrape endpoint through the composition root; it becomes network-visible in [202](202-server-runtime.md). Ship reference Grafana dashboards and Prometheus alert rules.

## Motivation

A database that cannot be observed cannot be operated. Phase 1 issues expose internal stats structs (`PagerStats`, transaction counters, lag), and nothing exports them. Guide §10.3 lists required metrics for commits and conflicts, storage and amplification, queries, maintenance lag, subscriptions, quotas and auth, backup and restore, and corruption.

## Current State

- `crates/pizzlydb-storage/src/pager.rs` has `PagerStats` (hits, misses, evictions).
- No metrics crate dependency exists, and there is no exporter.

## Scope

### In scope

- **Library and conventions.** A metrics facade (`metrics` with a Prometheus exporter, or `prometheus-client`), chosen in a short ADR. Names follow the `pizzlydb_<subsystem>_<name>_<unit>` convention. Labels may include namespace but never entity ids or user values, and a documented cardinality budget applies. Histogram buckets are chosen for each latency class.
- **Metric set:**
  - Storage: pager hit ratio, evictions, WAL bytes and frames, checkpoint duration and backlog, fsync latency, database file size, free pages, retained WAL bytes due to snapshots or floors, write amplification (WAL bytes per logical byte), and degraded state.
  - Transactions: commits, aborts by reason, commit latency by durability mode, validation time, group-commit size, active transactions, and oldest snapshot age.
  - Queries: planning and execution latency, rows scanned, edges visited, rejected plans by reason, and plan cache hit ratio.
  - Incremental: per-plan lag (commits and seconds), processing time, delta amplification, arrangement bytes, and checkpoint duration.
  - Subscriptions: count by state, backlog bytes and age, ack latency, replay rate, expired cursors, and disconnects by reason.
  - Operations: backup and restore success and age, last successful restore drill, scrub progress, and corruption detected.
- **Exposure.** The scrape endpoint and `Database::metrics()` for embedded use.
- **Reference assets.** Grafana dashboards (JSON) and Prometheus alert rules in `deploy/observability/`, with runbook links. The alerts cover:
  - commit failures, degraded mode, and disk usage
  - lag SLO breach and retention growth
  - backup age and corruption detected
  - expired cursors spike

### Out of scope

- Tenant quota metrics ([208](208-multi-tenancy-and-quotas.md)) and auth metrics ([206](206-authentication.md)). They reuse this foundation.

## Dependencies

- [016](016-thread-safe-storage-engine.md): concurrent counters.
- [071](071-embedded-engine-composition.md): the composition root.

## Acceptance Criteria

- [ ] Every metric category in guide §10.3 that belongs to the engine has at least one metric, documented in `docs/operations/metrics.md`.
- [ ] Metric overhead is under 2% throughput on the commit benchmark.
- [ ] A cardinality test with 1,000 namespaces stays within the documented series budget.
- [ ] Dashboards and alerts load in Grafana and Prometheus in a CI smoke test (`promtool check rules`, dashboard JSON lint).

## Testing and Validation

- Unit tests asserting that the metrics change under known operations, plus a benchmark of the overhead.

## References

- [Guide §10.3 telemetry](../architecture/implementation-guide.md#103-telemetry)
- [Prometheus naming best practices](https://prometheus.io/docs/practices/naming/)
