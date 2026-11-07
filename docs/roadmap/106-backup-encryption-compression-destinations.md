# 106: Backup encryption, compression, and destinations

| Field | Value |
| --- | --- |
| Phase | 2 Operational layer |
| Priority | Must (encryption and local destination); Should (object storage) |
| Size | M |
| Crates | `pizzlydb-storage` (`backup`), `pizzlydb-engine` |
| Type | Feature, security |

## Summary

Add three capabilities to backups:

- **Authenticated encryption.** Envelope encryption: a random data key is generated per backup and wrapped by a key-encryption key that comes from a pluggable key provider (a local key file for development, and KMS through [210](210-secrets-management.md)). Encryption metadata, meaning the algorithm, the key id, and the wrapped data key, is stored in the manifest.
- **Compression** with zstd, applied before encryption.
- **A destination abstraction.** A local directory is required, and S3-compatible object storage is optional. Integrity is verified on every read.

## Motivation

Guide §10.2 requires encryption metadata and integrity verification. Backups usually leave the trust boundary of the database host, and an unencrypted backup is a data-exfiltration risk. Object storage is the usual off-site destination.

## Current State

- No encryption or compression exists. The destination is a local path written through the `Vfs`.

## Scope

### In scope

- **Streaming AEAD.** XChaCha20-Poly1305 or AES-256-GCM, using the STREAM construction so that chunks cannot be reordered or truncated, implemented with RustCrypto or `ring`. The choice and the rationale go in the ADR. Key handling: data keys are zeroized after use, and no key material is written to logs.
- **Key provider trait.** Implementations: a file key (development only, with a warning) and an external provider through [210](210-secrets-management.md).
- **zstd compression** with a configurable level.
- **`BackupDestination` trait** with `LocalDir` (required) and S3-compatible object storage (optional, behind a cargo feature). Large objects use multipart upload, with retries and checksum verification.
- **Manifest fields** for the encryption and compression metadata. The manifest itself is authenticated, either signed or MACed.

### Out of scope

- Encryption at rest for the live database ([909](909-native-encryption-at-rest.md)). For v1 this is delegated to the volume layer.

## Dependencies

- [105](105-online-consistent-backup.md): the backup format and manifest.

## Acceptance Criteria

- [ ] Each of the following makes verification fail with a typed error: a flipped byte in any chunk, a reordered or truncated chunk, a modified manifest, or the wrong key (tests).
- [ ] An encrypted and compressed backup restores byte-identically to an unencrypted one (round-trip test).
- [ ] No plaintext page content is present in the encrypted backup (test with a sentinel value).
- [ ] The S3 destination passes integration tests against MinIO or LocalStack in CI, if the feature is shipped.

## Testing and Validation

- Unit tests, fault tests, the round trip, and an entry in the security review checklist ([217](217-security-hardening-and-threat-model.md)).

## References

- [Guide §10.2](../architecture/implementation-guide.md#102-backups-and-restore), [§9](../architecture/implementation-guide.md#9-security-and-tenancy)
- Hoang, Reyhanitabar, Rogaway, and Vizár, "Online Authenticated-Encryption and its Nonce-Reuse Misuse-Resistance" (STREAM, CRYPTO 2015)
