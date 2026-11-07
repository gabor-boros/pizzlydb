# 002: Fix file identity and document the limits of backup, verify, salvage, and bulk load

| Field | Value |
| --- | --- |
| Phase | 1.0 Foundations |
| Priority | Must |
| Size | M |
| Crates | `pizzlydb-storage`, `pizzlydb-graph` |
| Type | Bug fix, hardening |

## Summary

The offline tools (reclaim, backup, quick and full verify, inspect, salvage, and bulk load) work, but they create databases with a constant or copied file identity and do not state their limits. Give every database they create a fresh identity, document each limitation on the public function, and remove the empty stub modules. The production versions of these tools are planned in Phase 2.

## Motivation

File identity is the only guard against pairing a main file with the wrong WAL. Several defects would cause silent problems:

- `bulk_load` and `salvage` create the destination with `|id| id.fill(1)`, so every database they produce has the same file identity, `0x0101…`.
- `backup` writes the source's file identity into the copy. If an operator places the source's `-wal` file next to the backup, the backup accepts it and replays the frames.
- `verify_quick` and `backup` open the source with `Storage::open`, which is read-write. Recovery can write to the file, and no lock prevents a live process from writing at the same time.
- `salvage` copies a single namespace, which silently drops all other namespaces.
- `bulk_load`, `salvage`, and `create_index` write everything in a single `GraphBatch`, so memory grows with the whole dataset.
- `verify_full` collects every node and edge id into a `Vec`.

## Current State

- `crates/pizzlydb-storage/src/{backup,verify,inspect,maintain}.rs` and `crates/pizzlydb-graph/src/{verify,salvage,bulk}.rs` implement the tools. `crates/pizzlydb-storage/tests/backup.rs` has six tests.
- `crates/pizzlydb-storage/src/meta.rs` contains only a module doc comment, and `crates/pizzlydb-storage/src/salvage.rs` is a one-line stub.
- `bulk.rs` (around lines 156–186) takes `script: &str`, calls `Storage::create(..., |id| id.fill(1))`, and applies all runs inside one `GraphBatch`.
- `salvage.rs` (around lines 170–206) takes one `namespace` parameter, uses the same constant file id, and copies inside one batch.
- `backup.rs` (around lines 78–137) uses `db.file_identity()` for the destination superblocks and WAL.

## Scope

### In scope

- Draw file identity from a CSPRNG for every database these tools create: bulk load, salvage, backup, export, and restore targets. Add `getrandom`, or reuse the caller-supplied generator.
- Give backup copies a fresh file identity. Keep a separate, stable *database lineage id* so later work ([105](105-online-consistent-backup.md)) can record it in the backup manifest. If no lineage field exists yet, record the decision in the format notes and defer the field.
- Add a rustdoc `# Limitations` section to `backup`, `verify_quick`, `salvage`, and `bulk_load` covering: offline only, single namespace, single transaction, and in-memory input. Each limitation links to its Phase 2 follow-up issue.
- Remove or implement the stub modules (`meta.rs`, `storage::salvage`) so that no empty module remains.

### Out of scope

- Online backup ([105](105-online-consistent-backup.md)), read-only open ([015](015-exclusive-ownership-and-read-only-open.md)), multi-namespace salvage ([109](109-repair-procedures-and-salvage-hardening.md)), and bounded-memory bulk load ([110](110-bounded-memory-bulk-loader.md)).

## Dependencies

- None.

## Acceptance Criteria

- [ ] No production code path creates a database with a constant or copied file identity. A test asserts that two bulk loads produce different identities.
- [ ] A test pairs a backup with the source's WAL file and gets `StorageError::Corrupt` (identity mismatch) instead of a silent replay.
- [ ] Every limitation listed above is documented on the public function and linked to its follow-up issue.
- [ ] `cargo test --workspace --locked` and `cargo clippy --workspace --all-targets --locked -- -D warnings` pass.

## Testing and Validation

- New unit tests cover distinct identities, the WAL-swap rejection, and salvage reporting that a namespace was omitted, if the single-namespace limitation is kept.
- Existing suites keep passing: `backup.rs`, the graph verify, salvage, and bulk tests, and the doctests.

## References

- `crates/pizzlydb-storage/src/superblock.rs` (file identity field)
- `crates/pizzlydb-storage/src/backup.rs`, `crates/pizzlydb-graph/src/bulk.rs`, `crates/pizzlydb-graph/src/salvage.rs`
