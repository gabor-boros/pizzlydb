# 031: `CommitSeq` and durable commit metadata

| Field | Value |
| --- | --- |
| Phase | 1B Transactions and logical commit log |
| Priority | Must (critical path) |
| Size | M |
| Crates | `pizzlydb-core` (`sequence`), `pizzlydb-txn` (`sequencer`), `pizzlydb-storage` (commit hook) |
| Type | Feature |

## Summary

Introduce `CommitSeq`, the logical commit order that every higher layer refers to: snapshots, logs, incremental frontiers, subscriptions, backups, and resume tokens. Persist a commit record for each committed transaction, atomically with its effects. Recover the frontier on open.

## Motivation

The guide treats `CommitSeq` as the universal logical clock (§4.3, WP-02), and it states that a physical snapshot sequence is not a `CommitSeq`. Storage LSNs advance on every storage commit, including future system commits, so they cannot serve as a user-visible order. Today no logical sequence exists at all: `crates/pizzlydb-core/src/sequence.rs` is empty.

## Current State

- `crates/pizzlydb-core/src/sequence.rs`: empty module.
- `crates/pizzlydb-txn/src/sequencer.rs`: a doc-comment stub.
- `crates/pizzlydb-storage/src/engine.rs`: `STATE_PAGE_ID = 4` stores the root and high-water mark for each commit, and LSNs are assigned in `pager.commit_pages`.

## Scope

### In scope

- A core newtype `CommitSeq(NonZeroU64)` with `next()` (checked overflow), big-endian key encoding, `Display`, and serde-free binary encoding. Also `CommitSeq::ZERO_FRONTIER`, meaning "before any commit", as a separate type or `Option`.
- A reserved *system key space* that cannot collide with graph keys: the all-zero 16-byte `NamespaceId` prefix, which `NamespaceId` already treats as invalid, followed by a system kind byte. Document it in the key-layout docs.
- Commit record `sys/commit/<seq>` containing the storage LSN, a wall-clock timestamp (UTC, microseconds), a transaction id, the namespaces touched, a mutation count, and reserved fields for the principal and the idempotency key. The record is versioned.
- `sys/commit_frontier`: the last durable `CommitSeq`, written in the same storage batch.
- A storage hook so that the transaction layer can learn the LSN being assigned inside the serialized commit section (for example `WriteBatch::commit_with(|lsn| ...)`), or an equivalent mechanism recorded in ADR-005.
- Recovery: on open, the frontier equals the last durable commit record. Gaps are impossible by construction, and a check reports `Corrupt` if a gap is ever found.
- ADR-005, part 1: `CommitSeq` semantics and the mapping to the LSN.

### Out of scope

- Logical mutation records ([032](032-logical-mutation-log.md)) and as-of snapshots ([061](061-as-of-snapshots-at-commit-sequence.md)).

## Dependencies

- [030](030-adr-transaction-isolation-model.md): the decision on `CommitSeq` semantics.
- [016](016-thread-safe-storage-engine.md): the commit section is serialized and thread-safe.

## Acceptance Criteria

- [ ] `CommitSeq` is dense and strictly increasing across 1,000,000 commits, crash and reopen cycles, and checkpoints (property test with crash simulation).
- [ ] The commit record and the transaction's effects are atomic: across a crash, either both are visible or neither is (crash enumeration).
- [ ] The system key space cannot be written through the graph API (a test with namespace zero fails).
- [ ] ADR-005 part 1 is merged.

## Testing and Validation

- Unit tests for encoding order, overflow, and the frontier.
- A crash-simulation suite extension.

## References

- [Guide §4.3 atomic commit record](../architecture/implementation-guide.md#43-atomic-commit-record), [§16 WP-02](../architecture/implementation-guide.md#16-first-implementation-slice--concrete-work-packages)
- `crates/pizzlydb-core/src/{sequence,id}.rs`, `crates/pizzlydb-storage/src/engine.rs`
