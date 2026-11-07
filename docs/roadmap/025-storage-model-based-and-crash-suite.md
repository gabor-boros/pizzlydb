# 025: Storage model-based and crash-consistency suite (exit gate for Phase 1A)

| Field | Value |
| --- | --- |
| Phase | 1A Storage engine hardening |
| Priority | Must (milestone gate) |
| Size | L |
| Crates | `pizzlydb-storage`, `pizzlydb-testkit`, `tests/model-based/`, `tests/crash-recovery/` |
| Type | Verification |

## Summary

A comprehensive, permanent test suite proves that the hardened storage engine behaves like an ordered map with snapshots under every combination of these operations: concurrent readers, writes, range deletes, checkpoints (manual and automatic), crashes at every VFS operation with partial writeback, disk-full, I/O errors, reopen, and read-only open. Passing it defines Milestone M1 ("Durable, concurrent, bounded storage engine").

## Motivation

Each Phase 1A issue tests its own change, and this gate tests the interactions between them. Interactions are where durability bugs hide: checkpoint during group commit, crash during reset with a pinned reader, disk-full during range delete. Guide invariants C3 (durability), S1 (a snapshot read sees exactly the committed state at that snapshot), and C9 (boundedness) must be demonstrated, not argued.

## Current State

- Storage has 149 unit tests and these integration suites: `backup` (6), `engine_batch` (10), `engine_checkpoint` (6), `engine_crash` (8), and `engine_snapshot` (8). They are example-based and run under the all-or-nothing crash model.
- `tests/model-based/` and `tests/crash-recovery/` are empty.

## Scope

### In scope

- **State-machine test (proptest).** The operations are:
  - `begin_read`, `get`, `scan_range` (forward and reverse), `end_read`
  - `write` (put, delete, delete_range), `commit` (each durability mode)
  - `checkpoint`, `crash(seed)`, `reopen`, `open_read_only`
  - `inject_fault(kind)`

  The reference is a `Vec<BTreeMap>` of committed versions plus a durability frontier per mode.
- **Concurrency test:** N reader threads checking snapshot consistency and monotonicity while writers commit and the checkpointer runs. It runs under ThreadSanitizer nightly.
- **Exhaustive crash enumeration** for small histories, plus random sampling for large ones.
- **Resource-bound assertions:** RSS and WAL size stay within the configured limits during a one-hour soak in the nightly job.
- Seeds for every failure, and a `cargo xtask crash-repro <seed>` helper.

### Out of scope

- Graph semantics ([047](047-graph-integrity-suite.md)) and real-device fault testing ([119](119-operational-fault-injection-suite.md)).

## Dependencies

- Test infrastructure: [003](003-testkit-reference-model-and-property-testing.md) and [004](004-crash-consistency-simulation-vfs.md).
- Storage issues under test:
  - durability and WAL: [012](012-crash-safe-wal-reset.md), [013](013-disk-resident-wal-index.md), [020](020-group-commit-and-durability-modes.md), [021](021-automatic-checkpointing-and-wal-bounds.md), [022](022-disk-full-and-io-error-safety.md)
  - space and format: [014](014-scalable-space-allocator.md), [023](023-btree-write-amplification.md), [024](024-storage-format-versioning-and-golden-fixtures.md)
  - access and concurrency: [015](015-exclusive-ownership-and-read-only-open.md), [016](016-thread-safe-storage-engine.md), [017](017-concurrent-page-table-and-zero-copy-reads.md), [018](018-storage-range-operations.md), [019](019-bounded-write-batches.md)

## Acceptance Criteria

- [ ] On each PR, the state-machine suite runs 2,000 cases (under 10 minutes). The nightly job runs 200,000 cases and has shown no failures for 14 consecutive nights.
- [ ] Crash enumeration covers every VFS operation index for histories of up to 50 operations, with 100 or more persistence subsets per point.
- [ ] Zero acknowledged-commit losses, zero failures to reopen, and zero `verify_quick` failures across all runs.
- [ ] A soak test shows bounded RSS and WAL size (graphs attached to the milestone report).
- [x] A milestone report in `docs/milestones/M1.md` summarizes the evidence.

## Testing and Validation

- This issue is the test. Meta-validation: re-introducing the prune bug ([012](012-crash-safe-wal-reset.md)) or a dropped sync makes the suite fail within the PR budget.

## References

- [Guide §12.1 and §12.2](../architecture/implementation-guide.md#12-verification-and-release-quality-gates)
- `crates/pizzlydb-storage/tests/*.rs`
