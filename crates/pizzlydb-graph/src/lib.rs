//! Graph records, the segmented identifier directory, adjacency, properties,
//! indexes, and constraints.
//!
//! This crate depends on `pizzlydb-core` and `pizzlydb-txn`. It does not import
//! `pizzlydb-storage` until a page API exists, and it does not import protocol
//! or server code.

mod adjacency;
mod catalog;
mod constraint;
mod directory;
mod index;
mod property;
mod record;
mod record_store;
