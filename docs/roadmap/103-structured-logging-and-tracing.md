# 103: Structured logging and OpenTelemetry-compatible tracing

| Field | Value |
| --- | --- |
| Phase | 2 Operational layer |
| Priority | Must |
| Size | M |
| Crates | all engine crates, `pizzlydb-engine` |
| Type | Feature, operability |

## Summary

Adopt `tracing` for structured logs and spans across the transaction, commit, query, maintenance, and delivery paths, with OpenTelemetry context propagation. No property payloads, parameter values, or credentials appear in logs or traces by default. The issue also adds a slow-query and slow-commit log with configurable thresholds.

## Motivation

Guide §10.3 requires OpenTelemetry-compatible trace context across the API, transaction, maintenance, and delivery boundaries, and it forbids graph-property payloads in traces by default. Incident diagnosis needs correlated, structured events such as checkpoint started and finished, degraded mode entered, plan invalidated, and subscription expired.

## Current State

- No logging or tracing dependency exists in any crate.

## Scope

### In scope

- `tracing` spans for:
  - `txn.begin`, `txn.commit` (with child spans for validate, write, and sync)
  - `query.parse`, `query.bind`, `query.plan`, `query.execute`
  - `maintenance.process_batch`, `subscription.deliver`, `checkpoint.step`, `backup`, and `restore`
- A catalogue of structured events with stable names and fields, documented in `docs/operations/logging.md`.
- Redaction rules and a lint or test that catches `Value` or parameter values in log fields. A typed wrapper with a redacting `Debug` implementation is one way to enforce it.
- An OpenTelemetry exporter (OTLP), configurable with a sampling rate, plus context propagation hooks for the protocol layer ([202](202-server-runtime.md)).
- Slow-query and slow-commit logs that record the query fingerprint and plan summary but no parameter values.
- JSON and human-readable output formats, and log levels per module.

### Out of scope

- The audit log, which is a separate channel with separate guarantees ([209](209-audit-logging.md)).

## Dependencies

- [016](016-thread-safe-storage-engine.md): thread-safe engine.
- [071](071-embedded-engine-composition.md): the composition root.

## Acceptance Criteria

- [ ] A single commit produces a connected trace with validate, write, and sync spans in an OTLP test collector (integration test).
- [ ] A redaction test runs a workload with sentinel property values and finds no sentinel in any log or span output.
- [ ] Tracing overhead with sampling at 0% is under 1%, and at 100% the overhead is documented (benchmark).

## Testing and Validation

- Integration tests with an in-memory OTLP collector, and the redaction test.

## References

- [Guide §10.3](../architecture/implementation-guide.md#103-telemetry)
- [`tracing`](https://docs.rs/tracing), [OpenTelemetry Rust](https://opentelemetry.io/docs/languages/rust/)
