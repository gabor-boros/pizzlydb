//! Property graph stored as keys in the `pizzlydb-storage` ordered map.
//!
//! [`GraphStore`] reads and writes the keys of one namespace: node and edge
//! records, catalog names, node labels, both adjacency directions, property
//! cells and declarations, secondary indexes, and label and relationship-type
//! membership. Reads take any [`KeyRead`]: a committed
//! [`ReadView`](pizzlydb_storage::ReadView) or an open [`GraphBatch`]. Writes
//! stage keys on a [`GraphBatch`], which reads its own staged writes and
//! publishes them as one atomic storage batch on [`GraphBatch::commit`].
//!
//! [`verify_full`] checks that the keys of one namespace agree.
//! [`salvage`] copies one namespace into a new database.
//! [`bulk_load`] creates a database from a line-oriented script.
//!
//! This crate does not import protocol or server code.
//!
//! # Examples
//!
//! ```
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use std::path::Path;
//!
//! use pizzlydb_core::{LabelId, NamespaceId, PropertyKeyId, TypeId, Value};
//! use pizzlydb_graph::{Direction, GraphBatch, GraphStore, Owner, Scope};
//! use pizzlydb_storage::{Durability, MemoryVfs, Storage};
//!
//! let vfs = MemoryVfs::new();
//! let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
//! let graph = GraphStore::new(NamespaceId::from_u128(1)?);
//!
//! let mut batch = GraphBatch::begin(&storage)?;
//! let person: LabelId = graph.intern(&mut batch, "Person")?;
//! let knows: TypeId = graph.intern(&mut batch, "KNOWS")?;
//! let name: PropertyKeyId = graph.intern(&mut batch, "name")?;
//! let by_name = graph.create_index(&mut batch, Scope::Label(person), name, true)?;
//!
//! let ada = graph.create_node(&mut batch)?;
//! let alan = graph.create_node(&mut batch)?;
//! for (node, text) in [(ada, "Alice"), (alan, "Alan")] {
//!     graph.add_label(&mut batch, node, person)?;
//!     graph.set_property(&mut batch, node, name, &Value::from(text))?;
//! }
//! graph.create_edge(&mut batch, ada, alan, knows)?;
//! batch.commit()?;
//!
//! let view = storage.snapshot()?;
//! let found = graph.index_lookup(&view, by_name, &Value::from("Alice"))?.collect::<Result<Vec<_>, _>>()?;
//! assert_eq!(found, vec![Owner::Node(ada)]);
//! let friend = graph.neighbors(&view, ada, Direction::Out)?.next().transpose()?;
//! assert_eq!(friend.map(|entry| entry.neighbor), Some(alan));
//! # Ok(())
//! # }
//! ```

mod access;
mod adjacency;
mod batch;
mod bulk;
mod catalog;
mod constraint;
mod directory;
mod error;
#[cfg(feature = "fuzzing")]
pub mod fuzz;
mod index;
mod keys;
mod property;
mod record;
mod record_store;
mod salvage;
mod verify;

pub use access::{IdScan, KeyRead};
pub use adjacency::{Direction, Neighbor, Neighbors};
pub use batch::{BatchScan, GraphBatch};
pub use bulk::{LoadOptions, LoadReport, bulk_load};
pub use catalog::{CatalogId, MAX_NAME_LEN};
pub use error::GraphError;
pub use index::{IndexDefinition, IndexScan};
pub use property::{Owner, PropertyDeclaration, Scope};
pub use record::{EdgeRecord, NodeRecord};
pub use record_store::GraphStore;
pub use salvage::{Omission, OmissionKind, OmissionReason, SalvageReport, salvage};
pub use verify::verify_full;
