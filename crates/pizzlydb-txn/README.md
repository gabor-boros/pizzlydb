# pizzlydb-txn

Multi-version concurrency control (MVCC), commit validation, commit sequencing, and the logical mutation log.

This crate is a skeleton. The sections below describe the intended contract. The crate compiles and exports no public API.

## Purpose

`pizzlydb-txn` will decide which transactions committed. A transaction begins at one stable snapshot. Its reads observe that snapshot plus its own writes. Commit assigns one linearized `CommitSeq` and validates entity conflicts, uniqueness, and predicate ranges.

Each committed transaction appends one logical mutation record. That record is the input to incremental maintenance. The physical write-ahead log (WAL) stays in `pizzlydb-storage`.

The crate depends on `pizzlydb-core` and `pizzlydb-storage`.

## Modules

- `snapshot` — MVCC read snapshots
- `validate` — commit-time conflict, uniqueness, and predicate-range checks
- `sequencer` — assignment of `CommitSeq`
- `log` — the durable logical mutation log

## License

Licensed under the [Apache License 2.0](../../LICENSE).
