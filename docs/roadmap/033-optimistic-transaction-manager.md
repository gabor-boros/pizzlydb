# 033: Optimistic transaction manager with serializable validation

| Field | Value |
| --- | --- |
| Phase | 1B Transactions and logical commit log |
| Priority | Must (critical path) |
| Size | XL |
| Crates | `pizzlydb-txn` (`snapshot`, `validate`, `sequencer`), `pizzlydb-storage` |
| Type | Feature |

## Summary

Implement the transaction protocol chosen in ADR-004 ([030](030-adr-transaction-isolation-model.md)). Transactions:

- begin at a snapshot
- record point reads and range reads
- buffer their writes
- commit through a single serialized pipeline, which runs these steps in order:
  1. validate the read set against the write sets committed after the snapshot
  2. assign a `CommitSeq`
  3. write the graph keys, mutation log, and commit record in one storage batch
  4. make the batch durable through group commit
  5. acknowledge

## Motivation

This layer turns a key-value engine with snapshots into a serializable database. With it, the writer lock is held only during commit and never during client round trips, so many transactions can execute concurrently while correctness is preserved. Guide §4.4 and Milestone M2 depend on it.

## Current State

- `crates/pizzlydb-txn/src/*.rs`: stubs only.
- Storage offers `begin_write()` (exclusive, returns `Busy`), `snapshot()`, and `WriteBatch::commit`.
- The graph layer holds the writer lock for the whole transaction (`crates/pizzlydb-graph/src/batch.rs`).

## Scope

### In scope

- **Transaction lifecycle.**
  - `TxnManager::begin(TxnOptions { read_only, deadline, limits })` returns a `Txn` bound to snapshot S, the latest committed state.
  - `Txn::get`, `scan_range` (forward and reverse) with read tracking, `put`, `delete`, `delete_range`, and a `merge_add(key, i64)` commutative update that is applied inside the commit section and excluded from validation.
  - `commit() -> Result<CommitSeq, TxnError>` and `rollback()`.
- **Read-set representation.** Point keys plus closed ranges `[start, last_seen_or_end]`. Ranges are coalesced, and memory is bounded by `max_read_set`.
- **Recent-commit write-set history.** An ordered list of `(CommitSeq, write keys, range tombstones)` retained from the oldest active snapshot. It is memory-bounded, and when the bound is hit the oldest transactions abort with `TxConflict { reason: HistoryEvicted }`.
- **Validation.** A transaction aborts with `TxConflict` (retryable) if any commit in (S, now] wrote a key in its read set or inside one of its read ranges.
- **Fast paths.** Read-only transactions do not validate. Single-shot transactions run under the commit lock.
- **Commit pipeline.** It integrates group commit ([020](020-group-commit-and-durability-modes.md)) so that validation and `CommitSeq` assignment happen in order inside the critical section and the durability wait happens outside it. Write sets pass through bounded batches ([019](019-bounded-write-batches.md)).
- **Errors.** `TxConflict`, `TxExpired`, `TransactionTooLarge`, `ReadOnlyViolation`, and `Closed`, each with documented retryability.
- **Statistics hooks.** Commits, aborts by reason, validation time, read-set and write-set sizes, and history size.

### Out of scope

- Graph integration ([035](035-graph-on-transactions.md)), and expiry policy for long readers ([037](037-snapshot-retention-and-transaction-limits.md)).

## Dependencies

- [030](030-adr-transaction-isolation-model.md): ADR-004.
- [031](031-commit-sequence-and-commit-metadata.md) and [032](032-logical-mutation-log.md): `CommitSeq`, the commit record, and the mutation log.
- Storage prerequisites: [016](016-thread-safe-storage-engine.md) (concurrency), [018](018-storage-range-operations.md) (bounded scans), [019](019-bounded-write-batches.md) (bounded batches), and [020](020-group-commit-and-durability-modes.md) (group commit).

## Acceptance Criteria

- [ ] Unit tests cover the classic anomalies at the key-value level, each of which must be prevented: lost update, write skew, read skew, and a range phantom (insert into a scanned range).
- [ ] A concurrency stress test with 32 threads and 1,000,000 transactions on a bank-transfer invariant shows the total preserved, and a range-count invariant holds.
- [ ] Read-only transactions never abort for conflicts (test).
- [ ] Throughput with 32 threads under low contention is at least 4x the single-writer-lock baseline (benchmark).
- [ ] All transaction memory is bounded by the configured limits (assertions).

## Testing and Validation

- Property tests against a serial-execution reference.
- `loom` models of the commit section.
- Full anomaly checking in [036](036-isolation-verification-suite.md).

## References

- [Guide §4.4](../architecture/implementation-guide.md#44-transactions-and-isolation), [§16 WP-09/WP-10](../architecture/implementation-guide.md#16-first-implementation-slice--concrete-work-packages)
- [ADR-004](030-adr-transaction-isolation-model.md)
- `crates/pizzlydb-txn/src/{snapshot,validate,sequencer}.rs`
