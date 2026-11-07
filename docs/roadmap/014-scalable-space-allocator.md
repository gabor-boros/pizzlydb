# 014: Scalable free-space allocator: remove the ~510 MiB database size limit

| Field | Value |
| --- | --- |
| Phase | 1A Storage engine hardening |
| Priority | Must |
| Size | L |
| Crates | `pizzlydb-storage` (`space`, `superblock`, `engine`, `backup`, `verify`) |
| Type | Architecture, on-disk format change |

## Summary

The allocator tracks page allocation in a single bitmap page, so a database can hold at most 65,312 pages × 8 KiB ≈ 510 MiB. Replace it with a multi-page bitmap, or a two-level extent map, that supports at least 2^32 pages (32 TiB at 8 KiB). The design must also keep per-commit allocator cost proportional to the pages it touches, not to the database size.

## Motivation

A production graph database must hold datasets far larger than 510 MiB; the limit makes the engine unusable beyond toy sizes. In addition, `Space` is cloned for every commit, so commit cost grows with the size of the free-space state. Because this is an on-disk format change, it must land before the format freeze ([024](024-storage-format-versioning-and-golden-fixtures.md)).

## Current State

- `crates/pizzlydb-storage/src/space.rs`: `MAX_BITMAP_PAGES = 65_312` (around line 83) is the bit capacity of one bitmap page.
- The constants are `INITIAL_PAGES = 64` and `EXTENT_PAGES = 64`. Allocation hints are lossy.
- The allocator page holds 64 retired pages (`MAX_RETIRE`). Further entries spill to `RetireSpill` pages instead of failing.
- `WriteBatch::publish` in `engine.rs` (around lines 1132–1178) clones the whole `Space` for each commit and writes the allocator, bitmap, and spill pages to the WAL whenever they change.
- The superblock stores the allocator page id, the FSM page id, and the logical page count.

## Scope

### In scope

- Bitmap page chain or directory (for example a root "space map" page listing bitmap pages, each covering a fixed range), sized for at least 2^32 pages.
- Copy-on-write updates that only dirty the bitmap pages whose bits change. No whole-state clone per commit; use a persistent or delta structure in memory.
- Extent-based growth with a configurable extent size, and allocation hints persisted or rebuilt cheaply.
- A two-phase free that remains correct together with the retire list. The invariant: a page that a visible snapshot can still reach is never reused.
- Updates to `verify_quick`, `inspect`, and `backup` (which copies up to the high-water mark).
- A documented maximum database size, plus `StorageError::Full` with context when it is reached.

### Out of scope

- File shrinking and truncation ([113](113-space-reclamation-and-compaction.md)), although the design must allow tail truncation.

## Dependencies

- [004](004-crash-consistency-simulation-vfs.md): the crash simulator, used for crash-safe allocator updates.
- [010](010-storage-error-taxonomy-fidelity.md): error taxonomy.

## Acceptance Criteria

- [ ] A test creates and verifies a database larger than 2 GiB of allocated pages. Sparse files on CI are acceptable, and a nightly job creates at least 16 GiB of real data.
- [ ] Per-commit allocator cost, measured as pages written and CPU, is independent of database size (benchmark from 1 GiB to 16 GiB).
- [ ] Crash enumeration over allocation, retirement, reclaim, and checkpoint leaks no pages and double-allocates none (checked by `verify_quick`).
- [ ] The format version is bumped and the new layout is documented in `docs/` (on-disk layout section).

## Testing and Validation

- Model-based test of the allocator against a `BTreeSet<PageId>` reference under random allocate, retire, reclaim, and crash histories.
- Benchmark curve from [005](005-benchmark-harness-and-baselines.md).

## References

- `crates/pizzlydb-storage/src/space.rs`, `engine.rs`, `superblock.rs`
