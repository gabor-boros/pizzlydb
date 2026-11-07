# 032: Logical mutation log, atomic with each commit

| Field | Value |
| --- | --- |
| Phase | 1B Transactions and logical commit log |
| Priority | Must (invariant S2, critical path) |
| Size | L |
| Crates | `pizzlydb-core` (`mutation` types), `pizzlydb-txn` (`log`), `pizzlydb-storage` |
| Type | Feature, on-disk format |

## Summary

Persist an ordered, versioned, deterministic log of the logical graph mutations of each committed transaction, keyed by `CommitSeq` and written in the same storage batch as the graph changes and the commit record. Provide three things on top of it:

- a tail API for consumers: incremental maintenance, subscriptions, archiving, and audit
- a registry of retention floors that protects consumer positions
- truncation below the minimum floor

## Motivation

Invariant S2 (guide §4.3) says that no committed graph mutation exists without a corresponding durable commit-log record, and no visible log record exists without its graph mutation. Invariant C1 (guide §12.2) requires the log entry to commit atomically with the base records, adjacency, and indexes. Incremental maintenance (§6), subscription replay (§7), point-in-time restore ([118](118-log-archiving-and-point-in-time-restore.md)), and audit all depend on the log. The storage WAL cannot serve this purpose: it holds physical page images, it is truncated at checkpoint, and it contains no logical information.

## Current State

- `crates/pizzlydb-txn/src/log.rs`: a doc-comment stub.
- No mutation types exist in any crate.
- Graph writes in `crates/pizzlydb-graph/src/{record_store,catalog,property,index}.rs` stage keys directly, without emitting logical records.

## Scope

### In scope

- **Mutation types.** `pizzlydb-core::mutation` contains `GraphMutation`, a pure-data enum. Placing it in core lets graph (the producer) and incremental (the consumer) share it without depending on each other. Its variants cover:
  - node create and delete
  - edge create and delete, including endpoints and type
  - label add and remove
  - property set and clear, with the old and new value
  - catalog name create and rename
  - declaration change
  - index create, ready, and drop
  - namespace create and drop

  Each record carries its `NamespaceId`.
- **Before-images.** Deletes and overwrites carry before-images, so that consumers can retract without as-of reads. This is the default recommendation, and the alternative is settled by ADR [060](060-adr-incremental-execution-engine.md). The log-size cost is documented and bounded by the transaction limits ([019](019-bounded-write-batches.md)).
- **Encoding.** Explicitly versioned and deterministic: a custom binary format, or `postcard` with a fixed schema version, with no `HashMap` iteration order anywhere. Large transactions are chunked, as `sys/log/<seq>/<chunk>`, to stay within `MAX_VALUE_LEN`.
- **Write contract.** The transaction layer writes the log records, the commit record ([031](031-commit-sequence-and-commit-metadata.md)), and the graph keys in one storage batch.
- **Tail API.** `LogReader::read(after: CommitSeq, max_records, max_bytes)` returns ordered batches with commit boundaries preserved. A notification mechanism (condition variable or channel) wakes consumers on new commits.
- **Retention floors.** A `RetentionFloors` registry, durable for durable consumers and in memory for ephemeral ones. Truncation deletes `< min(floors)` with `delete_range` ([018](018-storage-range-operations.md)) in bounded chunks, and a maximum retention bound keeps an abandoned floor from pinning the log forever (policy in [114](114-retention-management-and-inspection.md)).
- ADR-005, part 2: the log format and the retention contract.

### Out of scope

- Producing records from the graph layer ([035](035-graph-on-transactions.md)), and archiving ([118](118-log-archiving-and-point-in-time-restore.md)).

## Dependencies

- [031](031-commit-sequence-and-commit-metadata.md): `CommitSeq` and the system key space.
- [018](018-storage-range-operations.md): bounded scans and range delete for tailing and truncation.

## Acceptance Criteria

- [ ] Encoding round-trip and determinism: property test plus a fuzz target ([006](006-fuzzing-infrastructure.md)).
- [ ] Atomicity under crash simulation: the log record for `CommitSeq` N exists if and only if commit N is durable.
- [ ] The tail API delivers every record exactly once and in order, across a reader restart from a saved position.
- [ ] Truncation respects every registered floor, and a test that registers a floor and truncates proves it.
- [ ] ADR-005 part 2 is merged, with an example record for each mutation kind.

## Testing and Validation

- A crash-simulation extension, and a model test of tail, register floor, truncate, and resume.

## References

- [Guide §4.3 (S2)](../architecture/implementation-guide.md#43-atomic-commit-record), [§12.2 C1](../architecture/implementation-guide.md#122-mandatory-correctness-properties), [§16 WP-08](../architecture/implementation-guide.md#16-first-implementation-slice--concrete-work-packages)
- `crates/pizzlydb-txn/src/log.rs`
