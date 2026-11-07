# 202: Server runtime: async front end, blocking execution pools, request lifecycle, and health endpoints

| Field | Value |
| --- | --- |
| Phase | 3 Production capabilities |
| Priority | Must (critical path) |
| Size | L |
| Crates | `pizzlydb-server` |
| Type | Feature |

## Summary

Build the server process. It has these parts:

- A tokio-based gRPC front end built with tonic.
- Dedicated blocking thread pools for storage, transaction, and query work. The guide forbids blocking storage calls on async executor threads.
- A request lifecycle that covers deadlines and cancellation propagation, admission control ([117](117-resource-governance-and-memory-budgets.md)), and per-connection limits.
- Graceful shutdown wired to the engine lifecycle ([104](104-engine-lifecycle-and-health.md)).
- Liveness and readiness through the gRPC health service and HTTP `/healthz` and `/readyz`.
- A metrics endpoint (`/metrics`) and trace-context extraction ([103](103-structured-logging-and-tracing.md)).

## Motivation

Guide §2.1 separates the network, transaction, maintenance, and delivery planes. Guide §11.2 forbids blocking storage calls on async threads, and guide §10.1 requires probes and graceful shutdown. This issue turns the embedded engine into a service.

## Current State

- `crates/pizzlydb-server/src/{lib,runtime}.rs` contain doc comments only.
- No binary target exists. The CLI ([213](213-cli.md)) is expected to host `serve`.

## Scope

### In scope

- `Server::start(config, engine)` with listeners (TCP, plus Unix socket for local administration), the tonic service registry, and request middleware: deadlines, cancellation, tracing, metrics, authentication (a no-op until [206](206-authentication.md)), and a size limit.
- **Execution bridging.** Engine calls run through `spawn_blocking` or dedicated pools, with bounded queues. Cancelling a request cancels the engine work through the cancellation token ([055](055-physical-operators-and-executor.md)).
- **Connection management.** Limits per client, keep-alives, and HTTP/2 flow-control settings.
- **Shutdown.** `SIGTERM` stops accepting work, drains within a deadline, and then calls engine shutdown. `SIGINT` behaves the same, and a second signal forces exit.
- **Probes and metrics.** Health endpoints map the engine health model to the probes, and the metrics endpoint is served from the same process.

### Out of scope

- The service handlers themselves ([203](203-transaction-and-query-service.md), [204](204-subscription-delivery-service.md), [211](211-admin-api.md)), and TLS ([205](205-tls-transport-security.md)).

## Dependencies

- [201](201-native-protocol-definition.md): the protocol.
- [104](104-engine-lifecycle-and-health.md): lifecycle and health.
- [102](102-metrics-foundation.md) and [103](103-structured-logging-and-tracing.md): observability.
- [117](117-resource-governance-and-memory-budgets.md): admission control.

## Acceptance Criteria

- [ ] No engine call executes on a tokio worker thread. A debug-mode assertion or lint enforces this, together with a test.
- [ ] Client cancellation stops the corresponding engine work within one batch (integration test).
- [ ] SIGTERM under load drains and exits within the configured deadline, and no acknowledged commit is lost (test).
- [ ] Probes reflect the engine state, including degraded and draining (tests).

## Testing and Validation

- Integration tests with an in-process client, plus shutdown and cancellation tests.

## References

- [Guide §2.1](../architecture/implementation-guide.md#21-execution-planes), [§10.1](../architecture/implementation-guide.md#101-deployment), [§11.2](../architecture/implementation-guide.md#112-coding-rules)
- `crates/pizzlydb-server/src/*.rs`
