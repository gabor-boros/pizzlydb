# pizzlydb-testkit

Reference graph model, fault harnesses, and deterministic workloads.

This crate is a skeleton. The sections below describe the intended contract. The crate compiles and exports no public API.

## Purpose

`pizzlydb-testkit` will hold the independent oracles used to test the database. A reference in-memory graph will recompute query results. Fault harnesses will cover process kill, durability failure, and restart. Workload generators will produce deterministic graph mutations.

The crate depends on `pizzlydb-core`. It does not replace the tests that live next to each crate.

## Modules

- `reference` — the in-memory graph oracle

## License

Licensed under the [Apache License 2.0](../../LICENSE).
