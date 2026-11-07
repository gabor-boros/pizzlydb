# 204: Subscription delivery service: streaming, acknowledgements, flow control, and resume

| Field | Value |
| --- | --- |
| Phase | 3 Production capabilities |
| Priority | Must |
| Size | L |
| Crates | `pizzlydb-server`, `pizzlydb-subscriptions` |
| Type | Feature |

## Summary

Expose subscriptions over gRPC. Clients consume a server-streaming `Subscribe` RPC, which carries the snapshot-phase events, the change batches, and heartbeats that report all four frontiers. Durable cursors advance through a separate `Ack` RPC, and the service also provides a `WaitUntilMaintained` RPC. The engine's bounded queues are mapped onto HTTP/2 flow control and an application-level credit window. Slow consumers follow the engine policy (pause, disconnect, or expire), and each such event carries a precise reason. Clients resume with a resume token.

## Motivation

Guide §7.1 defines the event sequence, guide §7.2 defines at-least-once delivery in commit order, guide §7.5 requires flow control, and guide §8.1 requires server streaming for subscriptions with explicit ack RPCs. The engine already implements the semantics ([069](069-subscription-bootstrap-and-event-stream.md)); this issue must preserve them over an unreliable network.

## Current State

- Not implemented.

## Scope

### In scope

- **`Subscribe` RPC.** Its parameters are the query, the parameters, an optional durable name, and an optional resume token. The stream carries typed events with their event identity. Clients can request a credit window (`request_n`-style), or the server relies on HTTP/2 window backpressure; the ADR in this issue chooses one.
- **`Ack` RPC.** It returns only after the cursor is durable ([068](068-durable-subscriptions-core.md)).
- **Heartbeats.** They are sent at a configured interval and carry `commit_seq`, `maintained_seq`, `delivered_seq`, `acknowledged_seq`, and the lease expiry.
- **Disconnect handling.** A disconnect leaves a durable subscription in the `Disconnected` state with its retention window running. An ephemeral subscription is destroyed.
- **`WaitUntilMaintained` RPC.** It takes a deadline.
- **Delivery metrics:** backlog, ack latency, replay rate, and disconnect reasons.

### Out of scope

- Authorization checks during delivery ([207](207-authorization-rbac-and-policy-epochs.md)) and SDK resume logic ([212](212-rust-sdk.md)).

## Dependencies

- [202](202-server-runtime.md): the server runtime.
- [069](069-subscription-bootstrap-and-event-stream.md): the event stream.
- [070](070-resume-tokens.md): tokens.

## Acceptance Criteria

- [ ] R1 and R2 hold over the network. Property tests with random connection drops, server restarts, and slow clients show that the client's applied result equals the oracle at its acknowledged frontier, and that gaps never occur.
- [ ] A slow client cannot grow server memory beyond its per-subscription budget (test with a counting allocator).
- [ ] Heartbeats report frontiers that are correct and monotonic.

## Testing and Validation

- Network fault-injection tests that use a proxy to drop, delay, and reorder TCP segments, plus a soak test.

## References

- [Guide §7.1–§7.6](../architecture/implementation-guide.md#7-durable-subscriptions), [§8.1](../architecture/implementation-guide.md#81-protocol)
