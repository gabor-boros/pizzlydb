# ADR-002: Property-graph identity, value types, key encoding, and schema model

- Status: Accepted
- Date: 2026-11-07
- Owner issues: [034](../roadmap/034-id-allocation-and-identity-policy.md), [040](../roadmap/040-type-system-and-value-model.md)
- Guide: [§3.1](../architecture/implementation-guide.md#31-entities), [§3.2](../architecture/implementation-guide.md#32-integrity)
- Supersedes: none

## Context

Node and edge identifiers are 16 bytes, and zero is invalid. The previous graph path read and incremented a per-namespace counter inside the transaction. Under optimistic validation, two creates in the same namespace conflict on that counter.

The open questions for this part are the id layout, whether clients may supply ids, and how a deleted id differs from an id that was never issued.

## Decision

1. Server ids are sequential big-endian `u128` values with the high bit clear. They start at 1 inside each namespace and id kind. Sequential ids keep the current key order and append-mostly B-tree inserts. A time-ordered layout was rejected: it leaks a timestamp and changes the order existing tests and indexes already use. The entity-count side channel of a sequential id is accepted for v1.
2. The allocator reserves blocks of 1,024 ids in memory and persists the exclusive high-water mark and the next id in the system key space, in a user commit. Allocation does not read or write a graph counter key, so it is not part of the read set. After a crash, allocation resumes at the persisted next id. Ids already handed out are not reused. Gaps are allowed.
3. Client-supplied ids are allowed only when `TxnOptions::allow_client_ids` is set. The embedded API does not infer a principal; the caller has to authorize the option. A client id must have the high bit set, so it cannot collide with the allocator. Uniqueness is a tracked read of the record key.
4. Tombstones stay for allocator ids and for client ids. Deriving "deleted" from `id <= high-water && record absent` is rejected. Optimistic commits from the same block are not ordered by id, so a higher id can become durable while a lower id is still in flight. A reader would observe `Deleted` and later a live record. Tombstone reclamation stays with [113](../roadmap/113-space-reclamation-and-compaction.md).
5. `NotFound` means the record key is absent and there is no tombstone. `Deleted` means the record is a tombstone. That holds for allocator ids and client ids.
6. The graph logical format version is 1, stored at `sys/format`. The counter key kind `0x01` is not written.

Value types, numeric order, names, and over-long index keys are decided in [RFC 0001](../rfcs/0001-type-system.md). The graph logical format version stays 1.

## Consequences

- Concurrent creates in one namespace do not conflict on id allocation.
- Crash recovery can skip ids that were reserved and not committed. Callers must not assume a gap means deletion.
- Tombstones still accumulate until compaction.

## Alternatives considered

- UUIDv7-style ids. Rejected for v1 because of key locality and the existing numeric order.
- Drop tombstones and use the high-water mark. Rejected for the reason in decision 4.
- Client ids anywhere in the id space. Rejected. They would collide with the allocator unless every client id took the commit lock.

## Verification

- `cargo test -p pizzlydb-graph --tests` includes concurrent creates through the allocator and the `NotFound` / `Deleted` cases in `records`.
- `NamespaceId::from_u128(0)` fails, so the graph API cannot write the system prefix.

## References

- [ADR-004](0004-transaction-isolation.md)
- [ADR-005](0005-commit-sequence-and-mutation-log.md)
- [034](../roadmap/034-id-allocation-and-identity-policy.md)
