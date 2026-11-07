# 909: Native encryption at rest for the live database (RFC-gated)

| Field | Value |
| --- | --- |
| Phase | Future (post-GA expansion track) |
| Priority | Could |
| Size | L |
| Crates | `pizzlydb-storage` (VFS layer) |
| Type | RFC, security |

## Summary

Add transparent page-level encryption in the VFS layer, for example AES-GCM or XChaCha20-Poly1305 for each page, with a nonce derived from the page number and the write generation. The design includes key hierarchy and rotation through the secret provider ([210](210-secrets-management.md)), and encrypted WAL and metadata files.

## Motivation

For v1, encryption at rest is delegated to the volume layer (dm-crypt/LUKS or cloud-provider disk encryption), which is a documented assumption. Backups are encrypted natively ([106](106-backup-encryption-compression-destinations.md)). Native encryption matters when the volume layer cannot be trusted or when per-tenant keys are needed.

## Current State

- No encryption in the storage layer. The VFS abstraction ([011](011-vfs-capabilities.md)) is the natural place to add it.

## Scope

### In scope

- **The RFC:** threat model (what volume encryption does not cover), the nonce-uniqueness argument across crashes and torn writes, the interaction with the CRC-32C page checksums, the performance budget, and key rotation (online re-encryption through compaction, [113](113-space-reclamation-and-compaction.md)).
- **Implementation behind a format flag** ([024](024-storage-format-versioning-and-golden-fixtures.md)).

### Out of scope

- Per-tenant keys, unless the RFC justifies them.

## Dependencies

- [011](011-vfs-capabilities.md): the VFS layer.
- [210](210-secrets-management.md): key management.
- [024](024-storage-format-versioning-and-golden-fixtures.md): format versioning.

## Acceptance Criteria

- [ ] The RFC is accepted, with a nonce-reuse analysis.
- [ ] The crash suite ([025](025-storage-model-based-and-crash-suite.md)) passes with encryption enabled.
- [ ] Tampering with a page is detected as corruption.
- [ ] The performance overhead is measured and documented.

## Testing and Validation

- Crash suite, tamper tests, and benchmarks.

## References

- [Guide §9](../architecture/implementation-guide.md#9-security-and-tenancy)
