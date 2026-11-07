# 209: Tamper-evident audit logging

| Field | Value |
| --- | --- |
| Phase | 3 Production capabilities |
| Priority | Must |
| Size | M |
| Crates | `pizzlydb-server`, `pizzlydb-auth` |
| Type | Security, operations |

## Summary

Emit an audit event for every security-relevant action:

- authentication success and failure
- authorization denials
- policy, role, and credential changes
- schema and index changes
- namespace and tenant lifecycle events
- backup, restore, verify, repair, and salvage
- subscription deletion
- configuration reloads

Events go to a dedicated, append-only audit sink, either a file with rotation or a forwarder. They use a stable JSON schema and include who, what, target, outcome, the request id and trace id, and a timestamp. A hash chain makes the record tamper-evident. Audit events never contain secrets or graph payloads.

## Motivation

Guide §9 requires audit logs that cover administrative actions, policy changes, subscription deletion, and data export. Regulated deployments need a tamper-evident trail, and incident response needs it too.

## Current State

- Not implemented. Structured logging comes from [103](103-structured-logging-and-tracing.md).

## Scope

### In scope

- **Event schema** `audit.v1`, with a versioned, documented field list.
- **Sink.** Separate from operational logs. It is synchronous for administrative actions and fails closed: if the audit write fails, the administrative action fails. For authentication and authorization events on the hot path, it uses a bounded asynchronous queue whose overflow is counted and alerted on.
- **Hash chain.** Each record holds the hash of the previous record, and the chain head is checkpointed periodically into the metrics and log stream. A `pizzlydb audit verify` tool checks the chain.
- **Retention and rotation** configuration.

### Out of scope

- SIEM-specific integrations. Users can forward JSON with standard shippers.

## Dependencies

- [206](206-authentication.md): authentication events.
- [207](207-authorization-rbac-and-policy-epochs.md): authorization and policy events.

## Acceptance Criteria

- [ ] Every action in the list has an audit event, verified by a coverage test that triggers each one.
- [ ] If an audit write fails, the administrative action fails (test with a fault-injected sink).
- [ ] `audit verify` detects a modified, deleted, or reordered record (test).
- [ ] A sentinel test confirms that audit records contain no secrets, tokens, or graph property values.

## Testing and Validation

- Coverage tests, fault injection, and tamper tests.

## References

- [Guide §9](../architecture/implementation-guide.md#9-security-and-tenancy)
