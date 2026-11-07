//! Errors returned by identifier and value codecs.

use thiserror::Error;

/// Failure from an identifier or value codec.
///
/// Decoders return this for short or malformed input. They do not panic.
///
/// # Examples
///
/// ```
/// use pizzlydb_core::{CoreError, NodeId, decode_value};
///
/// assert_eq!(NodeId::try_from([0u8; 16]), Err(CoreError::InvalidIdentity));
/// assert_eq!(decode_value(&[]), Err(CoreError::Truncated));
/// assert_eq!(decode_value(&[0]), Err(CoreError::InvalidValue));
/// ```
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum CoreError {
    /// An identifier is all zero or does not have the identifier width.
    #[error("invalid identifier")]
    InvalidIdentity,
    /// A value cannot be encoded, or encoded bytes are not a valid value.
    #[error("invalid value")]
    InvalidValue,
    /// Encoded input ends before a complete item.
    #[error("input is truncated")]
    Truncated,
    /// A commit sequence was zero or could not be decoded.
    #[error("invalid commit sequence")]
    InvalidSequence,
    /// The next commit sequence would not fit in a `u64`.
    #[error("commit sequence overflow")]
    SequenceOverflow,
    /// A logical mutation could not be decoded.
    #[error("invalid mutation")]
    InvalidMutation,
}
