# pizzlydb-graph

Graph records, adjacency, properties, indexes, and constraints stored as keys in the `pizzlydb-storage` ordered map.

## Purpose

`pizzlydb-graph` stores and validates the property graph of one namespace: node and edge records, catalog names, node labels, both adjacency directions, property cells and declarations, secondary indexes, and label and relationship-type membership. Every logical change is a set of puts and deletes on one storage write batch.

The crate depends on `pizzlydb-core`, `pizzlydb-storage`, and `pizzlydb-txn`. It does not import the protocol or the server.

## Usage

`GraphBatch::begin` takes the storage writer lock and reads the latest snapshot. Reads through the batch see its staged writes. `GraphBatch::commit` publishes the batch atomically; dropping it publishes nothing. A `GraphStore` call that returns an error stages nothing.

```rust
use std::path::Path;

use pizzlydb_core::{LabelId, NamespaceId, TypeId};
use pizzlydb_graph::{Direction, GraphBatch, GraphStore};
use pizzlydb_storage::{Durability, MemoryVfs, Storage};

let vfs = MemoryVfs::new();
let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
let graph = GraphStore::new(NamespaceId::from_u128(1)?);

let mut batch = GraphBatch::begin(&storage)?;
let person: LabelId = graph.intern(&mut batch, "Person")?;
let knows: TypeId = graph.intern(&mut batch, "KNOWS")?;
let a = graph.create_node(&mut batch)?;
let b = graph.create_node(&mut batch)?;
graph.add_label(&mut batch, a, person)?;
graph.create_edge(&mut batch, a, b, knows)?;
batch.commit()?;

let view = storage.snapshot()?;
assert_eq!(graph.neighbors(&view, a, Direction::Out)?.count(), 1);
```

## Rules

- Node and edge identifiers come from per-namespace counters and are never reused. A never-created id is `NotFound`; a deleted id is `Deleted`.
- `create_edge` requires live endpoints (`EndpointNotLive`). `delete_node` refuses a node with an incident edge (`EndpointInUse`).
- Names are 1 to 256 bytes of UTF-8 with no NUL, compared as exact bytes. Labels, relationship types, and property keys have separate name spaces.
- Every edge has one outgoing and one incoming adjacency key. A neighbor scan is one prefix range and reads no property keys.
- A property cell is one key. Absence is a missing key; null is a stored `Value::Null`. A declaration on a label or relationship type fixes a value type and a nullable flag. A changed type, or a nullable declaration made required, is a `SchemaConflict`.
- An index covers one property key within one label or relationship type. Entries change in the same batch as the property, label, or edge. A property index includes null; a unique index excludes null and reports `UniqueConflict`, including against values staged earlier in the same batch.

## Modules

- `batch` — staged writes over one storage write batch
- `keys` — key layouts
- `record`, `record_store`, `directory` — node and edge records and identifier allocation
- `constraint` — endpoint and uniqueness checks
- `catalog` — names and node labels
- `adjacency` — outgoing and incoming adjacency
- `property` — property cells and declarations
- `index` — secondary indexes and membership
- `verify` — key checks for one namespace
- `salvage` — copy one namespace into a new database
- `bulk` — load a line-oriented script

`verify_full` reads one snapshot and does not write. `salvage` copies committed records through the graph API; a hole or an omission shifts later identifiers of that kind. `bulk_load` applies commands by phase and publishes the database only after `verify_full` succeeds.

## License

Licensed under the [Apache License 2.0](../../LICENSE).
