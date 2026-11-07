# 210: Secrets management and key rotation

| Field | Value |
| --- | --- |
| Phase | 3 Production capabilities |
| Priority | Must |
| Size | M |
| Crates | `pizzlydb-server`, `pizzlydb-engine` |
| Type | Security |

## Summary

Add a `SecretProvider` abstraction with three implementations: files with strict permission checks, environment variables for development only, and a cloud or Vault KMS adapter behind a feature flag. It covers every secret the system uses:

- TLS keys
- the resume-token signing key ring ([070](070-resume-tokens.md))
- backup encryption keys ([106](106-backup-encryption-compression-destinations.md))
- OIDC client secrets
- the API-credential pepper

The abstraction supports rotation without a restart, in-memory zeroization (`zeroize`), and redaction everywhere.

## Motivation

Rule: never hardcode secrets; use environment variables or secret managers. Several Phase 1 and Phase 2 features already need keys. [070](070-resume-tokens.md) uses a key ring and [106](106-backup-encryption-compression-destinations.md) uses envelope encryption, but each currently has its own ad hoc key-loading path. A single provider makes rotation and auditing uniform.

## Current State

- Configuration ([101](101-configuration-system.md)) references secrets by path. There is no provider abstraction.

## Scope

### In scope

- **`SecretProvider` trait:** `get(name)`, `watch(name)`, and version metadata.
- **File provider.** It refuses files readable by group or others, and it re-reads on change.
- **KMS adapter feature.** Decrypts wrapped data keys.
- **Rotation procedures**, documented and tested:
  - resume-token keys: add a new key, sign with it, then retire the old key after the token TTL
  - TLS: reload
  - backup keys: a new key for new backups, while old backups can still be decrypted with retained keys
- **Zeroization and redaction.** Secret types do not implement `Debug` or `Display`.

### Out of scope

- An integrated hardware security module (HSM) beyond what the KMS adapter provides.

## Dependencies

- [101](101-configuration-system.md): configuration.
- [106](106-backup-encryption-compression-destinations.md): backup keys.
- [070](070-resume-tokens.md): token keys.

## Acceptance Criteria

- [ ] Each secret type rotates without a restart, and the old material keeps working for its documented overlap (tests).
- [ ] A permission check rejects key files that are readable by others (test).
- [ ] No secret appears in logs, metrics, panics, or `Debug` output (sentinel test plus a compile-time check that the types do not implement `Debug`).

## Testing and Validation

- Rotation tests and sentinel tests.

## References

- [Guide §9](../architecture/implementation-guide.md#9-security-and-tenancy)
- [`zeroize`](https://docs.rs/zeroize)
