# 908: Fine-grained authorization: label-, property-, and row-level policies (RFC-gated)

| Field | Value |
| --- | --- |
| Phase | Future (post-GA expansion track) |
| Priority | Could |
| Size | XL |
| Crates | `pizzlydb-auth`, `pizzlydb-query`, `pizzlydb-incremental` |
| Type | RFC, security |

## Summary

Write an RFC for policies finer than a namespace: per-label or per-relationship-type access, property masking, and predicate-based row filters. The RFC must define how these policies interact with incremental maintenance. A policy change must retract rows from active subscriptions exactly, with no leak through deltas computed before the change.

## Motivation

Namespace-level RBAC ([207](207-authorization-rbac-and-policy-epochs.md)) is coarse. Fine-grained policies are a common enterprise requirement. They are hard to get right in a reactive system, because invariant A1 must hold for maintained state and journaled output as well as for live queries.

## Current State

- ADR-009 ([207](207-authorization-rbac-and-policy-epochs.md)) explicitly excludes fine-grained authorization.

## Scope

### In scope

- **The RFC:** a policy language, enforcement through query rewriting versus a delivery filter, incremental retraction on policy change, and the cost of plans scoped per principal.

### Out of scope

- Implementation until the RFC is accepted.

## Dependencies

- [207](207-authorization-rbac-and-policy-epochs.md): RBAC.
- [218](218-security-test-suite.md): the security suite to extend.

## Acceptance Criteria

- [ ] The RFC is accepted, with an A1 proof sketch and a test oracle.
- [ ] If implemented, the security suite is extended with policy-change-during-delivery scenarios, and they pass.

## Testing and Validation

- Security suite extension.

## References

- [Guide §9](../architecture/implementation-guide.md#9-security-and-tenancy)
