# 019: Bounded write batches and transaction size limits

| Field | Value |
| --- | --- |
| Phase | 1A Storage engine hardening |
| Priority | Must |
| Size | M |
| Crates | `pizzlydb-storage` (`engine`), `pizzlydb-graph` (`batch`) |
| Type | Robustness, resource control |

## Summary

Enforce configurable upper bounds on the memory and WAL footprint of a single write batch: staged entries, staged bytes, dirty pages in the overlay, and WAL bytes. A batch that exceeds a limit fails with a typed `TransactionTooLarge`, stating which limit it hit, instead of exhausting memory. Each stage of the commit pipeline is accounted for so that the limits are honest.

## Motivation

A batch is currently kept entirely in memory three times. The memory is never bounded, so any large operation can OOM-kill the server, violating guide invariant C9 (boundedness) and the transaction limits of §4.4.

| Copy | Contents |
| --- | --- |
| Staged map | The `BTreeMap` of key/value changes |
| Overlay | A `HashMap` of 8 KiB page images produced by replaying the stage into the B-tree |
| WAL | The committed images, in memory until [013](013-disk-resident-wal-index.md) |

These operations write everything in a single batch, so their memory grows with the dataset:

- `GraphStore::create_index` (`crates/pizzlydb-graph/src/index.rs`, around lines 270–307)
- `bulk_load`
- `salvage`

## Current State

- `crates/pizzlydb-storage/src/engine.rs`: `WriteBatch` stages an unbounded `BTreeMap`, and `publish` replays it into an `Overlay` (`HashMap<PageId, page image>`) before `pager.commit_pages`.
- `crates/pizzlydb-graph/src/batch.rs`: `GraphBatch` adds another staged map and an undo log.
- There is no size accounting or limit anywhere.

## Scope

### In scope

- A `BatchLimits { max_entries, max_staged_bytes, max_dirty_pages, max_wal_bytes }` struct configured at open, with documented defaults.
- Accounting at staging time (entries and bytes) and during publish (overlay pages). Exceeding a limit aborts the batch cleanly and leaves the handle open, because this is not a poisoning error.
- `StorageError::TransactionTooLarge { limit, observed }`, mapped by later layers to `RESOURCE_EXHAUSTED` (non-retryable without a change).
- Byte-level stats exposed for observability ([102](102-metrics-foundation.md)).
- Graph-layer accounting so that `GraphBatch` overhead is included.
- Documentation: index builds, bulk load, salvage, and namespace drop must not rely on a single unbounded batch. They move to the chunked designs in [044](044-online-index-build-and-drop.md), [110](110-bounded-memory-bulk-loader.md), [109](109-repair-procedures-and-salvage-hardening.md), and [045](045-namespace-registry-and-lifecycle.md).

### Out of scope

- Spilling large transactions to disk. Very large transactional writes are a non-goal; bulk paths use dedicated builders.

## Dependencies

- [010](010-storage-error-taxonomy-fidelity.md): error taxonomy.
- [013](013-disk-resident-wal-index.md): WAL bytes are no longer RAM-resident, but the limit still bounds per-commit WAL growth.

## Acceptance Criteria

- [x] A test with `max_dirty_pages = 64` and a batch that needs 65 fails with `TransactionTooLarge`, after which the database remains usable and unchanged.
- [ ] Peak RSS during a maximum-size batch is within a documented factor of the configured limits (benchmark harness assertion).
- [ ] The existing `create_index` path either respects the limit or is explicitly marked as superseded by [044](044-online-index-build-and-drop.md), with a test that documents the current failure.

## Testing and Validation

- Parameterized limit tests for each dimension.
- Fault-injection test: aborting at the limit during publish leaks no pages (`verify_quick`).

## References

- [Guide §4.4 transactions and isolation](../architecture/implementation-guide.md#44-transactions-and-isolation), [§12.2 C9](../architecture/implementation-guide.md#122-mandatory-correctness-properties)
- `crates/pizzlydb-storage/src/engine.rs`, `crates/pizzlydb-graph/src/{batch,index,bulk,salvage}.rs`
