# 070: Resume tokens

| Field | Value |
| --- | --- |
| Phase | 1E Reactive core |
| Priority | Must |
| Size | M |
| Crates | `pizzlydb-subscriptions` |
| Type | Feature, security-sensitive |

## Summary

Implement versioned, opaque, integrity-protected resume tokens as specified in guide §7.3. A token is authenticated with an AEAD or HMAC under a rotating keyring, and it binds:

- the database lineage, the namespace, and an epoch
- the subscription id, the plan fingerprint, the parameter fingerprint, and the plan revision
- the principal and scope, and the policy epoch (placeholders until [207](207-authorization-rbac-and-policy-epochs.md))
- the last acknowledged output position
- an expiry and the token format version

When a token is presented, the engine validates every binding and then either resumes or returns a typed error: `CURSOR_EXPIRED`, `PLAN_INVALIDATED`, `RESUME_FORBIDDEN`, or `INVALID_TOKEN`.

## Motivation

The guide says that invalid tokens yield typed errors and never result in undetected gaps. A token that is not integrity-protected could be forged to skip changes, or to resume another principal's subscription. Defining the token format at the engine level keeps transport layers from improvising it.

## Current State

- Not implemented. The keyring and secret management arrive in [210](210-secrets-management.md).

## Scope

### In scope

- Token format v1: a binary payload with a version byte, then AEAD (for example XChaCha20-Poly1305 or AES-GCM-SIV) under a key id, then base64url. A server-side token store is an alternative, and the ADR must justify the choice made.
- A `Keyring` trait with current and previous keys for rotation, and an in-memory test implementation. The production key source is [210](210-secrets-management.md).
- Validation that checks the token's bindings against the live engine state: subscription exists, plan revision is compatible, the cursor is inside retention, and the principal and epoch match (a placeholder hook that [207](207-authorization-rbac-and-policy-epochs.md) implements).
- A fuzz target for the decoder.

### Out of scope

- The authentication and authorization implementation ([206](206-authentication.md), [207](207-authorization-rbac-and-policy-epochs.md)).

## Dependencies

- [068](068-durable-subscriptions-core.md): subscription records and cursors.

## Acceptance Criteria

- [ ] Tokens that have been tampered with, truncated, signed with the wrong key, expired, or issued for a different subscription are each rejected with the documented error (tests).
- [ ] Rotation: a token signed with the previous key is accepted until the previous key is retired (test).
- [ ] A one-hour fuzz run of the decoder finds no panic.
- [ ] The token reveals no plaintext principal or parameter values, unless the ADR justifies doing so.

## Testing and Validation

- Unit tests, fuzzing, and a security review checklist item ([217](217-security-hardening-and-threat-model.md)).

## References

- [Guide §7.3 resume tokens](../architecture/implementation-guide.md#73-resume-tokens)
