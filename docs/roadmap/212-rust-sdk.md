# 212: Rust SDK

| Field | Value |
| --- | --- |
| Phase | 3 Production capabilities |
| Priority | Must (near-critical path) |
| Size | L |
| Crates | `pizzlydb-sdk` |
| Type | Feature |

## Summary

Build the official async Rust client. It provides:

- Connection management with TLS ([205](205-tls-transport-security.md)), authentication ([206](206-authentication.md)), pooling, keepalive, and deadlines.
- Typed transaction and query APIs with streaming results.
- A safe retry policy: an operation is retried automatically only when it is idempotent or carries an idempotency key.
- A subscription consumer that handles:
  - snapshot and delta application helpers
  - automatic resume with the last resume token
  - acknowledgement batching
  - typed handling of `SLOW_CONSUMER`, `PLAN_INVALIDATED`, `RESUME_FORBIDDEN`, and `CURSOR_EXPIRED`
- Observability hooks through tracing spans and metrics.

## Motivation

Guide §8.4 requires SDKs to preserve commit sequences, frontiers, and resume tokens, and to make safe retries the default. It also says that an SDK must never automatically retry a mutation that might not be idempotent unless it has an idempotency key. The SDK is how most users will experience the at-least-once contract, so it must make correct consumption the easy path.

## Current State

- `crates/pizzlydb-sdk/src/{lib,client}.rs` contain doc comments only.

## Scope

### In scope

- **`Client`, `Transaction`, `Query`, and `Subscription` types.** Public APIs are documented with examples and published with semver discipline.
- **Retry policy.** Retryable codes and backoff with jitter. Mutations are never retried automatically without an idempotency key, which the SDK can generate for the caller.
- **Subscription consumer.** It tracks the event identity to deduplicate replays on the client side, persists the resume token through a user callback, auto-resumes on disconnect with backoff, and exposes the frontiers.
- **`wait_until_maintained` helper.**
- **Example programs.** They cover the guide §16.2 scenario (the end-to-end test scenario) and a "read-your-writes subscription" example.

### Out of scope

- Other languages ([902](902-additional-sdks.md)).

## Dependencies

- [203](203-transaction-and-query-service.md): transaction and query RPCs.
- [204](204-subscription-delivery-service.md): subscription RPCs.
- [206](206-authentication.md): authentication.

## Acceptance Criteria

- [ ] The guide §16.2 end-to-end scenario passes through the SDK against a real server, including a restart in the middle of the scenario.
- [ ] Under injected network faults, the consumer's applied state equals the oracle at its acknowledged frontier (R1 and R2), with no duplicates applied after deduplication (property test).
- [ ] No non-idempotent mutation is retried automatically (test with an RPC-level spy).
- [ ] Public API documentation coverage is 100%, and the examples compile in CI.

## Testing and Validation

- Integration tests with a server fixture and fault-injection proxy tests.

## References

- [Guide §8.4](../architecture/implementation-guide.md#84-sdk-contract), [§16.2](../architecture/implementation-guide.md#162-initial-end-to-end-test-scenario)
- `crates/pizzlydb-sdk/src/*.rs`
