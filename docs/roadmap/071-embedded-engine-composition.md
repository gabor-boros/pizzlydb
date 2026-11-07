# 071: Embedded engine composition, startup recovery, the unified error model, and the end-to-end scenario

| Field | Value |
| --- | --- |
| Phase | 1E Reactive core |
| Priority | Must (critical path) |
| Size | L |
| Crates | new `pizzlydb-engine` (proposed) or `pizzlydb-server::engine`, plus every engine crate |
| Type | Feature, architecture |

## Summary

Compose storage, transactions, graph, query, incremental maintenance, and subscriptions into a single in-process engine API, `Database`. It owns:

- **Lifecycle:** open, recover, and close.
- **Startup recovery,** in this order:
  1. verify the metadata
  2. recover the storage engine
  3. establish the commit frontier
  4. load the namespace registry
  5. recover the plans
  6. replay the incremental state
  7. resume the subscriptions at their frontiers
- **Errors:** a unified error type mapped to the canonical codes of guide §8.3, each with its retryability.

The guide's §16.2 end-to-end scenario becomes a permanent black-box test of this API, including a simulated crash and restart.

## Motivation

Every crate below this one is tested in isolation. A composition root makes it possible to:

- test the whole engine without a network
- give offline tools and the CLI a single entry point
- define the startup recovery order exactly once, as guide §4.5 requires

The guide's crate list makes `pizzlydb-server` the composition root. Mixing the engine with networking, though, would force every test and offline tool to depend on the async runtime and the TLS stack. The recommendation is a small `pizzlydb-engine` crate with no network dependencies, and the ADR in this issue records the decision. The alternative is a feature-gated module in the server crate.

## Current State

- `crates/pizzlydb-server/src/runtime.rs` is a one-line stub that depends on every crate.
- No unified error type exists. Each crate has its own error enum.

## Scope

### In scope

- **Crate decision** (an ADR addendum to ADR-001), followed by the crate or module skeleton.
- **`Database` API.** It covers:
  - `open(config)`
  - namespace management
  - `begin` / `commit` transactions with graph operations
  - `query` and `explain`
  - `subscribe`, `ack`, and `resume`
  - `wait_until_maintained`
  - `checkpoint`
  - `close`

  The API is thread-safe.
- **Startup recovery orchestration,** with a progress report and a time bound per phase.
- **Unified error type.** `EngineError` with a `code()` (`INVALID_ARGUMENT`, `NOT_FOUND`, `CONSTRAINT_VIOLATION`, `TX_CONFLICT`, `TX_EXPIRED`, `QUERY_UNSUPPORTED`, `QUERY_TOO_EXPENSIVE`, `RESOURCE_EXHAUSTED`, `CURSOR_EXPIRED`, `PLAN_INVALIDATED`, `RESUME_FORBIDDEN`, `UNAVAILABLE`, `INTERNAL`) and `retryable()`. The mapping from every crate error is total, and a test proves it.
- **End-to-end test.** The §16.2 scenario as `tests/e2e/scenario_16_2.rs`, in process, with a crash and restart through the crash simulator.

### Out of scope

- Networking ([202](202-server-runtime.md)) and configuration files ([101](101-configuration-system.md)); this issue uses an in-code config struct.

## Dependencies

- Reactive core: [069](069-subscription-bootstrap-and-event-stream.md), [070](070-resume-tokens.md), [067](067-incremental-correctness-suite.md).
- Query gate: [059](059-query-correctness-suite.md).
- Engine services: [045](045-namespace-registry-and-lifecycle.md), [037](037-snapshot-retention-and-transaction-limits.md).

## Acceptance Criteria

- [ ] The §16.2 scenario passes in process, including the crash and restart and the duplicate-tolerant resume.
- [ ] Every crate error variant maps to exactly one canonical code (exhaustiveness test).
- [ ] Startup recovery from a crash in the middle of a mixed workload restores every invariant (C1–C7, R1, R2) at the engine level (crash-simulation test).
- [ ] The engine crate has no dependency on tokio, tonic, or rustls (`cargo tree` check in CI).

## Testing and Validation

- The end-to-end scenario, crash-simulation recovery tests, and error-mapping tests.

## References

- [Guide §16.2 end-to-end scenario](../architecture/implementation-guide.md#162-initial-end-to-end-test-scenario), [§4.5](../architecture/implementation-guide.md#45-retention-and-recovery), [§8.3](../architecture/implementation-guide.md#83-error-taxonomy), [§11](../architecture/implementation-guide.md#11-rust-implementation-structure)
- `crates/pizzlydb-server/src/runtime.rs`
