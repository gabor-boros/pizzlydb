# 203: Transaction and query service: interactive sessions, idempotency, and streaming results

| Field | Value |
| --- | --- |
| Phase | 3 Production capabilities |
| Priority | Must |
| Size | L |
| Crates | `pizzlydb-server`, `pizzlydb-txn` (idempotency records) |
| Type | Feature |

## Summary

Implement the transaction, graph-operation, and query RPCs. The service supports:

- **Interactive transactions:** a server-side session holds the `TxId`, with a time-to-live, an idle timeout, and a maximum number of sessions per principal. Each session is bound to its connection or principal.
- **Implicit single-operation transactions** for convenience.
- **Idempotency keys:** a client-supplied key is stored transactionally with the commit result and retained for a bounded window, so that retrying an ambiguous commit is safe.
- **Retry semantics** documented for each error.
- **Query and explain** with streaming pages and continuation tokens ([058](058-ordering-and-pagination.md)).

## Motivation

Guide §8.1 and §8.2 require transaction ids, idempotency keys for safe retry, and documented retry semantics. They also say that an SDK must never automatically retry a mutation that might not be idempotent unless it has an idempotency key. Interactive transactions are viable only because the optimistic design ([033](033-optimistic-transaction-manager.md)) holds no locks between requests.

## Current State

- Not implemented. The engine API comes from [071](071-embedded-engine-composition.md).

## Scope

### In scope

- **Session table.** Bounded, with expiry. An expired session returns `TX_EXPIRED`. Sessions are cleaned up on disconnect, and the configuration decides whether disconnect aborts the transaction.
- **Idempotency.** The record `sys/idem/<ns>/<principal>/<key>` holds the `CommitSeq` and a result digest. It is written in the same commit as the transaction and garbage-collected after its retention window. A replay with the same key returns the original result, and a replay with the same key but a different payload digest returns `INVALID_ARGUMENT`.
- **Graph operation RPCs.** Each one maps to the engine API, with namespace scoping.
- **Query RPC.** Results are streamed with backpressure, pagination tokens are provided, and `explain` and `explain analyze` are available, the latter only to authorized principals ([207](207-authorization-rbac-and-policy-epochs.md)).
- **Retry matrix** documentation in `docs/specifications/protocol.md`.

### Out of scope

- Subscriptions ([204](204-subscription-delivery-service.md)) and authorization enforcement ([207](207-authorization-rbac-and-policy-epochs.md)).

## Dependencies

- [202](202-server-runtime.md): the server runtime.
- [033](033-optimistic-transaction-manager.md): transactions.
- [058](058-ordering-and-pagination.md): pagination.

## Acceptance Criteria

- [ ] A commit retried with the same idempotency key after a simulated lost response returns the same `CommitSeq` and applies its effects only once (test).
- [ ] Session limits and expiry are enforced, with typed errors.
- [ ] The query stream applies backpressure to the server: a slow reader does not grow server memory beyond the configured budget (test).
- [ ] Every RPC has an integration test that covers both success and its documented errors.

## Testing and Validation

- Integration tests with fault injection that drops responses and connections.

## References

- [Guide §8.1, §8.2, §8.3](../architecture/implementation-guide.md#8-api-protocol-and-sdks)
