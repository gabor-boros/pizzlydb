//! Errors returned by graph operations.

use pizzlydb_core::PropertyKeyId;
use pizzlydb_storage::StorageError;
use pizzlydb_txn::TxnError;
use thiserror::Error;

/// Why a constraint rejected a write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConstraintKind {
    /// A required property is absent.
    RequiredMissing,
    /// A present value has the wrong type.
    TypeMismatch,
    /// A present value is null and the declaration forbids null.
    NullForbidden,
    /// Two declarations of one property key require different types.
    DeclarationConflict,
}

impl std::fmt::Display for ConstraintKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::RequiredMissing => "required property is missing",
            Self::TypeMismatch => "value has the wrong type",
            Self::NullForbidden => "null is not allowed",
            Self::DeclarationConflict => "declarations disagree on the value type",
        })
    }
}

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
    /// A declaration change is incompatible with another declaration.
    #[error("schema conflict")]
    SchemaConflict,
    /// A value or schema change breaks a constraint.
    #[error("constraint violation: {kind}: {detail}")]
    ConstraintViolation {
        /// Which rule failed.
        kind: ConstraintKind,
        /// `1` for a node, `2` for an edge, `0` when the failure is not about one entity.
        entity_kind: u8,
        /// Identifier bytes.
        entity: [u8; 16],
        /// Property key, when the rule names one.
        key: Option<PropertyKeyId>,
        /// Short explanation.
        detail: String,
    },
    /// Another entity already holds the value in a unique index.
    #[error("unique value is already in use")]
    UniqueConflict,
    /// A unique index build found duplicate values.
    #[error("duplicate values")]
    DuplicateValues {
        /// Owners that hold the repeated value, including the one being indexed.
        owners: Vec<[u8; 16]>,
    },
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
    /// The namespace is not in the registry.
    #[error("namespace was not found")]
    NamespaceNotFound,
    /// The namespace is marked dropping and rejects new work.
    #[error("namespace is being dropped")]
    NamespaceDropping,
    /// Detach delete would remove more edges than the caller allowed.
    #[error("too many incident edges (limit {limit}, found at least {found_at_least})")]
    TooManyIncidentEdges {
        /// Configured maximum.
        limit: u64,
        /// Count observed before the operation stopped. It is greater than `limit`.
        found_at_least: u64,
    },
    /// An index exists but is not `Ready`.
    #[error("index is not ready")]
    IndexNotReady,
    /// Stored graph bytes failed to decode.
    #[error("stored graph data is corrupt")]
    Corrupt,
    /// A stored key does not match the record it names.
    #[error("graph verification failed")]
    VerificationFailed,
    /// The storage engine failed.
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// The transaction manager rejected the operation.
    ///
    /// [`TxnError::is_retryable`](pizzlydb_txn::TxnError::is_retryable) reports
    /// whether a new transaction can retry the work. Conflicts are retryable.
    #[error(transparent)]
    Transaction(TxnError),
}

pub(crate) fn corrupt<E>(_: E) -> GraphError {
    GraphError::Corrupt
}

pub(crate) fn invalid_value<E>(_: E) -> GraphError {
    GraphError::InvalidValue
}

impl From<TxnError> for GraphError {
    fn from(err: TxnError) -> Self {
        match err {
            TxnError::Storage(err) => Self::Storage(err),
            TxnError::IdExhausted => Self::IdExhausted,
            TxnError::InvalidClientId | TxnError::ClientIdsDisabled => Self::InvalidIdentity,
            other => Self::Transaction(other),
        }
    }
}

impl GraphError {
    /// Canonical protocol error name for this failure.
    ///
    /// Constraint failures are `CONSTRAINT_VIOLATION`. A detach that exceeds
    /// its edge limit is `RESOURCE_EXHAUSTED`.
    #[must_use]
    pub fn client_code(&self) -> &'static str {
        match self {
            Self::ConstraintViolation { .. }
            | Self::SchemaConflict
            | Self::UniqueConflict
            | Self::DuplicateValues { .. }
            | Self::NameConflict
            | Self::EndpointInUse
            | Self::EndpointNotLive => "CONSTRAINT_VIOLATION",
            Self::TooManyIncidentEdges { .. }
            | Self::KeyTooLarge
            | Self::ValueTooLarge
            | Self::IdExhausted => "RESOURCE_EXHAUSTED",
            Self::NotFound | Self::Deleted | Self::NamespaceNotFound | Self::InvalidIdentity => {
                "NOT_FOUND"
            }
            Self::NamespaceDropping | Self::IndexNotReady => "NOT_FOUND",
            Self::InvalidName | Self::InvalidValue => "INVALID_ARGUMENT",
            Self::Transaction(err) => match err {
                TxnError::Conflict { .. } => "TX_CONFLICT",
                TxnError::Expired | TxnError::SnapshotTooOld => "TX_EXPIRED",
                TxnError::TransactionTooLarge { .. } => "RESOURCE_EXHAUSTED",
                _ => "INTERNAL",
            },
            Self::Storage(_) | Self::Corrupt | Self::VerificationFailed => "INTERNAL",
        }
    }
}
