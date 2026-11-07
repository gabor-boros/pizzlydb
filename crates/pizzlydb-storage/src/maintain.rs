//! Reclaim pages that no open snapshot can still read.
//!
//! [`reclaim_to_horizon`] clears retire bits strictly below the pager horizon:
//! the oldest registered snapshot, or one past the committed sequence when no
//! snapshot is registered. A page retired at the horizon stays allocated.
//! Checkpoint and dropping a snapshot both call it.
//!
//! This module does not compact adjacency, property, or membership records.

use crate::engine::{Storage, StorageError};
use crate::vfs::Vfs;

/// Reclaim retired pages that are invisible to every open snapshot.
///
/// Returns whether the in-memory allocator changed. The caller persists that
/// image when a checkpoint covers the committed sequence.
///
/// # Errors
///
/// Returns [`StorageError::Corrupt`] when a retire entry is already free or
/// names a reserved page. The handle is then closed.
pub(crate) fn reclaim_to_horizon<V: Vfs>(storage: &Storage<V>) -> Result<bool, StorageError> {
    storage.reclaim_horizon()
}
