# System keys

System keys start with 16 zero bytes. That prefix is not a valid `NamespaceId`, so a graph key cannot collide with a system key. The next byte is the kind.

| Kind | Suffix | Value |
| --- | --- | --- |
| 1 | none | Last durable `CommitSeq`, 8 big-endian bytes |
| 2 | `CommitSeq` | Version 1 commit record: LSN, UTC microseconds, transaction id, namespaces, mutation count, principal, idempotency key |
| 3 | `CommitSeq` + chunk `u32` | Version 1 logical mutation chunk |
| 4 | floor id `u64` | `CommitSeq` a durable consumer must retain |
| 5 | namespace + id kind | Reserved exclusive id high-water and next id, two big-endian `u128`s |
| 6 | none | Graph logical format version, big-endian `u16`. The version is 1 |

The graph API rejects namespace zero. The transaction manager is the writer of these keys.

See [ADR-005](../adr/0005-commit-sequence-and-mutation-log.md).
