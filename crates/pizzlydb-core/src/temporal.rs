//! Instants, civil dates, and durations.
//!
//! [`Timestamp`] is a UTC instant in microseconds. [`Date`] is a proleptic
//! Gregorian day. [`LocalDateTime`] is civil time with no offset, also in
//! microseconds from the Unix epoch. [`Duration`] is a signed elapsed span in
//! microseconds. These types do not compare equal to each other.

use crate::error::CoreError;

/// UTC instant with microsecond resolution.
///
/// The value is microseconds since the Unix epoch. The range is the full
/// `i64`, about ±292,277 years.
///
/// # Examples
///
/// ```
/// use pizzlydb_core::Timestamp;
///
/// let instant = Timestamp::from_unix_micros(1_700_000_000_000_000);
/// assert_eq!(instant.unix_micros(), 1_700_000_000_000_000);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(i64);

impl Timestamp {
    /// Instant `micros` microseconds after the Unix epoch.
    #[must_use]
    pub const fn from_unix_micros(micros: i64) -> Self {
        Self(micros)
    }

    /// Microseconds since the Unix epoch.
    #[must_use]
    pub const fn unix_micros(self) -> i64 {
        self.0
    }
}

/// Proleptic Gregorian calendar day.
///
/// The value is days since 1970-01-01. Negative days are earlier. There is
/// no time zone.
///
/// # Examples
///
/// ```
/// use pizzlydb_core::Date;
///
/// assert!(Date::from_unix_days(0).unix_days() < Date::from_unix_days(1).unix_days());
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date(i32);

impl Date {
    /// Day `days` days after 1970-01-01.
    #[must_use]
    pub const fn from_unix_days(days: i32) -> Self {
        Self(days)
    }

    /// Days since 1970-01-01.
    #[must_use]
    pub const fn unix_days(self) -> i32 {
        self.0
    }
}

/// Civil date and time of day, without an offset.
///
/// The value is microseconds since 1970-01-01T00:00:00 in that civil clock.
/// It is not a UTC instant and does not compare equal to a [`Timestamp`] of
/// the same number.
///
/// # Examples
///
/// ```
/// use pizzlydb_core::LocalDateTime;
///
/// let noon = LocalDateTime::from_unix_micros(12 * 60 * 60 * 1_000_000);
/// assert_eq!(noon.unix_micros(), 43_200_000_000);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalDateTime(i64);

impl LocalDateTime {
    /// Civil time `micros` microseconds after 1970-01-01T00:00:00.
    #[must_use]
    pub const fn from_unix_micros(micros: i64) -> Self {
        Self(micros)
    }

    /// Microseconds since 1970-01-01T00:00:00.
    #[must_use]
    pub const fn unix_micros(self) -> i64 {
        self.0
    }
}

/// Signed elapsed time in microseconds.
///
/// # Examples
///
/// ```
/// use pizzlydb_core::Duration;
///
/// assert!(Duration::from_micros(-1).micros() < Duration::from_micros(1).micros());
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Duration(i64);

impl Duration {
    /// Span of `micros` microseconds. Negative values point backward.
    #[must_use]
    pub const fn from_micros(micros: i64) -> Self {
        Self(micros)
    }

    /// Elapsed microseconds.
    #[must_use]
    pub const fn micros(self) -> i64 {
        self.0
    }
}

/// Order-preserving encoding of a signed 32-bit integer.
#[must_use]
pub(crate) const fn encode_i32_key(value: i32) -> [u8; 4] {
    (value.cast_unsigned() ^ (1 << 31)).to_be_bytes()
}

pub(crate) const fn decode_i32_key(bytes: [u8; 4]) -> i32 {
    (u32::from_be_bytes(bytes) ^ (1 << 31)).cast_signed()
}

pub(crate) fn read_i64(bytes: &[u8]) -> Result<i64, CoreError> {
    let slice = bytes.get(..8).ok_or(CoreError::Truncated)?;
    let mut buf = [0_u8; 8];
    buf.copy_from_slice(slice);
    Ok(i64::from_be_bytes(buf))
}

pub(crate) fn read_i32(bytes: &[u8]) -> Result<i32, CoreError> {
    let slice = bytes.get(..4).ok_or(CoreError::Truncated)?;
    let mut buf = [0_u8; 4];
    buf.copy_from_slice(slice);
    Ok(i32::from_be_bytes(buf))
}
