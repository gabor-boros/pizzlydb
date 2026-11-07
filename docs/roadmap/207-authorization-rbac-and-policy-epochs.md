# 207: Authorization: RBAC, enforcement points, and policy epochs (ADR-009)

| Field | Value |
| --- | --- |
| Phase | 3 Production capabilities |
| Priority | Must (decision gate, critical path) |
| Size | L |
| Crates | `pizzlydb-auth` (`permission`, `policy`), `pizzlydb-query` (hook), `pizzlydb-subscriptions`, `pizzlydb-server` |
| Type | Security, ADR |

## Summary

Implement role-based access control over organizations, namespaces, graphs, and administrative operations. It is enforced at three points:

1. Before planning, through the binder hook from [052](052-binder-type-checker-and-ir.md).
2. Before every mutation.
3. Before *every delivery* of subscription data.

Policy changes bump a durable policy epoch. Subscriptions and resume tokens are bound to the principal and the epoch, and a revocation invalidates the affected subscriptions with `RESUME_FORBIDDEN` or `PERMISSION_DENIED`. ADR-009 records the model.

## Motivation

Guide §9 requires RBAC at least for organization, namespace, graph, and administrative operations, query authorization before planning or result delivery, and resume tokens bound to identity, permissions, and policy revision. Invariant A1 states that no principal can obtain graph contents or cached or durable subscription deltas beyond the authority it currently holds. Durable subscriptions are the hard case: a principal whose access is revoked must stop receiving deltas, including replays of data journaled before the revocation.

## Current State

- `crates/pizzlydb-auth/src/{permission,policy}.rs` contain doc comments only.
- The binder authorization hook comes from [052](052-binder-type-checker-and-ir.md).

## Scope

### In scope

- **ADR-009.** Permission vocabulary, for example `namespace.read`, `namespace.write`, `namespace.subscribe`, `schema.admin`, `ops.backup`, and `ops.admin`. Role definitions and bindings at organization and namespace scope. Deny-by-default. Explicit non-goals: no property- or row-level authorization, which [908](908-fine-grained-authorization.md) may add later, and no authorization derived from graph reachability.
- **Policy store.** Roles and bindings live in the system key space and are managed transactionally through the admin API. Each change bumps the policy epoch.
- **Enforcement.** The query hook, the mutation RPCs, subscription creation, resume, and *every delivery batch*, checked against the current epoch through a cache invalidated by an epoch bump. Explain output is protected as well.
- **Token binding.** Resume tokens ([070](070-resume-tokens.md)) bind the principal and the epoch. On resume, permissions are rechecked; if they still hold, the subscription continues and the token is reissued with the new epoch, otherwise the resume is rejected.
- **Plan sharing** ([065](065-plan-installation-and-lifecycle.md)) respects the authorization scope, so plans are never shared across scopes that could leak data.

### Out of scope

- Fine-grained authorization ([908](908-fine-grained-authorization.md)).

## Dependencies

- [206](206-authentication.md): authentication.
- [070](070-resume-tokens.md): tokens.
- [203](203-transaction-and-query-service.md) and [204](204-subscription-delivery-service.md): enforcement sites.
- [045](045-namespace-registry-and-lifecycle.md): namespaces.

## Acceptance Criteria

- [ ] A complete enforcement matrix test covers every RPC against every permission, checking both deny and allow.
- [ ] Revocation during an active subscription stops delivery within one batch, or within the documented delay. A later resume is rejected. Replays of pre-revocation journal data are not delivered (end-to-end test, invariant A1).
- [ ] No plan instance is shared between principals with different authorization scopes (test).
- [ ] ADR-009 is accepted.

## Testing and Validation

- Matrix tests and end-to-end revocation tests. Further coverage comes from the security suite in [218](218-security-test-suite.md).

## References

- [Guide §9](../architecture/implementation-guide.md#9-security-and-tenancy), [§7.3](../architecture/implementation-guide.md#73-resume-tokens), [§14 ADR-009](../architecture/implementation-guide.md#14-mandatory-adrrfc-process)
- `crates/pizzlydb-auth/src/{permission,policy}.rs`
