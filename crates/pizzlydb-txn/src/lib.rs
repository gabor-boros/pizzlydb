//! MVCC snapshots, serializable validation, commit sequencing, and the logical
//! mutation log.
//!
//! Page ownership stays in `pizzlydb-storage`. This crate decides which
//! transactions committed. Graph code calls [`TxnManager`] instead of
//! `Storage::begin_write`.

mod error;
mod keys;
mod log;
mod manager;
mod readset;
mod record;

pub use error::{ConflictReason, TxnError};
pub use keys::{GRAPH_LOGICAL_VERSION, is_system_key};
pub use log::CommitLog;
pub use manager::{FloorId, Txn, TxnLimits, TxnManager, TxnOptions, TxnStats};
pub use record::CommitRecord;
