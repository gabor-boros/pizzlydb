//! Errors from the transaction manager.
//!
//! [`TxnError::is_retryable`] is the contract callers use. A conflict can be
//! retried as a new transaction. Expiry, size, and read-only failures cannot.

use pizzlydb_core::CoreError;
use pizzlydb_storage::StorageError;
use thiserror::Error;

/// Why validation rejected a commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictReason {
    /// A commit after this snapshot wrote a key or range this transaction read.
    ReadWrite,
    /// The write-set history no longer covers this snapshot.
    HistoryEvicted,
}

impl std::fmt::Display for ConflictReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReadWrite => write!(f, "read-write conflict"),
            Self::HistoryEvicted => write!(f, "validation history was evicted"),
        }
    }
}

/// Failure from a transaction operation or commit.
#[derive(Debug, Error)]
pub enum TxnError {
    /// Validation failed. The transaction did not commit.
    #[error("transaction conflict ({reason})")]
    Conflict {
        /// Why the histories were incompatible.
        reason: ConflictReason,
    },
    /// The lifetime or idle limit elapsed.
    #[error("transaction expired")]
    Expired,
    /// The pinned snapshot was reaped.
    #[error("snapshot is too old")]
    SnapshotTooOld,
    /// A configured memory or cardinality limit was crossed.
    #[error("transaction exceeds the {limit} limit; observed {observed}")]
    TransactionTooLarge {
        /// Which limit fired.
        limit: &'static str,
        /// Observed size.
        observed: u64,
    },
    /// A write was attempted on a read-only transaction.
    #[error("read-only transaction cannot write")]
    ReadOnlyViolation,
    /// The transaction was already committed or rolled back.
    #[error("transaction is closed")]
    Closed,
    /// [`crate::TxnOptions::allow_client_ids`] is false.
    #[error("client-supplied ids are not enabled")]
    ClientIdsDisabled,
    /// The id is in the allocator range. Client ids use the high bit.
    #[error("client-supplied id is outside the reserved range")]
    InvalidClientId,
    /// The allocator has no id left below the client-id bit.
    #[error("identifier space is exhausted")]
    IdExhausted,
    /// A commit record, frontier, or log chunk failed a check.
    #[error("stored transaction data is corrupt")]
    Corrupt,
    /// The storage engine failed.
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// A codec in `pizzlydb-core` failed.
    #[error(transparent)]
    Core(#[from] CoreError),
}

impl TxnError {
    /// Whether a new transaction, with the same work, can succeed.
    ///
    /// Conflicts are retryable. Expiry, size limits, read-only violations,
    /// closed handles, and corruption are not.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Conflict {
                reason: ConflictReason::ReadWrite | ConflictReason::HistoryEvicted
            }
        )
    }
}

pub(crate) fn map_storage(err: StorageError) -> TxnError {
    match err {
        StorageError::SnapshotTooOld => TxnError::SnapshotTooOld,
        other => TxnError::Storage(other),
    }
}
