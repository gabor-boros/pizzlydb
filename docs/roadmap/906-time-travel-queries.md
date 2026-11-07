# 906: User-facing time-travel queries (RFC-gated)

| Field | Value |
| --- | --- |
| Phase | Future (post-GA expansion track) |
| Priority | Could |
| Size | L |
| Crates | `pizzlydb-query`, `pizzlydb-txn`, `pizzlydb-engine` |
| Type | RFC, feature |

## Summary

Let clients run snapshot queries `AS OF CommitSeq` or `AS OF timestamp` over a configurable history window. This requires retention policies, quotas, and explicit `HISTORY_EXPIRED` errors.

## Motivation

[061](061-as-of-snapshots-at-commit-sequence.md) implements as-of snapshots internally, for bootstrap and replay at protected floors. Exposing the same capability to users enables auditing and debugging. However, user-facing history has a storage cost that must be bounded and charged ([208](208-multi-tenancy-and-quotas.md)).

## Current State

- Internal as-of snapshots exist only at protected floors ([061](061-as-of-snapshots-at-commit-sequence.md)).

## Scope

### In scope

- **The RFC:** retention model, the mapping from timestamp to `CommitSeq` (commit metadata from [031](031-commit-sequence-and-commit-metadata.md)), cost, and quotas.
- **Implementation:** query syntax and API parameters.

### Out of scope

- Bitemporal modeling.

## Dependencies

- [061](061-as-of-snapshots-at-commit-sequence.md): as-of snapshots.
- [208](208-multi-tenancy-and-quotas.md): quotas.

## Acceptance Criteria

- [ ] The RFC is accepted.
- [ ] As-of query results equal the oracle at each sequence number within the window (test).
- [ ] Queries outside the window fail with `HISTORY_EXPIRED`.
- [ ] Retained history is charged against the tenant quota.

## Testing and Validation

- Differential tests with the reference model.

## References

- [Guide §4.2](../architecture/implementation-guide.md#42-versioning-model), [§4.5](../architecture/implementation-guide.md#45-retention-and-recovery)
