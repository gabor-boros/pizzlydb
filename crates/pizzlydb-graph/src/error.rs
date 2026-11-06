//! Errors returned by graph operations.

use pizzlydb_storage::StorageError;
use thiserror::Error;

/// Failure from a [`GraphStore`](crate::GraphStore) operation.
///
/// A mutating call that returns an error leaves its
/// [`GraphBatch`](crate::GraphBatch) as it was before the call.
///
/// # Examples
///
/// ```
/// use pizzlydb_graph::GraphError;
/// use pizzlydb_storage::StorageError;
///
/// let err = GraphError::from(StorageError::Busy);
/// assert!(matches!(err, GraphError::Storage(StorageError::Busy)));
/// assert!(!matches!(GraphError::NotFound, GraphError::Deleted));
/// ```
#[derive(Debug, Error)]
pub enum GraphError {
    /// The node, edge, or name was never created in this namespace.
    #[error("not found")]
    NotFound,
    /// The node or edge was deleted. Its identifier is not reused.
    #[error("deleted")]
    Deleted,
    /// A label, relationship type, property key, or index identifier was not
    /// issued in this namespace.
    #[error("identifier is not issued in this namespace")]
    InvalidIdentity,
    /// An edge endpoint is missing or deleted.
    #[error("edge endpoint is missing or deleted")]
    EndpointNotLive,
    /// The node still has an incident edge.
    #[error("node has incident edges")]
    EndpointInUse,
    /// A name is empty, longer than [`MAX_NAME_LEN`](crate::MAX_NAME_LEN)
    /// bytes, contains a NUL byte, or is not UTF-8.
    #[error("invalid name")]
    InvalidName,
    /// A rename target is already used by another identifier.
    #[error("name is already in use")]
    NameConflict,
    /// A declaration change is incompatible, or a value does not match the
    /// declaration that applies to it.
    #[error("schema conflict")]
    SchemaConflict,
    /// Another entity already holds the value in a unique index.
    #[error("unique value is already in use")]
    UniqueConflict,
    /// A value cannot be encoded: NaN, or a string or byte string longer than
    /// `u32::MAX` bytes.
    #[error("invalid value")]
    InvalidValue,
    /// An encoded key is longer than
    /// [`MAX_KEY_LEN`](pizzlydb_storage::MAX_KEY_LEN).
    #[error("key is too long")]
    KeyTooLarge,
    /// An encoded value is longer than
    /// [`MAX_VALUE_LEN`](pizzlydb_storage::MAX_VALUE_LEN).
    #[error("value is too long")]
    ValueTooLarge,
    /// A counter has issued its last identifier.
    #[error("identifier space is exhausted")]
    IdExhausted,
    /// Stored graph bytes failed to decode.
    #[error("stored graph data is corrupt")]
    Corrupt,
    /// The storage engine failed.
    #[error(transparent)]
    Storage(#[from] StorageError),
}

pub(crate) fn corrupt<E>(_: E) -> GraphError {
    GraphError::Corrupt
}

pub(crate) fn invalid_value<E>(_: E) -> GraphError {
    GraphError::InvalidValue
}
