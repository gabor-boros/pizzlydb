# 030: ADR-004: transaction, isolation, and commit-sequence model

| Field | Value |
| --- | --- |
| Phase | 1B Transactions and logical commit log |
| Priority | Must (decision gate) |
| Size | M |
| Crates | `docs/adr/` (decision), affects `pizzlydb-txn`, `pizzlydb-graph`, `pizzlydb-storage` |
| Type | Architecture decision |

## Summary

Decide and document how pizzlydb provides serializable transactions on top of the single-commit-pipeline storage engine. The decision covers six points:

- the concurrency-control protocol
- read-set granularity and phantom protection
- the relationship between `CommitSeq` and the storage LSN
- the separation between user commits and system commits (maintenance state, cursors)
- which layer owns transactions: graph on top of txn on top of storage, per guide §11.1
- the limits that keep validation bounded

## Motivation

The guide requires serializable isolation through MVCC with optimistic validation (§4.4). The code implements neither as a reusable layer:

- `GraphBatch::begin` takes the storage writer lock and holds it for the whole transaction.

Holding the writer lock for the whole transaction does make writers serial, and therefore serializable. In a networked server, though, one slow client holding an interactive transaction blocks every writer in the system. The code also has these gaps:

- There is no `CommitSeq`, no logical log, and no validation.
- `pizzlydb-txn` is an empty skeleton.
- The graph crate goes directly to storage, which contradicts guide §11.1.

The protocol has to be chosen before [031](031-commit-sequence-and-commit-metadata.md)–[037](037-snapshot-retention-and-transaction-limits.md) can be built.

## Current State

- `crates/pizzlydb-graph/src/batch.rs` (around lines 93–103): `GraphBatch::begin` calls `storage.begin_write()` (which returns `Busy` if a writer exists) and `storage.snapshot()`.
- `crates/pizzlydb-storage/src/engine.rs`: one `WriteBatch` at a time, plus physical snapshots registered at the committed LSN.
- `crates/pizzlydb-txn/src/{log,sequencer,snapshot,validate}.rs` are one-line stubs. `crates/pizzlydb-core/src/sequence.rs` is empty.
- `crates/pizzlydb-graph/Cargo.toml` depends on `pizzlydb-txn` but does not use it.

## Scope

### In scope

The ADR compares these options with evidence (complexity, abort rate, latency, implementation risk):

- **(A)** The status quo: a pessimistic global writer, with only non-interactive transactions over the network.
- **(B)** Optimistic transactions over physical snapshots with a serialized validation-and-commit step, using backward validation of read keys and ranges against the write sets committed after the snapshot.
- **(C)** Serializable Snapshot Isolation (SSI) with rw-antidependency tracking.
- **(D)** Two-phase locking with key-range locks.

**Recommendation to evaluate:** option (B), with these refinements:

- Read-only transactions are never validated; they serialize at their snapshot.
- Single-request transactions may run directly inside the commit section, with no validation.
- Read sets record point keys and the actual `[start, last-seen]` ranges of bounded scans ([018](018-storage-range-operations.md)).
- The recent-write-set history is retained from the oldest active snapshot onward; a transaction older than the retained history aborts with a retryable conflict.

Because `CommitSeq` order equals acknowledgement order and every new snapshot starts at the latest commit, option (B) also gives strict serializability. The ADR must confirm or refute that argument.

The ADR also defines:

- The versioning model for as-of reads. Guide §4.2 asks for graph records and index entries that are logically versioned by `CommitSeq`, and it states that pager snapshots and logical MVCC do not substitute for each other. This roadmap's as-of reads ([061](061-as-of-snapshots-at-commit-sequence.md)) instead retain physical page versions at registered protected floors. The ADR must either adopt logical record versions or record a superseding decision with evidence: retention cost, write amplification, compaction interaction, the needs of time-travel queries ([906](906-time-travel-queries.md)), and how guide work package WP-05 ("as-of queries return the correct records at each commit sequence") is satisfied.
- `CommitSeq` semantics: dense, assigned under the commit lock, durable in the same storage commit, never reused, and continuous across crash and restore.
- That system commits (incremental state, cursors, maintenance) do not consume a `CommitSeq` and are not logged as graph mutations.
- How `CommitSeq` maps to the storage LSN.
- Which keys are excluded from read sets: id allocation ([034](034-id-allocation-and-identity-policy.md)) and commutative statistics ([046](046-graph-statistics.md)).
- The limits: lifetime, read-set size, write-set size, and retained-history memory.
- The error contract: `TX_CONFLICT` (retryable), `TX_EXPIRED`, and `RESOURCE_EXHAUSTED`.

### Out of scope

- Implementation; issues 031–037 implement the decision.
- Distributed transactions, which are a non-goal for v1.

## Dependencies

- [001](001-design-record-and-baseline-adrs.md): the ADR home and process.

## Acceptance Criteria

- [ ] ADR-004 is accepted in `docs/adr/`, with the alternatives considered, consequences, and a falsifiable claim: "every committed history is strictly serializable at the key and key-range level".
- [ ] The ADR names the anomalies that must be tested in [036](036-isolation-verification-suite.md): G0, G1a, G1b, G1c, G-single, G2-item, G2 phantoms, lost update, and write skew.
- [ ] The ADR specifies the crate dependency direction graph → txn → storage, and states the plan for the graph crate to stop calling `Storage::begin_write` directly ([035](035-graph-on-transactions.md)).

## Testing and Validation

- Architecture review sign-off by at least one reviewer other than the author.
- A small prototype or a TLA+/P model of the validation protocol is recommended, and its outcome is recorded in the ADR.

## References

- [Guide §4.4 transactions and isolation](../architecture/implementation-guide.md#44-transactions-and-isolation), [§11.1 dependency discipline](../architecture/implementation-guide.md#111-dependency-discipline)
- Kung and Robinson, "On Optimistic Methods for Concurrency Control" (1981). Cahill, Röhm, and Fekete, "Serializable Isolation for Snapshot Databases" (2008). Adya, "Weak Consistency" (1999).
