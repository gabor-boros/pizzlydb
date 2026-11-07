# 068: Durable subscriptions core: identity, cursors, leases, retention, and expiry (ADR-008)

| Field | Value |
| --- | --- |
| Phase | 1E Reactive core |
| Priority | Must (decision gate) |
| Size | L |
| Crates | `pizzlydb-subscriptions` (`cursor`, `output_log`) |
| Type | Feature, ADR |

## Summary

Implement the engine-side model for subscriptions, independent of any transport:

- **Named durable subscriptions** bind a plan instance to a durable client consumption cursor and to lease and retention metadata.
- **Unnamed ephemeral subscriptions** do not survive a disconnect.
- **Acknowledgement** advances the cursor durably before the acknowledgement is confirmed.
- **Bounded retention** keeps the history of disconnected subscriptions only for a configured window. After that the subscription expires and a resume returns `CURSOR_EXPIRED`.

The output-log retention floor is derived from the cursors, subject to a maximum retention bound.

## Motivation

Guide §7.2 and §7.4 define the delivery contract: at-least-once delivery in commit order, cursors that track client consumption rather than database position, cursor state flushed before an acknowledgement is confirmed, and bounded retention for disconnected subscriptions. ADR-008 must record these semantics and their consequences.

## Current State

- `crates/pizzlydb-subscriptions/src/{cursor,output_log,bootstrap}.rs` are one-line stubs.

## Scope

### In scope

- **ADR-008.** Subscription identity and the event-identity tuple `(subscription_id, plan_revision, commit_seq, batch_index)`. The ack semantics, the lease model, the retention policy, and the expiry semantics.
- **Subscription records.** Stored under `sys/sub/<ns>/<id>` with the principal placeholder, the plan reference, the parameters, the cursor, the lease expiry, the creation time, and the state. Each record change is a system commit.
- **`ack(sub, position)`.** Monotonic. It is durable before it returns, and it uses group commit ([020](020-group-commit-and-durability-modes.md)) for throughput.
- **Floors.** Each plan's output-log floor is the minimum over its active cursors and over durable subscriptions inside their retention window. A maximum retention (time and bytes) prevents unbounded pinning.
- **Expiry.** An expiry job marks subscriptions `Expired` and releases their floors.
- **Ephemeral subscriptions** hold only in-memory floors.

### Out of scope

- Bootstrap and the event stream ([069](069-subscription-bootstrap-and-event-stream.md)), and tokens ([070](070-resume-tokens.md)).

## Dependencies

- [064](064-maintained-state-and-output-log.md): the output log.
- [065](065-plan-installation-and-lifecycle.md): plan references.

## Acceptance Criteria

- [ ] An ack that has returned survives a crash, while an ack in flight may be lost but is never partially applied (crash simulation).
- [ ] Output-log truncation never removes deltas above the cursor of any live durable subscription inside its window (property test).
- [ ] A subscription disconnected longer than the window expires, and its floor is released (time-controlled test).
- [ ] ADR-008 is accepted.

## Testing and Validation

- Property tests, crash simulation, and tests with an injected clock.

## References

- [Guide §7.2, §7.4](../architecture/implementation-guide.md#72-delivery-guarantee), [§14 ADR-008](../architecture/implementation-guide.md#14-mandatory-adrrfc-process)
- `crates/pizzlydb-subscriptions/src/*.rs`
