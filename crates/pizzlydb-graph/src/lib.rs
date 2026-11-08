//! Property graph stored as keys in the `pizzlydb-storage` ordered map.
//!
//! [`GraphStore`] reads and writes the keys of one namespace. Reads take any
//! [`KeyRead`]: a committed [`ReadView`](pizzlydb_storage::ReadView) or an open
//! [`GraphTxn`]. Writes stage keys on a [`GraphTxn`]. Commit validates the
//! transaction and publishes it as one atomic storage batch.
//!
//! [`verify_full`] checks that the keys of one namespace agree.
//! [`salvage`] copies one namespace into a new database.
//! [`bulk_load`] creates a database from a line-oriented script.
//!
//! This crate does not import protocol or server code, and it does not call
//! [`Storage::begin_write`](pizzlydb_storage::Storage::begin_write).
//!
//! # Examples
//!
//! ```
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use std::path::Path;
//!
//! use pizzlydb_core::{LabelId, NamespaceId, PropertyKeyId, TypeId, Value};
//! use pizzlydb_graph::{Direction, GraphStore, GraphTxn, Owner, Scope};
//! use pizzlydb_storage::{Durability, MemoryVfs, Storage};
//!
//! let vfs = MemoryVfs::new();
//! let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
//! let graph = GraphStore::new(NamespaceId::from_u128(1)?);
//!
//! let mut txn = GraphTxn::begin(&storage)?;
//! let person: LabelId = graph.intern(&mut txn, "Person")?;
//! let knows: TypeId = graph.intern(&mut txn, "KNOWS")?;
//! let name: PropertyKeyId = graph.intern(&mut txn, "name")?;
//! let by_name = graph.create_index(&mut txn, Scope::Label(person), name, true)?;
//!
//! let ada = graph.create_node(&mut txn)?;
//! let alan = graph.create_node(&mut txn)?;
//! for (node, text) in [(ada, "Alice"), (alan, "Alan")] {
//!     graph.add_label(&mut txn, node, person)?;
//!     graph.set_property(&mut txn, node, name, &Value::from(text))?;
//! }
//! graph.create_edge(&mut txn, ada, alan, knows)?;
//! txn.commit()?;
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
mod namespace;
mod property;
mod record;
mod record_store;
mod salvage;
mod schema;
mod stats;
mod verify;

pub use access::{IdScan, KeyRead};
pub use adjacency::{Direction, Neighbor, Neighbors};
pub use batch::{GraphBatch, GraphScan, GraphTxn};
pub use bulk::{LoadOptions, LoadReport, bulk_load};
pub use catalog::{CatalogId, MAX_NAME_LEN};
pub use error::{ConstraintKind, GraphError};
pub use index::{IndexDefinition, IndexPhase, IndexProgress, IndexScan, IndexState};
pub use namespace::{
    NamespaceLimits, NamespaceMeta, NamespaceOptions, NamespaceRegistry, NamespaceState,
    verify_registered,
};
pub use property::{Owner, PropertyDeclaration, Scope};
pub use record::{EdgeRecord, NodeRecord};
pub use record_store::{DetachLimits, GraphStore};
pub use salvage::{Omission, OmissionKind, OmissionReason, SalvageReport, salvage};
pub use schema::{CatalogCache, CatalogSnapshot};
pub use stats::{GraphStats, StatAsOf, Statistic};
pub use verify::verify_full;
