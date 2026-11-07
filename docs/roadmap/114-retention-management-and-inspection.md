# 114: Retention management and `inspect_retention` (ADR-010)

| Field | Value |
| --- | --- |
| Phase | 2 Operational layer |
| Priority | Must (decision gate) |
| Size | M |
| Crates | `pizzlydb-txn`, `pizzlydb-subscriptions`, `pizzlydb-incremental`, `pizzlydb-engine` |
| Type | Feature, ADR |

## Summary

Unify every retention mechanism under one documented policy and one inspection API:

- snapshot retention ([037](037-snapshot-retention-and-transaction-limits.md))
- protected log floors ([061](061-as-of-snapshots-at-commit-sequence.md))
- commit-log truncation ([032](032-logical-mutation-log.md))
- output-log retention ([068](068-durable-subscriptions-core.md))
- backup and archive holds ([105](105-online-consistent-backup.md), [118](118-log-archiving-and-point-in-time-restore.md))

Each consumer class gets a maximum retention, in time and in bytes. Hitting a maximum triggers a defined action: the consumer is expired with a typed error, or truncation is paused and an alert raised. `inspect_retention()` reports what each holder pins and how many bytes it retains.

## Motivation

Guide §4.5 and §8.2 list `inspect_retention`, and ADR-010 (retention policy) is mandatory under guide §14. Retention is where slow consumers turn into disk exhaustion (C9). Operators must be able to answer one question: *why is my disk filling up, and who is holding it?*

## Current State

- The individual mechanisms come from Phase 1 issues. No unified policy, cap, or inspection API exists yet.

## Scope

### In scope

- **ADR-010:** the retention classes, the defaults, the order in which caps are applied, and the expiry semantics for each class.
- **`RetentionManager`:** it computes the effective floors, applies the caps, and triggers expiry with reasons. Expired subscriptions receive `CURSOR_EXPIRED`, and a plan whose log floor is cut is marked for rebuild.
- **`inspect_retention()`** returns a list of holders. Each entry gives the kind, the id, the namespace, the frontier, the age, the retained WAL bytes, the retained log bytes, and the retained output bytes.
- **Metrics and alerts** for retained bytes per class ([102](102-metrics-foundation.md)).

### Out of scope

- Per-tenant retention quotas ([208](208-multi-tenancy-and-quotas.md)).

## Dependencies

- Engine retention mechanisms: [037](037-snapshot-retention-and-transaction-limits.md), [068](068-durable-subscriptions-core.md), [032](032-logical-mutation-log.md).
- Backup holds: [105](105-online-consistent-backup.md).

## Acceptance Criteria

- [ ] A subscription that is never acknowledged does not grow retained bytes past its class cap. It is expired with a typed error (soak test).
- [ ] `inspect_retention()` attributes at least 95% of the retained bytes to named holders (test that compares against the file sizes).
- [ ] ADR-010 is accepted.

## Testing and Validation

- Soak tests with adversarial consumers, unit tests, and an injected clock.

## References

- [Guide §4.5](../architecture/implementation-guide.md#45-retention-and-recovery), [§6.4](../architecture/implementation-guide.md#64-checkpoints-and-replay), [§7.4](../architecture/implementation-guide.md#74-persisted-output-and-cursors), [§14](../architecture/implementation-guide.md#14-mandatory-adrrfc-process)
