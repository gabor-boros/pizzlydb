# ADR-008: Snapshot and subscription protocol, acknowledgement, replay, and expiry

- Status: Reserved
- Owner issue: [068](../roadmap/068-durable-subscriptions-core.md)
- Guide: [§7 durable subscriptions](../architecture/implementation-guide.md#7-durable-subscriptions), [§14](../architecture/implementation-guide.md#14-mandatory-adrrfc-process)

This number is reserved. No decision is recorded yet. The owner issue replaces this file with the ADR, using the [ADR template](template.md).

## Open questions

- Subscription identity, the event-identity tuple, acknowledgement semantics, the lease model, and expiry. The roadmap's starting recommendation is durable named cursors with leases and explicit expiry.
