# ADR-004: Transaction isolation, concurrency control, and conflicts

- Status: Reserved
- Owner issue: [030](../roadmap/030-adr-transaction-isolation-model.md)
- Guide: [§4.2 versioning model](../architecture/implementation-guide.md#42-versioning-model), [§4.4 transactions and isolation](../architecture/implementation-guide.md#44-transactions-and-isolation), [§14](../architecture/implementation-guide.md#14-mandatory-adrrfc-process)

This number is reserved. No decision is recorded yet. The owner issue replaces this file with the ADR, using the [ADR template](template.md).

## Already fixed elsewhere

Guide §4.4 sets the target: serializable multi-entity transactions, with snapshot isolation allowed only as an internal stage that is never advertised as serializable. Until this ADR is accepted, the code runs one writer at a time (`Storage::begin_write` returns `StorageError::Busy` while a batch is open) with physical read snapshots.

## Open questions

- The concurrency-control protocol. The roadmap's starting recommendation is optimistic transactions over physical snapshots, with a serialized validate-and-commit step.
- Whether as-of reads use logical record versions by `CommitSeq` (guide §4.2) or retained physical page versions.
