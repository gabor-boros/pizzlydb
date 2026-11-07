# 017: Concurrent page table and zero-copy read path

| Field | Value |
| --- | --- |
| Phase | 1A Storage engine hardening |
| Priority | Must |
| Size | L |
| Crates | `pizzlydb-storage` (`pager`, `btree`, `engine`) |
| Type | Performance |

## Summary

Make page access O(1) expected and allocation-free:

- **Page table:** replace the linear frame scan in `find_cached` with a sharded hash map from `(PageId, version)` to the frame index.
- **Pins:** let B-tree traversal borrow pinned frame bytes instead of copying 8 KiB into a `Vec` per node visit.
- **Cursors:** keep the current leaf pinned between `next()` calls.
- **Readahead:** add optional sequential readahead for long scans.

## Motivation

Two costs currently grow with the cache and with the WAL:

- `find_cached` iterates over every frame on every pin, so a 1 GiB pool (131,072 frames) costs 131,072 comparisons per page access.
- `SnapshotPages::pin` copies the whole page (`pin.bytes().to_vec()`), so a point lookup in a four-level tree allocates and copies 32 KiB, and a scan copies every leaf.

Neither is acceptable for production, and both become contention points once [016](016-thread-safe-storage-engine.md) adds concurrency.

## Current State

- `crates/pizzlydb-storage/src/pager.rs`: `State::find_cached` (around lines 1472–1497) loops over `self.meta`, and `choose_victim` is CLOCK over the same array.
- `crates/pizzlydb-storage/src/engine.rs`: `SnapshotPages::pin` (around lines 1221–1234) copies the bytes into `Vec<u8>`.
- `crates/pizzlydb-storage/src/btree.rs`: `PageRead` returns owned bytes. `scan_from` and `next` (around lines 482 and 1227) re-pin from the root or the sibling on each step.

## Scope

### In scope

- Concurrent page table: sharded `HashMap<PageId, SmallVec<FrameRef>>` with versions per frame, consistent with the WAL version index from [013](013-disk-resident-wal-index.md).
- A `PageRead` API that returns a guard, `PageRef<'_>` derefing to `&[u8; PAGE_SIZE]`, which holds the pin and a shared latch. The B-tree decodes in place (zero-copy node views).
- Scan cursors that hold one leaf pin, release it on advance, and are bounded in total pins.
- Readahead hints for sequential leaf scans (a configurable window).
- Updated `PagerStats`: lookups, hits, misses, evictions, readahead hits, and pin wait time.

### Out of scope

- B-tree structural changes ([023](023-btree-write-amplification.md)).
- Prefix compression (optional; evaluate with benchmarks first).

## Dependencies

- [016](016-thread-safe-storage-engine.md): the concurrency model and latch protocol.
- [005](005-benchmark-harness-and-baselines.md): the baseline against which the improvement is measured.

## Acceptance Criteria

- [ ] Point-lookup latency is independent of buffer-pool size (benchmark: 1,024 to 262,144 frames, flat within 20%).
- [ ] Zero heap allocations per B-tree node visit on the read path, verified with a counting allocator in a benchmark test.
- [ ] Scan throughput improves measurably against the baseline (published numbers), and the total pins held by a scan are at most a documented constant.
- [ ] No pin leaks: the pool returns to zero pins after every test (assertion in the test harness).

## Testing and Validation

- Property test: the new read path returns the same results as the reference `BTreeMap` for random trees and snapshots.
- `loom` or stress tests of a concurrent pin, evict, and lookup.

## References

- `crates/pizzlydb-storage/src/{pager,engine,btree}.rs`
