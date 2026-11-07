# ADR-005: Logical mutation log and `CommitSeq` invariants

- Status: Reserved
- Owner issues: [031](../roadmap/031-commit-sequence-and-commit-metadata.md) (part 1, `CommitSeq` semantics and the mapping to the storage LSN) and [032](../roadmap/032-logical-mutation-log.md) (part 2, log format and retention contract)
- Guide: [§4.2 versioning model](../architecture/implementation-guide.md#42-versioning-model), [§4.3 atomic commit record](../architecture/implementation-guide.md#43-atomic-commit-record), [§14](../architecture/implementation-guide.md#14-mandatory-adrrfc-process)

This number is reserved. No decision is recorded yet. The owner issues replace this file with the ADR, using the [ADR template](template.md).

## Already fixed elsewhere

Guide §4.3 requires one immutable logical mutation record per commit, written in the same atomic storage operation as the graph changes (invariant S2). The code has no commit sequence yet: `crates/pizzlydb-core/src/sequence.rs` and the `pizzlydb-txn` modules contain only module comments. A storage `Lsn` is a physical sequence, not a `CommitSeq`.
