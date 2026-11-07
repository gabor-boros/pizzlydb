# 034: Conflict-free id allocation and the identity and tombstone policy (ADR-002, part 1)

| Field | Value |
| --- | --- |
| Phase | 1B Transactions and logical commit log |
| Priority | Must |
| Size | M |
| Crates | `pizzlydb-graph` (`record_store`, `keys`), `pizzlydb-txn`, `pizzlydb-core` (`id`) |
| Type | Architecture, on-disk format |

## Summary

Move node, edge, and catalog id allocation out of the transactional read set, using a durable block-reserving sequence. Gaps are allowed, ids are never reused, and allocation never causes an optimistic-concurrency conflict. The issue also decides three related questions:

- whether and how client-supplied ids are accepted
- whether permanent tombstones are still needed
- how "deleted" is distinguished from "never existed"

## Motivation

Every create operation reads and increments a per-namespace counter key inside its transaction. Under the optimistic protocol of [033](033-optimistic-transaction-manager.md), two concurrent transactions that both create a node therefore always conflict, which reduces write concurrency to one creator at a time and produces constant retries.

Tombstones are permanent, so storage grows with the number of deletes and never shrinks. With server-assigned monotonic ids, "deleted" can be derived from `id <= allocated_high_water && record absent`, which removes the need for tombstones.

Guide §3.1 requires client-supplied ids where authorized, and they are not supported today.

## Current State

- `crates/pizzlydb-graph/src/keys.rs`: counter key kind `0x01` (around lines 7 and 66–70).
- `crates/pizzlydb-graph/src/record_store.rs`: the counter is read and incremented in the batch, and tombstones are written on delete and kept forever (around lines 217 and 379).
- `crates/pizzlydb-core/src/id.rs`: 128-bit ids with zero invalid.

## Scope

### In scope

- **Sequence allocator.** It reserves id blocks (for example 1,024 at a time) and persists only the reserved high-water mark, either in a system commit or piggybacked on any commit. Ids are handed out from memory. After a crash, unused ids in the reserved block are skipped; they are never reused. The allocator is per namespace and per id kind.
- **Identity ADR (ADR-002, part 1).** It decides:
  - The id format: sequential within 128 bits versus a time-ordered UUIDv7-like layout. This trades key locality, information leakage (an entity-count side channel), and B-tree insert patterns.
  - Client-supplied ids: whether they are allowed only within a reserved id range or format, what authorization requires, and how uniqueness is guaranteed (a key-existence check that is part of the read set).
  - Tombstones: either removed for allocator ids (derive `Deleted` from the high-water mark) or kept with a retention and GC policy. The recommendation is to keep them only for client-supplied ids.
- **Graph migration.** Migrate the graph layer to the allocator and remove the counter key from the transactional path.
- **Format.** Changes to the graph logical format version ([024](024-storage-format-versioning-and-golden-fixtures.md)).

### Out of scope

- Physical reclamation of tombstones ([113](113-space-reclamation-and-compaction.md)).

## Dependencies

- [030](030-adr-transaction-isolation-model.md): read-set exclusions.
- [016](016-thread-safe-storage-engine.md): the allocator is shared across threads.

## Acceptance Criteria

- [ ] Concurrent transactions that create nodes in the same namespace never conflict because of id allocation. In a stress test with 32 threads, conflicts due to the counter are 0.
- [ ] Ids are unique and never reused across crash enumeration (property: the union of all ids ever returned contains no duplicates).
- [ ] `get_node` on a deleted id returns the documented `Deleted` versus `NotFound` behavior, with or without tombstones.
- [ ] The ADR is merged, and the key-layout documentation is updated.

## Testing and Validation

- Model-based tests, crash simulation of the block reservation, and a concurrency stress test.

## References

- [Guide §3.1 entities](../architecture/implementation-guide.md#31-entities), [§3.2 integrity](../architecture/implementation-guide.md#32-integrity)
- `crates/pizzlydb-graph/src/{keys,record_store}.rs`
