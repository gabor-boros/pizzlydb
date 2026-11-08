# pizzlydb-graph

Graph records, adjacency, properties, indexes, and constraints stored as keys in the `pizzlydb-storage` ordered map.

## Purpose

`pizzlydb-graph` stores and validates the property graph of one namespace: node and edge records, catalog names, node labels, both adjacency directions, property cells and declarations, secondary indexes, and label and relationship-type membership. Every logical change is a set of puts and deletes on one storage write batch.

The crate depends on `pizzlydb-core`, `pizzlydb-storage`, and `pizzlydb-txn`. It does not import the protocol or the server. Graph writes go through `GraphTxn` and do not call `Storage::begin_write`.

## Usage

`GraphTxn::begin` opens an optimistic transaction on the latest snapshot. Reads through the transaction see its staged writes. `GraphTxn::commit` validates and publishes the batch; dropping it publishes nothing. A `GraphStore` call that returns an error stages nothing.

```rust
use std::path::Path;

use pizzlydb_core::{LabelId, NamespaceId, TypeId};
use pizzlydb_graph::{Direction, GraphStore, GraphTxn};
use pizzlydb_storage::{Durability, MemoryVfs, Storage};

let vfs = MemoryVfs::new();
let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
let graph = GraphStore::new(NamespaceId::from_u128(1)?);

let mut txn = GraphTxn::begin(&storage)?;
let person: LabelId = graph.intern(&mut txn, "Person")?;
let knows: TypeId = graph.intern(&mut txn, "KNOWS")?;
let a = graph.create_node(&mut txn)?;
let b = graph.create_node(&mut txn)?;
graph.add_label(&mut txn, a, person)?;
graph.create_edge(&mut txn, a, b, knows)?;
txn.commit()?;

let view = storage.snapshot()?;
assert_eq!(graph.neighbors(&view, a, Direction::Out)?.count(), 1);
```

## Rules

- Node and edge identifiers come from per-namespace counters and are never reused. A never-created id is `NotFound`; a deleted id is `Deleted`.
- `create_edge` requires live endpoints (`EndpointNotLive`). `delete_node` refuses a node with an incident edge (`EndpointInUse`).
- Names are 1 to 256 bytes of UTF-8 with no NUL, compared as exact bytes. Labels, relationship types, and property keys have separate name spaces. Names are not retired; rename is the only change, and ids are not reused.
- Every edge has one outgoing and one incoming adjacency key. A neighbor scan is one prefix range and reads no property keys.
- A property cell is one key. Absence is a missing key; null is a stored `Value::Null`. A declaration fixes a value type, nullability, and whether the property is required. A node must satisfy every label declaration. Required properties are checked at commit. An index key longer than `MAX_KEY_LEN` fails the write.
- An index covers one property key within one label or relationship type. It is `Building`, `Ready`, `Dropping`, or `Dropped`. Lookups use `Ready` indexes. A property index includes null; a unique index excludes null and reports `UniqueConflict`. `create_index` builds and marks the index ready in the caller's transaction. `start_index`, `backfill_index`, and `finish_index` build it across transactions. `drop_index` and `reclaim_index` remove it in chunks.
- `detach_delete_node` removes a node and its incident edges, counting a self-loop once, up to `DetachLimits::max_edges`.
- `GraphStore::open` requires a registered active namespace. `GraphStore::new` does not. Namespace ids are never reused. `NamespaceRegistry::reclaim` deletes a dropping namespace in chunks.
- Exact node, edge, label, type, index, and byte counters use commutative `merge_add` updates. Degree and distinct-value samples are refreshed explicitly.

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
