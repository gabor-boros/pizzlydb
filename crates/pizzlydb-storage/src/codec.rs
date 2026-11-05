//! Little-endian integers for on-disk page and log records.
//!
//! [`put_u16`], [`get_u16`], and the wider helpers return [`CodecError::Truncated`]
//! when `offset` plus the integer width does not fit in the slice.

use thiserror::Error;

/// Failure while reading or writing an on-disk image.
///
/// # Examples
///
/// ```ignore
/// use pizzlydb_storage::codec::{get_u16, CodecError};
///
/// assert_eq!(get_u16(&[0x01], 0), Err(CodecError::Truncated));
/// ```
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum CodecError {
    /// The slice is shorter than the encoded value, or a page image is not exactly one page.
    #[error("truncated input")]
    Truncated,
    /// The page does not start with the format magic.
    #[error("bad page magic")]
    BadMagic,
    /// The page version is not the version this crate writes.
    #[error("unsupported page version")]
    UnsupportedVersion,
    /// The stored CRC-32C does not match the page image.
    #[error("checksum mismatch")]
    ChecksumMismatch,
    /// The page kind is not one of the kinds this crate owns.
    #[error("unknown page kind")]
    BadKind,
}

fn read_bytes<const N: usize>(src: &[u8], offset: usize) -> Result<[u8; N], CodecError> {
    let end = offset.checked_add(N).ok_or(CodecError::Truncated)?;
    let bytes = src.get(offset..end).ok_or(CodecError::Truncated)?;
    bytes.try_into().map_err(|_| CodecError::Truncated)
}

fn write_bytes<const N: usize>(
    dst: &mut [u8],
    offset: usize,
    bytes: [u8; N],
) -> Result<(), CodecError> {
    let end = offset.checked_add(N).ok_or(CodecError::Truncated)?;
    let slot = dst.get_mut(offset..end).ok_or(CodecError::Truncated)?;
    slot.copy_from_slice(&bytes);
    Ok(())
}

/// Read a little-endian `u16` at `offset`.
///
/// # Errors
///
/// Returns [`CodecError::Truncated`] when `offset + 2` does not fit in `src`.
///
/// # Examples
///
/// ```ignore
/// use pizzlydb_storage::codec::{get_u16, put_u16};
///
/// let mut buf = [0; 4];
/// put_u16(&mut buf, 1, 0x0201).unwrap();
///
/// assert_eq!(get_u16(&buf, 1).unwrap(), 0x0201);
/// ```
pub fn get_u16(src: &[u8], offset: usize) -> Result<u16, CodecError> {
    Ok(u16::from_le_bytes(read_bytes(src, offset)?))
}

/// Write a little-endian `u16` at `offset`.
///
/// # Errors
///
/// Returns [`CodecError::Truncated`] when `offset + 2` does not fit in `dst`.
///
/// # Examples
///
/// ```ignore
/// use pizzlydb_storage::codec::{get_u16, put_u16, CodecError};
///
/// let mut buf = [0; 4];
/// put_u16(&mut buf, 1, 0x0201).unwrap();
///
/// assert_eq!(&buf[1..3], &[0x01, 0x02]);
/// assert_eq!(get_u16(&buf, 3), Err(CodecError::Truncated));
/// ```
pub fn put_u16(dst: &mut [u8], offset: usize, value: u16) -> Result<(), CodecError> {
    write_bytes(dst, offset, value.to_le_bytes())
}

/// Read a little-endian `u32` at `offset`.
///
/// # Errors
///
/// Returns [`CodecError::Truncated`] when `offset + 4` does not fit in `src`.
///
/// # Examples
///
/// ```ignore
/// use pizzlydb_storage::codec::{get_u32, put_u32};
///
/// let mut buf = [0; 6];
/// put_u32(&mut buf, 1, 0x0403_0201).unwrap();
///
/// assert_eq!(get_u32(&buf, 1).unwrap(), 0x0403_0201);
/// ```
pub fn get_u32(src: &[u8], offset: usize) -> Result<u32, CodecError> {
    Ok(u32::from_le_bytes(read_bytes(src, offset)?))
}

/// Write a little-endian `u32` at `offset`.
///
/// # Errors
///
/// Returns [`CodecError::Truncated`] when `offset + 4` does not fit in `dst`.
///
/// # Examples
///
/// ```ignore
/// use pizzlydb_storage::codec::put_u32;
///
/// let mut buf = [0; 6];
/// put_u32(&mut buf, 2, 0x0403_0201).unwrap();
///
/// assert_eq!(&buf[2..6], &[0x01, 0x02, 0x03, 0x04]);
/// ```
pub fn put_u32(dst: &mut [u8], offset: usize, value: u32) -> Result<(), CodecError> {
    write_bytes(dst, offset, value.to_le_bytes())
}

