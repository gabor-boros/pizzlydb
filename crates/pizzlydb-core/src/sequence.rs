//! Dense logical commit order.
//!
//! [`CommitSeq`] is the user-visible order of committed transactions. It is
//! not a storage log sequence number. Zero is not a commit. The frontier
//! before any commit is [`CommitSeq::ZERO_FRONTIER`].

use std::fmt;
use std::num::NonZeroU64;

use crate::error::CoreError;

/// One committed transaction in logical order.
///
/// Values start at 1 and increase by 1. The byte encoding is big-endian, so
/// unsigned byte order matches numeric order.
///
/// # Examples
///
/// ```
/// use pizzlydb_core::{CommitSeq, CoreError};
///
/// let first = CommitSeq::first();
/// assert_eq!(first.get(), 1);
/// assert_eq!(first.next().unwrap().get(), 2);
/// assert_eq!(CommitSeq::from_u64(0), Err(CoreError::InvalidSequence));
/// assert_eq!(CommitSeq::ZERO_FRONTIER, None);
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommitSeq(NonZeroU64);

impl CommitSeq {
    /// Frontier before any commit.
    pub const ZERO_FRONTIER: Option<CommitSeq> = None;

    /// The first commit sequence.
    #[must_use]
    pub const fn first() -> Self {
        Self(NonZeroU64::MIN)
    }

    /// Build a sequence from a non-zero `u64`.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::InvalidSequence`] when `value` is zero.
    pub const fn from_u64(value: u64) -> Result<Self, CoreError> {
        match NonZeroU64::new(value) {
            Some(value) => Ok(Self(value)),
            None => Err(CoreError::InvalidSequence),
        }
    }

    /// Numeric value. It is never zero.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }

    /// The following sequence.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::SequenceOverflow`] when this value is `u64::MAX`.
    pub const fn next(self) -> Result<Self, CoreError> {
        match self.0.checked_add(1) {
            Some(value) => Ok(Self(value)),
            None => Err(CoreError::SequenceOverflow),
        }
    }

    /// Big-endian key bytes.
    #[must_use]
    pub const fn to_be_bytes(self) -> [u8; 8] {
        self.get().to_be_bytes()
    }

    /// Inverse of [`Self::to_be_bytes`].
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::InvalidSequence`] when the bytes are zero.
    pub const fn from_be_bytes(bytes: [u8; 8]) -> Result<Self, CoreError> {
        Self::from_u64(u64::from_be_bytes(bytes))
    }

    /// Append the big-endian encoding.
    pub fn encode(self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.to_be_bytes());
    }
}

impl fmt::Display for CommitSeq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.get())
    }
}

impl fmt::Debug for CommitSeq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CommitSeq({})", self.get())
    }
}

#[cfg(test)]
mod tests {
    use super::CommitSeq;
    use crate::error::CoreError;

    #[test]
    fn encoding_order_matches_numeric_order() {
        let values = [1, 2, 255, 256, 65_536, u64::MAX - 1, u64::MAX];
        for pair in values.windows(2) {
            let low = CommitSeq::from_u64(pair[0]).unwrap();
            let high = CommitSeq::from_u64(pair[1]).unwrap();
            assert!(low < high);
            assert!(low.to_be_bytes() < high.to_be_bytes());
            assert_eq!(CommitSeq::from_be_bytes(low.to_be_bytes()), Ok(low));
        }
    }

    #[test]
    fn next_checks_overflow() {
        let max = CommitSeq::from_u64(u64::MAX).unwrap();
        assert_eq!(max.next(), Err(CoreError::SequenceOverflow));
        assert_eq!(CommitSeq::first().to_string(), "1");
    }
}
