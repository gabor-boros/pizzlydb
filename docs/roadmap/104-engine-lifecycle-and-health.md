# 104: Engine lifecycle, graceful shutdown, poison recovery, and the health model

| Field | Value |
| --- | --- |
| Phase | 2 Operational layer |
| Priority | Must |
| Size | M |
| Crates | `pizzlydb-engine` |
| Type | Feature, operability |

## Summary

Define and implement the engine's lifecycle:

- **Startup** with recovery progress reporting and time bounds.
- **Graceful shutdown:** stop intake, drain or abort transactions with a typed error, finish in-flight maintenance batches, flush cursors, checkpoint, and release the lock.
- **Poisoned-handle handling:** after a sync failure or disk-full, the engine either reopens itself or stays down, according to documented policy.
- **A health model** with the states `Starting`, `Ready`, `Degraded(reason)`, `Draining`, and `Failed(reason)`, mapped later to liveness and readiness probes ([202](202-server-runtime.md), [216](216-packaging-and-deployment.md)).

## Motivation

Guide §10.1 requires readiness and liveness probes and graceful shutdown. Storage poisons its handle on sync failure ([022](022-disk-full-and-io-error-safety.md)), so something must decide whether to reopen, and must tell operators and orchestrators what is happening. Shutdown without a checkpoint makes the next startup slow.

## Current State

- `Storage::close()` exists, and failed commits set `closed = true`.
- Phase 1 defines no lifecycle beyond `open` and `close` ([071](071-embedded-engine-composition.md) adds startup recovery orchestration).

## Scope

### In scope

- A lifecycle state machine with transitions published to metrics and logs.
- Startup phases with progress reports: storage recovery (WAL bytes replayed), namespace load, plan recovery, and subscription resume. Each phase has a configured time budget and a warning when it is exceeded.
- **Shutdown.** `shutdown(deadline)` runs the documented order, and any work left at the deadline is aborted with `UNAVAILABLE`. Shutdown checkpoints so that the next startup replays a near-empty WAL.
- **Poison policy.** A sync failure moves the engine to `Failed`, and the default is no automatic reopen, because data on disk may be inconsistent with what the kernel reported (fsyncgate). Disk-full moves it to `Degraded(read_only)`, with an automatic reopen once space is restored, if configured.
- A `health()` API returning the state, reasons, and frontiers.

### Out of scope

- HTTP and gRPC probe endpoints ([202](202-server-runtime.md)).

## Dependencies

- [071](071-embedded-engine-composition.md): the composition root.
- [021](021-automatic-checkpointing-and-wal-bounds.md): checkpoint on shutdown.
- [022](022-disk-full-and-io-error-safety.md): the degraded modes.
- [101](101-configuration-system.md): the policies are configured.

## Acceptance Criteria

- [ ] After a graceful shutdown under load, restart replays at most one checkpoint step of WAL, and no acknowledged commit or ack is lost (integration test).
- [ ] An injected sync failure moves the engine to `Failed`, and an injected disk-full moves it to `Degraded`. `health()` reports both correctly (tests).
- [ ] Startup recovery reports its progress, and the time for each phase is measured in benchmarks.

## Testing and Validation

- Integration tests with fault injection, plus a shutdown-under-load test.

## References

- [Guide §10.1](../architecture/implementation-guide.md#101-deployment), [§4.5](../architecture/implementation-guide.md#45-retention-and-recovery)