/// Read a little-endian `u64` at `offset`.
///
/// # Errors
///
/// Returns [`CodecError::Truncated`] when `offset + 8` does not fit in `src`.
///
/// # Examples
///
/// ```ignore
/// use pizzlydb_storage::codec::{get_u64, CodecError};
///
/// assert_eq!(get_u64(&[0; 8], usize::MAX), Err(CodecError::Truncated));
/// ```
pub fn get_u64(src: &[u8], offset: usize) -> Result<u64, CodecError> {
    Ok(u64::from_le_bytes(read_bytes(src, offset)?))
}

/// Write a little-endian `u64` at `offset`.
///
/// # Errors
///
/// Returns [`CodecError::Truncated`] when `offset + 8` does not fit in `dst`.
///
/// # Examples
///
/// ```ignore
/// use pizzlydb_storage::codec::{get_u64, put_u64};
///
/// let mut buf = [0; 10];
/// put_u64(&mut buf, 2, 0x0807_0605_0403_0201).unwrap();
///
/// assert_eq!(get_u64(&buf, 2).unwrap(), 0x0807_0605_0403_0201);
/// ```
pub fn put_u64(dst: &mut [u8], offset: usize, value: u64) -> Result<(), CodecError> {
    write_bytes(dst, offset, value.to_le_bytes())
}

#[cfg(test)]
mod tests {
    use super::{CodecError, get_u16, get_u32, get_u64, put_u16, put_u32, put_u64};

    #[test]
    fn example_truncated_u16() {
        assert_eq!(get_u16(&[0x01], 0), Err(CodecError::Truncated));
    }

    #[test]
    fn example_u16_roundtrip() {
        let mut buf = [0; 4];
        put_u16(&mut buf, 1, 0x0201).unwrap();
        assert_eq!(get_u16(&buf, 1).unwrap(), 0x0201);
        assert_eq!(&buf[1..3], &[0x01, 0x02]);
        assert_eq!(get_u16(&buf, 3), Err(CodecError::Truncated));
    }

    #[test]
    fn example_u32_roundtrip() {
        let mut buf = [0; 6];
        put_u32(&mut buf, 1, 0x0403_0201).unwrap();
        assert_eq!(get_u32(&buf, 1).unwrap(), 0x0403_0201);
        put_u32(&mut buf, 2, 0x0403_0201).unwrap();
        assert_eq!(&buf[2..6], &[0x01, 0x02, 0x03, 0x04]);
    }

    #[test]
    fn example_u64_roundtrip_and_overflow() {
        assert_eq!(get_u64(&[0; 8], usize::MAX), Err(CodecError::Truncated));
        let mut buf = [0; 10];
        put_u64(&mut buf, 2, 0x0807_0605_0403_0201).unwrap();
        assert_eq!(get_u64(&buf, 2).unwrap(), 0x0807_0605_0403_0201);
        assert_eq!(
            &buf[2..10],
            &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]
        );
    }

    #[test]
    fn short_tail_and_empty_slice_are_truncated() {
        assert_eq!(get_u16(&[], 0), Err(CodecError::Truncated));
        assert_eq!(put_u16(&mut [], 0, 1), Err(CodecError::Truncated));
        assert_eq!(get_u32(&[0; 4], 1), Err(CodecError::Truncated));
        assert_eq!(put_u32(&mut [0; 4], 1, 1), Err(CodecError::Truncated));
        assert_eq!(get_u64(&[0; 8], 1), Err(CodecError::Truncated));
        assert_eq!(put_u64(&mut [0; 8], 1, 1), Err(CodecError::Truncated));
    }

    #[test]
    fn offset_overflow_is_truncated() {
        assert_eq!(get_u16(&[0; 2], usize::MAX), Err(CodecError::Truncated));
        assert_eq!(
            put_u16(&mut [0; 2], usize::MAX, 1),
            Err(CodecError::Truncated)
        );
        assert_eq!(get_u32(&[0; 4], usize::MAX), Err(CodecError::Truncated));
        assert_eq!(
            put_u32(&mut [0; 4], usize::MAX, 1),
            Err(CodecError::Truncated)
        );
        assert_eq!(get_u64(&[0; 8], usize::MAX - 1), Err(CodecError::Truncated));
        assert_eq!(
            put_u64(&mut [0; 8], usize::MAX - 1, 1),
            Err(CodecError::Truncated)
        );
    }
}
