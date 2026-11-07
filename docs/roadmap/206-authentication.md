# 206: Authentication: OIDC/JWT, service principals, and API credentials

| Field | Value |
| --- | --- |
| Phase | 3 Production capabilities |
| Priority | Must (critical path) |
| Size | L |
| Crates | `pizzlydb-auth` (`identity`), `pizzlydb-server` |
| Type | Security |

## Summary

Authenticate every request to a `Principal` before it reaches the engine. Three credential types are supported:

- **OIDC/JWT bearer tokens** with strict validation of issuer, audience, expiry, not-before, signing algorithm, and key id. The JWKS is fetched over TLS, cached, and rotated.
- **mTLS client certificates**, mapped to service principals ([205](205-tls-transport-security.md)).
- **Scoped API credentials** for service principals. Secrets are stored hashed with Argon2id, credentials are rotated with an overlap window, and they can be revoked.

Every authentication failure is counted in metrics and recorded in the audit log ([209](209-audit-logging.md)).

## Motivation

Guide §9 requires OIDC/JWT verification with strict issuer, audience, expiry, and key-rotation checks, supports service principals and scoped API credentials, and requires every operation to be authenticated (§8.2). The resume-token principal binding ([070](070-resume-tokens.md)) and RBAC ([207](207-authorization-rbac-and-policy-epochs.md)) both need a stable principal identity.

## Current State

- `crates/pizzlydb-auth/src/identity.rs` contains only the doc comment "Principals, including service credentials."

## Scope

### In scope

- **`Principal`:** a stable id, a kind (user, service, or certificate), the tenant or organization, groups and roles taken from configured claims, and the authentication time.
- **JWT validation.** It uses a vetted library such as `jsonwebtoken` together with explicit policy:
  - Allowed algorithms come from an allowlist; `none` and symmetric algorithms are rejected unless configured.
  - Clock skew is bounded.
  - JWKS is refreshed on an unknown `kid`, at a limited rate.
- **API credential store.** Credentials live in the system key space, hashed with Argon2id. Each has scopes, an expiry, and a revocation flag, and admin RPCs manage them ([211](211-admin-api.md)).
- **Server middleware.** It extracts credentials, caches authentication results with a short lifetime, and maps failures to `UNAUTHENTICATED` without leaking the reason to the client. The detailed reason goes to the audit log.
- **Development mode.** The local Unix socket can map to a configured local admin principal, enabled explicitly.

### Out of scope

- Authorization ([207](207-authorization-rbac-and-policy-epochs.md)).

## Dependencies

- [205](205-tls-transport-security.md): TLS.

## Acceptance Criteria

- [ ] Each of these JWT cases is rejected (negative test suite): expired, a future `nbf`, the wrong issuer, the wrong audience, `alg=none`, the wrong key, and an unknown `kid` after a refresh.
- [ ] JWKS rotation: a new key is accepted after rotation without a restart (test with a mock IdP).
- [ ] A revoked API credential is rejected within the configured cache lifetime (test).
- [ ] Neither credentials nor tokens appear in any log (sentinel test).

## Testing and Validation

- Negative and positive test suites, and an integration test with a mock OIDC provider.

## References

- [Guide §9](../architecture/implementation-guide.md#9-security-and-tenancy)
- [RFC 8725: JWT Best Current Practices](https://www.rfc-editor.org/rfc/rfc8725)
- `crates/pizzlydb-auth/src/identity.rs`
