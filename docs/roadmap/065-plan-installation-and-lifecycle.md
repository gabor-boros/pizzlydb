# 065: Plan installation, sharing, invalidation, and restart recovery

| Field | Value |
| --- | --- |
| Phase | 1E Reactive core |
| Priority | Must (critical path) |
| Size | L |
| Crates | `pizzlydb-incremental`, `pizzlydb-txn` |
| Type | Feature |

## Summary

Manage the lifecycle of maintained plans:

- **Installation** happens atomically with registering a protected log frontier at S. The initial state is evaluated at snapshot S through [061](061-as-of-snapshots-at-commit-sequence.md), and tailing starts at S+1.
- **Sharing.** Identical plans, with the same fingerprint, parameters, namespace, and authorization scope, share one maintained instance.
- **Removal** releases the plan's state and retention floors.
- **Invalidation.** A schema change that affects a plan is detected, the plan's revision is bumped or the plan is invalidated (`PLAN_INVALIDATED`), and consumers are notified.
- **Restart.** Every installed plan is recovered on restart.

## Motivation

Guide §6.4 says that an initial graph scan followed by tailing must not lose an intervening commit. Guide §3.3 says that schema changes affecting typing invalidate subscriptions through an explicit plan-revision protocol. Sharing plans prevents N identical subscriptions from consuming N times the state.

## Current State

- Not implemented. Schema-change records come from [043](043-schema-and-catalog-lifecycle.md), and as-of snapshots from [061](061-as-of-snapshots-at-commit-sequence.md).

## Scope

### In scope

- **Install protocol.**
  1. Register a durable floor at S, where S is the latest commit.
  2. Evaluate the initial state at snapshot S in bounded chunks, persisting state as it goes.
  3. Set `processed_seq = S`.
  4. Begin tailing.

  If the process crashes during the initial build, the build resumes or restarts, and no commit is ever missed.
- **Sharing.** Plans are registered under a key of plan fingerprint, parameter fingerprint, namespace, and authorization scope, with reference counts. Data isolation between tenants is respected, so plans are never shared across namespaces.
- **Invalidation.** Each plan records the schema elements it depends on. When the processor encounters a `SchemaChanged` log record that touches one of them, it either recompiles the plan to a new revision with a documented restart semantic, or invalidates the plan. The choice follows rules in the language RFC.
- **Removal.** Deleting a plan's state is chunked, its floors are released, and the space is reclaimed.
- **Recovery.** On startup, every plan is loaded and resumes from its `processed_seq`.

### Out of scope

- Scheduling and lag ([066](066-maintenance-scheduler-and-lag.md)), and subscriber cursors ([068](068-durable-subscriptions-core.md)).

## Dependencies

- [064](064-maintained-state-and-output-log.md): state persistence.
- [061](061-as-of-snapshots-at-commit-sequence.md): as-of snapshots.
- [043](043-schema-and-catalog-lifecycle.md): schema versions and change events.

## Acceptance Criteria

- [ ] Installing a plan while 1,000 commits/s arrive results in a state at every later frontier that equals the oracle (property test). Nothing is missed or applied twice.
- [ ] A crash during the initial build, followed by recovery, gives a correct plan state (crash simulation).
- [ ] A dependent schema change triggers the documented revision or invalidation behavior, and an unrelated schema change does not (tests).
- [ ] Two identical subscriptions share one plan instance (state-size assertion).

## Testing and Validation

- Property tests with concurrent writers, and crash simulation.

## References

- [Guide §6.4](../architecture/implementation-guide.md#64-checkpoints-and-replay), [§3.3](../architecture/implementation-guide.md#33-schema-model), [§6.5](../architecture/implementation-guide.md#65-graph-specific-correctness-traps)
