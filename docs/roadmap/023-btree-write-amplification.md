# 023: Reduce B+ tree write amplification by removing stored parent pointers

| Field | Value |
| --- | --- |
| Phase | 1A Storage engine hardening |
| Priority | Must (on-disk format change, so it must land before the format freeze) |
| Size | L |
| Crates | `pizzlydb-storage` (`btree`) |
| Type | Performance, on-disk format change |

## Summary

Every B+ tree page stores a parent pointer. `write_internal` unconditionally rewrites *every* child of an internal node to refresh that pointer, so one separator change dirties up to the node's full fan-out of pages, each of which becomes an 8 KiB WAL image. Two changes fix this:

1. Remove stored parent pointers, and find parents through a root-to-leaf path stack during mutation.
2. Review the other structural write costs: sibling links, `propagate_min` separator maintenance, and rebalance thresholds.

Measure before and after.

## Motivation

Write amplification determines WAL volume, checkpoint cost, fsync size, and SSD wear. Because the WAL stores full page images, each unnecessary dirty page costs 8 KiB:

- **Leaf splits:** with ascending keys (the graph's counter-assigned node and edge ids), every leaf split triggers `add_child`, which calls `write_internal` on the parent, which rewrites all of the parent's children. That is up to hundreds of pages per split.
- **Minimum-key changes:** the same cascade follows whenever a leaf's minimum key changes, through `propagate_min`.

The parent pointer is also redundant state that `verify` must cross-check, and it gives corruption more surface. Path-stack descent is the standard technique.

## Current State

- `crates/pizzlydb-storage/src/btree.rs`:
  - Page layout: bytes `16..24` hold the parent pointer (around line 19).
  - `write_internal` (around lines 1549–1589) ends with `set_parent(pages, left_child, id)` and `for item in items { set_parent(pages, item.child, id) }`.
  - `set_parent` calls `update_u64`, which loads, rewrites, and stores the child page even when the value is unchanged (around lines 2258–2284).
  - Callers include `commit_internal`, `add_child`, the rebalance paths, and `propagate_min`.

## Scope

### In scope

- A benchmark ([005](005-benchmark-harness-and-baselines.md)) that records pages dirtied per insert for sequential, random, and hub-adjacency key patterns. It runs before any change.
- Removal of the parent field from the node format: mutation descends with a path stack, and `verify` checks the structure top-down.
- At minimum, skip rewrites whose value is unchanged. This is valid only as a stopgap if the format change is deferred, which needs justification.
- An evaluation of separator semantics: storing minimum keys forces `propagate_min` cascades, while classic B+ tree separators (any key k with left < k ≤ right) do not need updating when a leaf's minimum changes. Prefer the variant with fewer writes, and document the choice.
- Reconsider whether left sibling links are needed. Keep right links for scans and reverse links for [018](018-storage-range-operations.md) if justified by measurement.
- Bump the format version.

### Out of scope

- Prefix or suffix compression and node layout changes beyond the header.
- Bottom-up bulk build, which is part of [110](110-bounded-memory-bulk-loader.md); it should reuse the new layout.

## Dependencies

- [005](005-benchmark-harness-and-baselines.md): the benchmark baseline.
- [010](010-storage-error-taxonomy-fidelity.md): error taxonomy.

## Acceptance Criteria

- [ ] For 1,000,000 sequential 24-byte keys, WAL bytes per insert drop by at least 5x compared to the baseline. Report the exact figure for random keys as well.
- [ ] No mutation dirties a page whose bytes are unchanged (asserted in debug builds by comparing before and after images in the overlay).
- [ ] The model-based tree test against `BTreeMap` passes, with random insert, delete, and scan histories of at least 100,000 operations.
- [ ] `verify` detects every structural corruption it detected before, except stale parent pointers, which no longer exist.

## Testing and Validation

- Benchmarks published in `docs/benchmarks/`.
- Property tests and crash-simulation runs over splits and merges.

## References

- `crates/pizzlydb-storage/src/btree.rs` (`write_internal`, `set_parent`, `update_u64`, `propagate_min`, `add_child`)
- Graefe, "Modern B-Tree Techniques" (2011)
