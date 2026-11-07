# 211: Administrative API

| Field | Value |
| --- | --- |
| Phase | 3 Production capabilities |
| Priority | Must (critical path) |
| Size | M |
| Crates | `pizzlydb-server`, `pizzlydb-protocol` |
| Type | Feature, operations |

## Summary

Expose every Phase 2 operation through authenticated, authorized, and audited RPCs:

- **Backup and restore:** start, status, list, and verify ([105](105-online-consistent-backup.md), [108](108-backup-verification-and-restore.md)).
- **Verification and scrub** status ([107](107-full-verification-and-online-scrubbing.md)).
- **Maintenance controls** ([115](115-background-maintenance-scheduler.md)).
- **Retention and plan inspection** ([114](114-retention-management-and-inspection.md)).
- **Diagnostics** ([116](116-diagnostics-and-introspection.md)).
- **Namespace and tenant lifecycle.**
- **Credentials and roles** ([206](206-authentication.md), [207](207-authorization-rbac-and-policy-epochs.md)).
- **Configuration view and reload.**

Long-running operations follow a shared job model.

## Motivation

Operators need remote, scriptable, and access-controlled administration. The engine exposes these operations only in-process. Guide §8.2 lists the administrative functions, including subscription inspect and delete, query and plan explain, and metrics and health.

## Current State

- The operations are engine APIs from Phase 2, with no remote access.

## Scope

### In scope

- **`AdminService`** RPCs, each with its own permission ([207](207-authorization-rbac-and-policy-epochs.md)) and an audit event ([209](209-audit-logging.md)).
- **Job model:** start an operation to get a job id, then poll its status, cancel it, or read its result. There is a bounded limit on concurrent jobs, and job history is retained.
- **Dangerous operations** (restore, salvage, and namespace drop) require explicit confirmation tokens, and dry-run is the default where possible.

### Out of scope

- A web UI.

## Dependencies

- [201](201-native-protocol-definition.md): the protocol.
- [207](207-authorization-rbac-and-policy-epochs.md): authorization.
- [108](108-backup-verification-and-restore.md): backup and restore.
- [114](114-retention-management-and-inspection.md): retention.
- [116](116-diagnostics-and-introspection.md): diagnostics.

## Acceptance Criteria

- [ ] Every Phase 2 operator action is available through the API (coverage checklist test).
- [ ] Every RPC is authorization-checked and audited (matrix test shared with [207](207-authorization-rbac-and-policy-epochs.md) and [209](209-audit-logging.md)).
- [ ] Cancelling a job leaves the system consistent (tests for backup, verify, and compaction).

## Testing and Validation

- Integration tests through the server.

## References

- [Guide §8.2](../architecture/implementation-guide.md#82-initial-api-surface), [§10](../architecture/implementation-guide.md#10-operational-target)
