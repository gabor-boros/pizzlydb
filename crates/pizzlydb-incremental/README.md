# pizzlydb-incremental

Signed-delta operators, indexed arrangements, and maintained-state checkpoints.

This crate is a skeleton. The sections below describe the intended contract. The crate compiles and exports no public API.

## Purpose

`pizzlydb-incremental` will maintain the result of an accepted reactive query. For a query `Q` and committed snapshot `S`, applying the delta for commit `S+1` must equal evaluating `Q` from scratch on the graph at `S+1`.

The crate reads query IR from `pizzlydb-query` and logical graph mutations from `pizzlydb-txn`. It processes commits in `CommitSeq` order. It does not decide whether a transaction committed, and it does not read gRPC messages.

A maintained plan will persist its `processed_seq`, operator state, and output deltas together. Replaying a commit must not apply that commit twice.

The crate depends on `pizzlydb-core`, `pizzlydb-query`, and `pizzlydb-txn`.

## Modules

- `delta` — signed result changes and multiplicity
- `arrangement` — indexed state for fixed graph patterns
- `checkpoint` — durable `processed_seq` and operator state

## License

Licensed under the [Apache License 2.0](../../LICENSE).
