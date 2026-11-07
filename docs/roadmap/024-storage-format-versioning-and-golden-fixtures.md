# 024: Storage format versioning policy and golden compatibility fixtures

| Field | Value |
| --- | --- |
| Phase | 1A Storage engine hardening |
| Priority | Must |
| Size | M |
| Crates | `pizzlydb-storage`, `pizzlydb-graph`, `tests/compatibility/` |
| Type | Compatibility, governance |

## Summary

Define how on-disk formats are versioned and how compatibility is tested. The formats covered are the superblock, page header, WAL header and frame, the allocator, the B-tree node, and the graph key/value layout. The deliverables:

- A major/minor version scheme plus feature flags (incompatible, read-only compatible, and compatible).
- A database-level *logical format version* for the graph key layout, which has no version today.
- Golden fixture databases checked into `tests/compatibility/`, plus cross-architecture open tests.

## Motivation

Issues [012](012-crash-safe-wal-reset.md), [014](014-scalable-space-allocator.md), and [023](023-btree-write-amplification.md) all change the format. Without a written policy and fixtures, format changes after 1.0 could silently break existing databases. Guide §15 ("on-disk format and protocol compatibility policy") and ADR-005/010 depend on this. The graph layer encodes several per-record versions (for example `DEFINITION_VERSION`), but nothing at the database level states which graph key layout a file uses.

## Current State

- `crates/pizzlydb-storage/src/superblock.rs` has `FORMAT_MAJOR = 1` and `FORMAT_MINOR = 0`. `wal.rs` has `VERSION = 1`, and page headers carry a version byte.
- `StorageError::UnsupportedVersion` exists.
- `crates/pizzlydb-graph/src/keys.rs` documents key kinds `0x01` through `0x10`, with no database-level version.
- `tests/compatibility/` is empty, and CI runs only on `ubuntu-latest` (x86-64).

## Scope

### In scope

- A format-policy ADR in `docs/adr/`. It covers what forces a major bump, the feature-flag semantics (unknown incompatible flag means refuse to open; unknown read-only-compatible flag means open read-only), and the pre-1.0 rule (breaking changes allowed but fixtures still updated).
- Superblock fields for the feature flags and the logical format version, stored under a reserved key that the graph layer writes at creation.
- A fixture generator (an `xtask` or test helper) that produces small, deterministic databases covering every page kind, overflow chains, retire spill, a WAL with uncheckpointed frames, and every graph key kind. The fixtures are checked into the repository together with expected-content manifests.
- Tests that open every fixture, read the expected content, and run `verify_quick` and `verify_full`.
- A CI matrix entry for Linux ARM64 that opens x86-64-generated fixtures. Endianness is already big-endian on disk, and this proves it.

### Out of scope

- Upgrade and migration tooling ([112](112-format-upgrade-and-migration.md)).

## Dependencies

- [012](012-crash-safe-wal-reset.md), [013](013-disk-resident-wal-index.md), [014](014-scalable-space-allocator.md), and [023](023-btree-write-amplification.md): the pending format changes, which land first.

## Acceptance Criteria

- [ ] The ADR is merged, and every on-disk structure documents its version field.
- [ ] Golden fixtures exist, and CI fails if a code change alters how a fixture is read.
- [ ] Opening a file with an unknown incompatible feature flag returns `UnsupportedVersion` with a clear message (test).
- [ ] The ARM64 CI job passes.

## Testing and Validation

- Fixture tests and a fuzzed superblock with random feature flags ([006](006-fuzzing-infrastructure.md)).

## References

- [Guide §15 definition of done](../architecture/implementation-guide.md#15-definition-of-done-for-pizzlydb-core)
- `crates/pizzlydb-storage/src/{superblock,page,wal}.rs`, `crates/pizzlydb-graph/src/keys.rs`
