# ADR-003: In-house page engine with graph data as ordered keys in one B+ tree

- Status: Accepted
- Date: 2026-11-07
- Owner issue: [001](../roadmap/001-design-record-and-baseline-adrs.md)
- Guide: [§4.1 initial storage technology](../architecture/implementation-guide.md#41-initial-storage-technology), [§3.4 initial physical index set](../architecture/implementation-guide.md#34-initial-physical-index-set), [§11.3 external engine evaluation](../architecture/implementation-guide.md#113-external-engine-evaluation)
- Supersedes: none

## Context

The guide requires an in-house page engine behind a narrow `StorageEngine` boundary and says that a different substrate requires an ADR (§4.1, §11.3). It also lists the logical index set that graph data needs (§3.4).

Graph data can be stored in two ways: in purpose-built page layouts for each structure (identifier directories, slotted record heaps, adjacency blocks, property column segments, and membership bitmaps), or as keys in one ordered map. This ADR records the choice of engine and the choice of layout. Both are already implemented.

## Decision

1. **Build the storage engine in-house.** `pizzlydb-storage` implements every layer itself:
   - a VFS trait with memory, POSIX, and fault-injecting backends (`Vfs`, `MemoryVfs`, `PosixVfs`, `FaultInjecting` in `vfs.rs`, `vfs_memory.rs`, `vfs_posix.rs`);
   - 8 KiB pages with a CRC-32C checksum in every page header (`page.rs`, `checksum.rs`);
   - two alternating superblocks (`superblock.rs`);
   - a page-image write-ahead log (`wal.rs`);
   - a bounded buffer pool with CLOCK eviction and RAII pins (`pager.rs`);
   - a bitmap allocator that reuses a retired page only after every snapshot that could read it has closed (`space.rs`, `maintain.rs`);
   - a bytewise B+ tree with overflow pages for large values (`btree.rs`).
2. **Expose only ordered bytes.** The public surface is `Storage`, `StorageEngine`, `ReadView`, `Scan`, `WriteBatch`, `Durability`, and `StorageError`. Page kinds, WAL frames, and tree nodes stay private to the crate, and `pizzlydb-storage` does not depend on `pizzlydb-core`.
3. **Store all graph state as keys in the one B+ tree.** Every key starts with a namespace prefix built by `pizzlydb_core::KeyBuf`, followed by a kind byte defined in `crates/pizzlydb-graph/src/keys.rs`. The kinds cover identifier counters, node and edge records, catalog names in both directions, adjacency, node labels, label and edge-type membership, property declarations, declared and undeclared property cells, index definitions and targets, and unique and non-unique index entries. Byte order of the encoded key is the logical order; `pizzlydb-core` encodes integers, floats, and identifiers so that unsigned byte comparison preserves it.
4. **Commit related keys in one batch.** A `GraphBatch` stages a record, both adjacency directions (`adjacency::link`), label membership, property cells, and index entries, and commits them as one storage batch. Either all of them become visible or none do.
5. **No structure-specific page layouts.** There are no graph page kinds, no identifier directory pages, no slotted heaps, no adjacency delta chains, no property column segments, and no membership bitmaps. The superblock and the state page hold engine metadata only, such as the allocator page, the free-space map, and the B+ tree root. Node and edge counters are ordinary keys.

## Consequences

- One atomic commit path covers records, adjacency, and indexes, which is what invariant C1 needs.
- Because all durable state lives in one map, one physical snapshot is one consistent backup. The roadmap relies on this for online backup ([105](../roadmap/105-online-consistent-backup.md)).
- An identifier lookup is one point read, and a neighbor scan is one key-range scan. Traversal locality depends on B+ tree leaf order, not on dedicated adjacency blocks.
- Costs that structure-specific layouts would avoid need explicit work:
  - write amplification from internal-node parent pointers ([023](../roadmap/023-btree-write-amplification.md));
  - a maximum key length of 512 bytes (`btree::MAX_KEY_LEN`);
  - no columnar scan path for analytics, which the guide lists as a non-goal (§1.3).
- The current engine has limits that this decision does not fix. Other roadmap issues own them:
  - one allocator bitmap page caps a database at about 510 MiB ([014](../roadmap/014-scalable-space-allocator.md));
  - the engine is single-threaded ([016](../roadmap/016-thread-safe-storage-engine.md));
  - WAL pruning is not crash-safe ([012](../roadmap/012-crash-safe-wal-reset.md));
  - durability offers only `Full` and `None` ([020](../roadmap/020-group-commit-and-durability-modes.md)).
- The on-disk format is version 1.0, the page header is version 1, and the WAL is version 1 (`PZLW`). It is not frozen yet. [024](../roadmap/024-storage-format-versioning-and-golden-fixtures.md) owns the versioning policy and the format freeze.

## Alternatives considered

- **Embed an existing key-value engine.** Such an engine owns its file format, its I/O path, and its recovery. pizzlydb could not inject faults through its own VFS or tie page reuse to its own snapshot horizon without modifying that engine. Engines built on `mmap` conflict with [ADR-001](0001-rust-workspace-and-unsafe-policy.md) and with I/O errors that the VFS reports explicitly. Engines reached through a C or C++ library add a foreign-function boundary.
- **Purpose-built page layouts for each structure.** They give better scan locality for adjacency and declared properties, but each one needs its own page kinds, verifier, compaction, and crash tests. One ordered map with atomic batches meets the same invariants with one structure.
- **Use a separate tree per structure.** A tree per index or per adjacency direction would need a multi-root commit protocol to keep invariant C1. One tree commits every structure with one root update.

Revisit this decision only with benchmark evidence from [005](../roadmap/005-benchmark-harness-and-baselines.md) or [219](../roadmap/219-benchmark-suite-and-slos.md) that shows the key layout failing a guide workload.

## Verification

Run the storage and graph suites from the repository root:

```shell
cargo test --locked -p pizzlydb-storage -p pizzlydb-graph
```

- `engine_batch.rs` (`memory_one_batch_is_atomic_across_crash`) and `engine_crash.rs` (`memory_every_log_prefix_is_a_committed_batch_prefix`) in `crates/pizzlydb-storage/tests/` check that a batch is atomic across crashes.
- `engine_snapshot.rs` (`memory_snapshot_keeps_the_sequence_from_before_commit`) checks that a snapshot keeps reading one committed sequence.
- `crates/pizzlydb-graph/tests/catalog_adjacency.rs` (`every_edge_appears_once_in_each_direction`) checks that both adjacency directions are written together.
- `pizzlydb_graph::verify_full` checks that the records, adjacency, membership, and index entries of one namespace agree.

## References

- `crates/pizzlydb-storage/src/lib.rs`, `crates/pizzlydb-graph/src/keys.rs`, `crates/pizzlydb-graph/src/directory.rs`
