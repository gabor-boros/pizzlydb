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
| 7 | namespace id | Namespace registry record: state, logical format version, creation `CommitSeq`, placeholder limits, name, and an optional drop cursor |
| 8 | name bytes | Namespace id for that unique name |
| 9 | namespace + stat code + optional id | Exact counter (`i64`) or a sampled statistic blob |

The graph logical format version stays 1. Namespace ids and statistics use new system kinds inside that version. A namespace id is never reused. Dropping a namespace deletes its graph prefix and its statistic and id-watermark keys in chunks, then removes kinds 7 and 8.

The transaction manager writes kinds 1–6 and stamps the creation `CommitSeq` into a kind 7 record. Graph code writes kinds 7–9.

See [ADR-005](../adr/0005-commit-sequence-and-mutation-log.md).
