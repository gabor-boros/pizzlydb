# 205: TLS and mTLS transport security

| Field | Value |
| --- | --- |
| Phase | 3 Production capabilities |
| Priority | Must (critical path) |
| Size | M |
| Crates | `pizzlydb-server`, `pizzlydb-sdk` |
| Type | Security |

## Summary

TLS is required for every non-local connection, using rustls with TLS 1.2 and 1.3 and a modern cipher policy. Optional mTLS supports service-to-service authentication, and certificates and keys reload without a restart. Plaintext is allowed only on loopback or a Unix socket, behind an explicit development flag. The SDK verifies servers with system roots or a configured CA bundle, and it never offers an "accept invalid certificates" option outside test builds.

## Motivation

Guide §9 requires TLS for non-local connections and supports mTLS. Every credential and every data payload crosses this boundary.

## Current State

- Not implemented.

## Scope

### In scope

- **Server:** `rustls` configuration from files or a secret provider ([210](210-secrets-management.md)), certificate reload on file change or `SIGHUP`, optional client-certificate verification against a CA, and extraction of the client-certificate identity for [206](206-authentication.md).
- **Startup policy:** the server refuses to start on a non-loopback listener without TLS unless an explicit insecure flag is set, and that flag logs a warning on every startup.
- **SDK:** TLS configuration that supports system roots, a custom CA, and client certificates.
- **Metrics:** handshake failures by reason, and days until certificate expiry.

### Out of scope

- Authentication semantics ([206](206-authentication.md)).

## Dependencies

- [202](202-server-runtime.md): the server runtime.

## Acceptance Criteria

- [ ] A non-loopback plaintext listener is refused without the insecure flag (test).
- [ ] Each of these is rejected with a clear error: an expired certificate, the wrong host name, an unknown CA, and a missing client certificate when mTLS is required (tests).
- [ ] A certificate rotated through reload applies to new connections, and existing connections continue (test).
- [ ] An external scanner (for example `testssl.sh`) reports no weak protocols or ciphers. It runs as a nightly job.

## Testing and Validation

- Integration tests with generated test certificates (`rcgen`) and a scanner job.

## References

- [Guide §9](../architecture/implementation-guide.md#9-security-and-tenancy)
- [rustls](https://docs.rs/rustls)
