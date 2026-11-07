# ADR-005: Logical mutation log and `CommitSeq` invariants

- Status: Accepted
- Date: 2026-11-07
- Owner issues: [031](../roadmap/031-commit-sequence-and-commit-metadata.md), [032](../roadmap/032-logical-mutation-log.md)
- Guide: [§4.3](../architecture/implementation-guide.md#43-atomic-commit-record), [§12.2](../architecture/implementation-guide.md#122-mandatory-correctness-properties)
- Supersedes: none

## Context

Guide §4.3 requires one immutable logical mutation record per commit, written in the same storage batch as the graph changes (invariant S2). A storage LSN also advances for system commits, so it cannot be the user-visible order. The logical log has to be trimmable without trimming the page log, and consumers need a floor they can register.

## Decision

### Part 1: `CommitSeq` and the storage LSN

1. `CommitSeq` is `NonZeroU64`. The first commit is 1. `next` fails on overflow. The key encoding is big-endian. `CommitSeq::ZERO_FRONTIER` is `None` and means "before any commit".
2. The commit record `sys/commit/<seq>` stores version 1 fields: the storage LSN, a UTC timestamp in microseconds, a transaction id, the namespaces touched, the mutation count, and empty reserved principal and idempotency fields.
3. `sys/commit_frontier` stores the last durable `CommitSeq` in the same batch.
4. `WriteBatch::commit_with` and `WriteBatch::commit_unacked` reveal the LSN inside the commit section so the record can store it before the batch is published. `CommitAck::acknowledge` performs the durability wait outside the transaction commit lock.
5. On open, the frontier must equal the last commit record and the records from 1 through that frontier must be contiguous. A gap is `Corrupt`.

### Part 2: log format and retention

1. `GraphMutation` in `pizzlydb-core` is the shared record. Deletes and overwrites carry before-images. The encoding is version 1, length-prefixed, and independent of map order.
2. The transaction layer writes `sys/log/<seq>/<chunk>` in the same batch as the graph keys and the commit record. A chunk stays within `MAX_VALUE_LEN`. An empty mutation list still writes one chunk, so commit N has a log record exactly when it is durable.
3. `TxnManager::read_log` returns commits after a cursor, in order, without splitting a commit across a call except when the caller stops between commits.
4. `register_floor` protects a `CommitSeq`. Durable floors are stored at `sys/floor/<id>`. Ephemeral floors are memory only. `truncate_log` deletes chunks below the minimum live floor, in bounded batches. A floor further than `max_pin` commits behind the frontier does not pin the log.
5. System keys use the 16-byte zero prefix, which `NamespaceId` rejects, then a kind byte. The layout is [system keys](../architecture/system-keys.md).

Example records, one per kind, are the variants of `GraphMutation`: node create and delete, edge create and delete, label add and remove, property set and clear, catalog create and rename, declaration change, index create, index ready, index drop, namespace create, and namespace drop. `encode_mutation_list` round-trips each of them.

## Consequences

- Incremental maintenance, subscriptions, and audit can tail `CommitSeq` without reading page images.
- The page WAL can still be truncated at checkpoint. The logical log cannot, until every floor allows it.
- Before-images make the log larger. The write-set limit bounds a single commit.

## Alternatives considered

- Use the storage LSN as `CommitSeq`. Rejected. System commits would appear as user commits and would skip numbers that readers treat as graph commits.
- Store mutations only in the page WAL. Rejected. Checkpoint truncation would destroy the logical history, and page images are not a graph mutation.

## Verification

- `cargo test -p pizzlydb-core --lib mutation` round-trips every mutation kind.
- `cargo test -p pizzlydb-txn --test protocol` checks the frontier across a crash, atomicity of the commit record, the log tail, and floor truncation.

## References

- [ADR-004](0004-transaction-isolation.md)
- [Guide §4.3](../architecture/implementation-guide.md#43-atomic-commit-record)
