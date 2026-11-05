//! CRC-32C checksums for page images and the write-ahead log.
//!
//! The digest is CRC-32C (Castagnoli), computed by the `crc32c` crate.
//! [`Checksum::hash_skipping`] covers `bytes` and treats `skip` as zeros.
//! Page sealing uses that for the stored checksum, so the field can sit inside
//! the image it protects.

use std::ops::Range;

use crate::codec::CodecError;

/// CRC-32C digest of an on-disk image.
///
/// # Examples
///
/// ```ignore
/// use pizzlydb_storage::checksum::Checksum;
///
/// assert_eq!(Checksum::hash(b"123456789").as_u32(), 0xE306_9283);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Checksum(u32);

impl Checksum {
    /// CRC-32C of `bytes`.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use pizzlydb_storage::checksum::Checksum;
    ///
    /// assert_eq!(Checksum::hash(b"123456789").as_u32(), 0xE306_9283);
    /// ```
    #[must_use]
    pub fn hash(bytes: &[u8]) -> Self {
        Self(crc32c::crc32c(bytes))
    }

    /// CRC-32C of `bytes` with `skip` treated as zeros.
    ///
    /// `skip` is inside `bytes` when `skip.start <= skip.end` and
    /// `skip.end <= bytes.len()`. Any other range is [`CodecError::Truncated`].
    ///
    /// # Errors
    ///
    /// Returns [`CodecError::Truncated`] when `skip` is outside `bytes`.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use pizzlydb_storage::checksum::Checksum;
    ///
    /// let mut bytes = [1_u8; 32];
    /// bytes[8..12].copy_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
    ///
    /// let skipped = Checksum::hash_skipping(&bytes, 8..12).unwrap();
    /// bytes[8..12].fill(0);
    ///
    /// assert_eq!(skipped, Checksum::hash(&bytes));
    /// ```
    pub fn hash_skipping(bytes: &[u8], skip: Range<usize>) -> Result<Self, CodecError> {
        if skip.start > skip.end || skip.end > bytes.len() {
            return Err(CodecError::Truncated);
        }

        let mut crc = crc32c::crc32c(&bytes[..skip.start]);
        let zeros = [0_u8; 16];

        let mut remaining = skip.end - skip.start;
        while remaining > 0 {
            let n = remaining.min(zeros.len());
            crc = crc32c::crc32c_append(crc, &zeros[..n]);
            remaining -= n;
        }

        Ok(Self(crc32c::crc32c_append(crc, &bytes[skip.end..])))
    }

    /// Wrap a stored checksum. This does not recompute the digest.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use pizzlydb_storage::checksum::Checksum;
    ///
    /// let sum = Checksum::from_u32(0xE306_9283);
    /// assert_eq!(sum.as_u32(), 0xE306_9283);
    /// ```
    #[must_use]
    pub const fn from_u32(raw: u32) -> Self {
        Self(raw)
    }

    /// Stored little-endian value.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use pizzlydb_storage::checksum::Checksum;
    ///
    /// assert_eq!(Checksum::from_u32(0xE306_9283).as_u32(), 0xE306_9283);
    /// ```
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Range;

    use super::Checksum;
    use crate::codec::CodecError;

    #[test]
    fn example_known_vector() {
        assert_eq!(Checksum::hash(b"123456789").as_u32(), 0xE306_9283);
    }

    #[test]
    fn example_from_stored_u32() {
        let sum = Checksum::from_u32(0xE306_9283);
        assert_eq!(sum.as_u32(), 0xE306_9283);
    }

    #[test]
    fn example_hash_skipping_matches_a_zeroed_field() {
        let mut bytes = [1_u8; 32];
        bytes[8..12].copy_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
        let skipped = Checksum::hash_skipping(&bytes, 8..12).unwrap();
        bytes[8..12].fill(0);
        assert_eq!(skipped, Checksum::hash(&bytes));
    }

    #[test]
    fn empty_skip_matches_hash() {
        let bytes = [9_u8; 8];
        assert_eq!(
            Checksum::hash_skipping(&bytes, 0..0).unwrap(),
            Checksum::hash(&bytes)
        );
        assert_eq!(
            Checksum::hash_skipping(&bytes, 3..3).unwrap(),
            Checksum::hash(&bytes)
        );
    }

    #[test]
    fn skipping_the_whole_buffer_hashes_zeros() {
        let bytes = [7_u8; 8];
        let skipped = Checksum::hash_skipping(&bytes, 0..bytes.len()).unwrap();
        assert_eq!(skipped, Checksum::hash(&[0_u8; 8]));
    }

    #[test]
    fn skip_outside_the_slice_is_truncated() {
        let bytes = [1_u8; 4];
        assert_eq!(
            Checksum::hash_skipping(&bytes, Range { start: 2, end: 1 }),
            Err(CodecError::Truncated)
        );
        assert_eq!(
            Checksum::hash_skipping(&bytes, 0..5),
            Err(CodecError::Truncated)
        );
        assert_eq!(
            Checksum::hash_skipping(&bytes, 5..5),
            Err(CodecError::Truncated)
        );
    }
}
