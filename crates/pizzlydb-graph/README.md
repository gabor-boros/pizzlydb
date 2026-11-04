# pizzlydb-graph

Graph records, adjacency, properties, indexes, and constraints.

This crate is a skeleton. The sections below describe the intended contract. The crate compiles and exports no public API.

## Purpose

`pizzlydb-graph` will store and validate the property graph: node and edge records, the segmented identifier directory, forward and reverse adjacency, declared properties, secondary indexes, the label and type catalog, and commit-time constraints.

Edge creation requires both endpoints to exist in the transaction's commit state. A node delete with incident edges fails unless the caller requests a detach delete. Cross-namespace edges are outside the core model.

The crate depends on `pizzlydb-core` and `pizzlydb-txn`. It does not import the protocol or the server. A direct dependency on `pizzlydb-storage` waits until that crate exposes a page API.

## Modules

- `record` and `record_store` — node and edge record layout
- `directory` — segmented identifier directory
- `adjacency` — forward and reverse adjacency
- `property` — declared property storage
- `index` — secondary indexes
- `catalog` — labels, edge types, and property keys
- `constraint` — endpoint and uniqueness rules

## License

Licensed under the [Apache License 2.0](../../LICENSE).
