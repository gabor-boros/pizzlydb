# ADR-012: Storage format versioning

- Status: Accepted
- Date: 2026-11-07
- Owner issue: [024](../roadmap/024-storage-format-versioning-and-golden-fixtures.md)
- Guide: [§3](../architecture/implementation-guide.md)
- Supersedes: none

## Context

The page file and the write-ahead log are separate versions. A reader has to reject a file it cannot interpret, including a file written by a newer build that set an incompatible feature bit. Parent pointers are still stored in B+ tree nodes; removing them is a later format change because splits still walk the stored parent.

## Decision

1. The superblock body is format 1.0 (`FORMAT_MAJOR` 1, `FORMAT_MINOR` 0). Bytes after the B-tree root are the write-ahead log generation (`u64`) and a feature-flag word (`u64`). The body ends at offset 116. Any other major, minor, or page size is `UnsupportedVersion`. No earlier superblock was released, so there is no upgrade into 1.0.
2. A feature bit outside `KNOWN_FEATURE_FLAGS` (currently zero) is `UnsupportedVersion`. Compatible bits may be added without a minor bump. An incompatible change bumps the minor, or the major when old readers must not ignore it.
3. The log header is version 2. Bytes 32..40 are the log generation, starting at 1. A reset writes generation + 1 and syncs the header. It does not rewrite frames. Frame checksums cover `generation || header || payload`, so a frame from the previous generation fails its checksum and recovery stops.
4. A log whose version is not 2 is `UnsupportedVersion`. A superblock whose minor is not 0 is `UnsupportedVersion`.
5. `restamp` keeps the log generation and the superblock's recorded log generation when it replaces the file id.
6. Golden bytes for one superblock page live in `crates/pizzlydb-storage/tests/compatibility/`. A test decodes that page and rejects a copy with an unknown feature bit.

## Consequences

- Format 1.0 is the first superblock layout. A different minor is rejected. There is no migration into this version.
- The copy-selection generation in the superblock is not the log generation. Checkpoint stores the log generation that was current when the superblock was sealed. A newer empty log adopts the checkpoint because its recorded LSN is zero.
- B+ tree parent pointers remain. `set_parent` skips a child whose stored parent is already correct, so an unchanged child is not rewritten. Deleting the field is a separate minor bump.

## Alternatives considered

- Overload the superblock copy-selection generation as the log salt. Rejected because copy selection and log reset advance for different reasons.
- Publish the log generation and feature flags as minor 1. Rejected because no earlier superblock was released, so 1.0 is that layout.

## Verification

`cargo test -p pizzlydb-storage --lib superblock::tests wal::tests`. Opening a version-1 log header or a superblock with a feature bit set returns `UnsupportedVersion`.

## References

- [012](../roadmap/012-crash-safe-wal-reset.md)
- [023](../roadmap/023-btree-write-amplification.md)
- [024](../roadmap/024-storage-format-versioning-and-golden-fixtures.md)
