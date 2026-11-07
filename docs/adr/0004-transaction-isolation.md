# ADR-004: Transaction isolation, concurrency control, and conflicts

- Status: Accepted
- Date: 2026-11-07
- Owner issue: [030](../roadmap/030-adr-transaction-isolation-model.md)
- Guide: [§4.2](../architecture/implementation-guide.md#42-versioning-model), [§4.4](../architecture/implementation-guide.md#44-transactions-and-isolation), [§11.1](../architecture/implementation-guide.md#111-dependency-discipline)
- Supersedes: the per-record logical version sentence in guide §4.2, for v1 as-of reads

## Context

`Storage::begin_write` allows one write batch. `GraphBatch::begin` used to hold that batch for the whole transaction. That is serializable, and it blocks every other writer for the length of a client round trip.

Guide §4.4 requires serializable multi-entity transactions. Guide §11.1 requires the dependency direction graph → txn → storage. The storage log sequence is not a user-visible commit order: system maintenance will also commit pages.

## Decision

1. Concurrency control is optimistic validation over a physical storage snapshot (option B). A transaction reads the latest committed snapshot, buffers writes, and commits through one pipeline. The pipeline validates, assigns a `CommitSeq`, writes keys and the logical log in one storage batch, and acknowledges after the configured durability wait.
2. Read sets contain point keys and the half-open predicate of each bounded scan. Overlapping ranges are merged. Validation is backward: a commit aborts when a commit after its snapshot wrote a key in that set or inside one of those ranges.
3. Read-only transactions are not validated. They serialize at their snapshot.
4. A single-shot transaction runs inside the commit lock and is not validated. No other commit can land while that lock is held.
5. `CommitSeq` is dense, starts at 1, is assigned under the commit lock, is stored in the same storage batch as the commit's effects, and is never reused. The order of acknowledgement is the `CommitSeq` order. A new transaction snapshots the latest commit. That is strict serializability at the key and key-range level: every committed history is strictly serializable at the key and key-range level.
6. `CommitSeq` is not the storage LSN. The commit record stores both. System commits (id-block watermarks flushed alone, retention truncation, maintenance) do not take a `CommitSeq` and are not graph mutations.
7. Id allocation and commutative `merge_add` counters are not part of the read set. They are applied under the commit lock.
8. As-of reads keep physical page versions at a registered snapshot floor. Records are not stored once per `CommitSeq`. The logical mutation log is the per-commit history. This supersedes the guide §4.2 sentence that each graph record is logically versioned by `CommitSeq`, until time-travel queries ([906](../roadmap/906-time-travel-queries.md)) require a different design. Pager snapshots and the logical log are different mechanisms and do not substitute for each other.
9. A transaction has limits on lifetime, idle time, snapshot age, read-set entries, write-set entries, and retained validation history. Crossing a size limit returns `TransactionTooLarge`. Lifetime and idle time return `Expired`. A reaped snapshot returns `SnapshotTooOld`. A validation failure returns `Conflict`, which is retryable.
10. The graph crate reaches storage writes through `pizzlydb-txn`. It does not call `Storage::begin_write`.

Anomalies that the isolation suite must detect: G0, G1a, G1b, G1c, G-single, G2-item, G2 phantoms, lost update, and write skew.

## Consequences

- Interactive transactions no longer hold the storage writer. The writer lock is held only while a batch is applied.
- A transaction older than the retained write-set history aborts with a retryable conflict instead of reading an incomplete history.
- As-of queries at a `CommitSeq` wait on [061](../roadmap/061-as-of-snapshots-at-commit-sequence.md), which pins a physical snapshot. The log already has the mutations of each commit.
- Guide §4.2 no longer requires a second logical copy of every record for v1.

## Alternatives considered

- (A) Keep the global writer and allow only non-interactive transactions. Rejected. One slow client blocks every writer. The implementation is smaller, and the abort rate is zero, but the latency tail is the slowest client.
- (C) Serializable snapshot isolation with rw-antidependency tracking. Rejected for v1. It allows older snapshots and more concurrency, and it needs a second conflict graph. Backward validation already gives strict serializability because every snapshot is the latest commit and the commit order is the acknowledgement order.
- (D) Two-phase locking with key-range locks. Rejected. Deadlock handling and lock tables cost more than the validation history, and a lock held across a client round trip reintroduces the blocking problem of (A).

The executable check is the history checker in `pizzlydb-testkit` and the anomaly tests in `pizzlydb-txn`. A separate TLA+ model was not run. Those tests are the prototype: lost update, write skew, and a predicate insert are rejected when validation is on, and a history with validation off reports G2-item.

## Verification

- `cargo test -p pizzlydb-txn --test protocol` covers lost update, write skew, phantoms, read-only commits, and history eviction.
- `cargo test -p pizzlydb-testkit --lib history` covers the anomaly classes on hand-built histories.
- `rg begin_write crates/pizzlydb-graph/src` is empty.

## References

- Kung and Robinson, "On Optimistic Methods for Concurrency Control" (1981)
- Adya, "Weak Consistency" (1999)
- [031](../roadmap/031-commit-sequence-and-commit-metadata.md) through [037](../roadmap/037-snapshot-retention-and-transaction-limits.md)
