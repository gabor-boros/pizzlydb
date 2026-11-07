# 218: Security test suite: authorization matrix, tenant isolation, and revocation during delivery

| Field | Value |
| --- | --- |
| Phase | 3 Production capabilities |
| Priority | Must |
| Size | M |
| Crates | `pizzlydb-testkit`, `pizzlydb-server` tests |
| Type | Testing, security |

## Summary

Build an automated suite that proves invariants A1 and C8 end to end through the network API. It includes:

- A complete authorization matrix covering every RPC, permission, and scope.
- Cross-tenant probes through every API: queries, explain, errors, metrics labels, inspect output, subscription names, and resume tokens.
- A revocation test during live delivery and replay.
- Token tampering, replay, and cross-principal resume.
- Abuse cases: oversized messages, deep nesting, slow-read and slow-write clients, and connection floods.

## Motivation

Guide §12.1 lists "Security" as a required testing layer: permission enforcement, token tampering, and revoked-principal resume. Guide §12.2 makes A1 and C8 mandatory properties. Individual issues test their own enforcement points; this suite tests the *composition*.

## Current State

- Not implemented.

## Scope

### In scope

- A generated authorization matrix from the permission vocabulary ([207](207-authorization-rbac-and-policy-epochs.md)), executed against a live server.
- A two-tenant fixture with canary data, plus assertions that canary values never appear in any response, log, or metric of the other tenant.
- Revocation scenarios: revoke during the snapshot phase, during live deltas, while disconnected, and during a replay of journaled data.
- Token attacks: bit flips, a signature from another key, swapping the principal, an expired policy epoch, and the token of another subscription.
- The suite runs in CI on every pull request that touches auth, server, subscriptions, or query crates, and nightly.

### Out of scope

- External penetration testing ([223](223-external-review-and-ga-gate.md)).

## Dependencies

- [204](204-subscription-delivery-service.md): delivery.
- [207](207-authorization-rbac-and-policy-epochs.md): authorization.
- [208](208-multi-tenancy-and-quotas.md): tenancy.
- [209](209-audit-logging.md): audit logging, so that denials can be asserted as audited.

## Acceptance Criteria

- [ ] Every matrix cell is green, and new RPCs fail CI until they are added to the matrix.
- [ ] Canary-leak assertions pass across all surfaces.
- [ ] All revocation scenarios deliver no data after the documented revocation delay.
- [ ] All token attacks are rejected with the correct code and are audited.

## Testing and Validation

- The suite itself, run in CI.

## References

- [Guide §12.1](../architecture/implementation-guide.md#121-testing-layers), [§12.2 A1/C8](../architecture/implementation-guide.md#122-mandatory-correctness-properties)
