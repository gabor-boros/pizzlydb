# pizzlydb-core

Shared identifiers, typed graph values, errors, and commit sequence numbers.

This crate is a skeleton. The sections below describe the intended contract. The crate compiles and exports no public API.

## Purpose

`pizzlydb-core` is the shared type layer for the database crates. It will define opaque node and edge identifiers, typed property values, `CommitSeq`, and the errors those types return.

Property absence stays distinct from an explicit null. Identifier values do not encode a mutable storage location.

The crate has no dependency on storage, transactions, the graph layout, query planning, or the network protocol.

## Modules

- `id` — node and edge identifiers
- `value` — typed graph values
- `sequence` — monotonic `CommitSeq` values
- `error` — errors for those types

## License

Licensed under the [Apache License 2.0](../../LICENSE).
