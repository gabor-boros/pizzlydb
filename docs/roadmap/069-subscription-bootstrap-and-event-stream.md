# 069: Race-free subscription bootstrap and the in-process event stream

| Field | Value |
| --- | --- |
| Phase | 1E Reactive core |
| Priority | Must (near-critical path) |
| Size | L |
| Crates | `pizzlydb-subscriptions` (`bootstrap`) |
| Type | Feature |

## Summary

Implement the external subscription semantics of guide §7.1 as an in-process, transport-agnostic stream:

```text
SnapshotBegin(S, plan_revision)
SnapshotRow*
SnapshotEnd(S)
ChangeBatch(commit_seq > S)*
Heartbeat(frontiers)*
```

The snapshot is read at exactly S, through a snapshot lease or as the plan's state at `maintained_seq`. Deltas begin at S+1 with no gaps. Snapshot rows and large change batches are chunked. Every queue is bounded, with explicit backpressure: when a consumer is slow, the stream is paused, disconnected for later replay, or expired, and in each case it carries a precise reason.

## Motivation

The guide requires creation without a resume token to be race-free and forbids silently skipping changes made while the snapshot is being transmitted (§7.1). Invariant R1 says that a client completing snapshot S and consuming every delta through T holds the result at T. Guide §7.5 requires slow consumers to be unable to exhaust shared resources.

## Current State

- `crates/pizzlydb-subscriptions/src/bootstrap.rs` is a one-line stub.

## Scope

### In scope

- **Bootstrap.** Choose S as the plan's `maintained_seq`, or wait until maintained ([066](066-maintenance-scheduler-and-lag.md)). Stream the result rows from the maintained state at S, which must be readable consistently at S through a state snapshot. Then stream the output-log deltas with `commit_seq > S`.
- **Event types and chunking.** Events are typed and chunked. A heartbeat carries `commit_seq`, `maintained_seq`, `delivered_seq`, and `acknowledged_seq`, which correspond to the frontiers of guide §7.6.
- **Stream handle.** A `SubscriptionStream` with a bounded queue, a byte budget, a time-to-ack limit, and a rate limit. Policies (pause, disconnect, expire) are configurable, and the reason codes include `SLOW_CONSUMER`.
- **Resume from a cursor.** Resuming at an acknowledged position replays the unacknowledged batches. Duplicates are permitted by the delivery contract, and gaps are forbidden.
- **Ephemeral subscriptions.** The same stream, but nothing is durable.

### Out of scope

- The network transport ([204](204-subscription-delivery-service.md)) and tokens ([070](070-resume-tokens.md)).

## Dependencies

- [068](068-durable-subscriptions-core.md): the subscription core.
- [066](066-maintenance-scheduler-and-lag.md): frontiers and `wait_until_maintained`.

## Acceptance Criteria

- [ ] Invariant R1, checked by property test: under concurrent writes, a consumer that applies the snapshot and the deltas through T equals the oracle at T, for random T.
- [ ] Invariant R2 at engine level: a disconnect and resume at a random point produces duplicates only, never a gap. Dedup by event identity recovers the exact result.
- [ ] A slow consumer hits its budget and is paused or disconnected according to policy. Server memory stays within the configured bound (test with a counting allocator).
- [ ] A snapshot of 10 million rows streams in bounded memory (benchmark).

## Testing and Validation

- Property tests with concurrent writers and adversarial consumers, plus a soak test.

## References

- [Guide §7.1, §7.5, §7.6](../architecture/implementation-guide.md#71-external-semantics), [§12.2](../architecture/implementation-guide.md#122-mandatory-correctness-properties)
