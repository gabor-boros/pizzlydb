//! Property values and their byte encodings.
//!
//! [`Value`] is the set stored in a property cell: null, boolean, `i64`,
//! finite `f64`, [`Decimal`], UTF-8 string, bytes, [`Timestamp`], [`Date`],
//! [`LocalDateTime`], [`Duration`], heterogeneous [`Value::List`], and nested
//! [`Value::Map`]. Property absence has no encoding. Callers use
//! `Option<Value>`, where `None` is absent and `Some(Value::Null)` is an
//! explicit null.
//!
//! # Comparison
//!
//! [`index_cmp`] is the total order used by index keys. Integers, floats, and
//! decimals share one numeric order, so `Int(2)` sorts after `Float(1.5)`.
//! Equal numeric values encode to the same key. [`predicate_cmp`] is the
//! three-valued comparison used by queries: null and values from different
//! groups are unknown.
//!
//! String and byte comparison is unsigned byte order. Names use the same
//! binary UTF-8 order, with no normalization and no case folding. Collection
//! keys are ordered lexicographically and may be indexed when the encoded key
//! fits the storage key limit. An over-long index key is rejected at write
//! time.
//!
//! # Cell format
//!
//! [`encode_value`] writes one tag byte followed by the body:
//!
//! - `1` null: no body.
//! - `2` bool: one byte, `0` or `1`.
//! - `3` int: the eight bytes of [`encode_i64_key`].
//! - `4` float: the eight bytes of [`encode_f64_key`].
//! - `5` string: a big-endian `u32` length, then that many UTF-8 bytes.
//! - `6` bytes: a big-endian `u32` length, then that many bytes.
//! - `7` decimal: a big-endian `i128` coefficient and a scale byte.
//! - `8` timestamp: a big-endian `i64` of UTC microseconds.
//! - `9` date: a big-endian `i32` of days since 1970-01-01.
//! - `10` local date-time: a big-endian `i64` of civil microseconds.
//! - `11` duration: a big-endian `i64` of microseconds.
//! - `12` list: a big-endian `u32` count, then that many cells.
//! - `13` map: a big-endian `u32` count, then that many string-cell and
//!   value-cell pairs, sorted by key.
//!
//! Tag `0` and tags above `13` are invalid. [`decode_value`] reads exactly one
//! value and rejects trailing bytes. Nesting deeper than [`MAX_VALUE_DEPTH`]
//! is rejected.
//!
//! # Key format
//!
//! [`encode_value_key`] writes a group tag, then a body whose unsigned byte
//! order equals [`index_cmp`]:
//!
//! - `1` null, `2` bool, `3` number, `4` timestamp, `5` date,
//!   `6` local date-time, `7` duration, `8` string, `9` bytes, `10` list,
//!   `11` map.
//!
//! Numbers use one order-preserving decimal encoding. Strings and bytes escape
//! `0x00` as `0x00 0xFF` and end with `0x00 0x01`. Lists and maps concatenate
//! element keys and end with `0x00`. No encoded key is a prefix of another.
//! Decoding a numeric key returns a canonical integer, decimal, or float of
//! the same exact value, not necessarily the original variant.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use crate::error::CoreError;
use crate::numeric::{
    Canonical, Decimal, decode_exact, encode_exact, exact_decimal, exact_f64, exact_i64,
    exact_to_canonical,
};
use crate::temporal::{
    Date, Duration, LocalDateTime, Timestamp, decode_i32_key, encode_i32_key, read_i32, read_i64,
};

const TAG_NULL: u8 = 1;
const TAG_BOOL: u8 = 2;
const TAG_INT: u8 = 3;
const TAG_FLOAT: u8 = 4;
const TAG_STRING: u8 = 5;
const TAG_BYTES: u8 = 6;
const TAG_DECIMAL: u8 = 7;
const TAG_TIMESTAMP: u8 = 8;
const TAG_DATE: u8 = 9;
const TAG_LOCAL: u8 = 10;
const TAG_DURATION: u8 = 11;
const TAG_LIST: u8 = 12;
const TAG_MAP: u8 = 13;

const KEY_NULL: u8 = 1;
const KEY_BOOL: u8 = 2;
const KEY_NUMBER: u8 = 3;
const KEY_TIMESTAMP: u8 = 4;
const KEY_DATE: u8 = 5;
const KEY_LOCAL: u8 = 6;
const KEY_DURATION: u8 = 7;
const KEY_STRING: u8 = 8;
const KEY_BYTES: u8 = 9;
const KEY_LIST: u8 = 10;
const KEY_MAP: u8 = 11;

const SIGN_BIT: u64 = 1 << 63;
const LEN_PREFIX: usize = 4;
const ESCAPE: u8 = 0x00;
const ESCAPED_ZERO: u8 = 0xFF;
const TERMINATOR: u8 = 0x01;

/// Maximum nesting depth of a list or map, counting the outermost value as 1.
pub const MAX_VALUE_DEPTH: u32 = 32;

/// One property value.
///
/// `Float` holds any `f64`. Encoders reject NaN, so a NaN value cannot be
/// stored. `-0.0` is stored as `+0.0`.
///
/// # Examples
///
/// ```
/// use pizzlydb_core::{Value, ValueType};
///
/// let absent: Option<Value> = None;
/// let null = Some(Value::Null);
/// assert_ne!(absent, null);
/// assert_eq!(Value::from(7_i64).value_type(), Some(ValueType::Int));
/// assert_eq!(Value::Null.value_type(), None);
/// ```
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// Explicit null.
    Null,
    /// Boolean.
    Bool(bool),
    /// Signed 64-bit integer.
    Int(i64),
    /// Finite or infinite 64-bit float. NaN does not encode.
    Float(f64),
    /// Exact decimal.
    Decimal(Decimal),
    /// UTF-8 string.
    String(String),
    /// Byte string.
    Bytes(Vec<u8>),
    /// UTC instant in microseconds.
    Timestamp(Timestamp),
    /// Calendar day.
    Date(Date),
    /// Civil date and time without an offset.
    LocalDateTime(LocalDateTime),
    /// Signed duration in microseconds.
    Duration(Duration),
    /// Ordered list. Elements may have different types.
    List(Vec<Value>),
    /// Map from strings to values. Keys are unique and ordered by bytes.
    Map(BTreeMap<String, Value>),
}

impl Value {
    /// Map from `pairs`.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::InvalidValue`] when a key is repeated.
    ///
    /// # Examples
    ///
    /// ```
    /// use pizzlydb_core::Value;
    ///
    /// let map = Value::try_map([("b", Value::Int(1)), ("a", Value::from("x"))]).unwrap();
    /// assert!(matches!(map, Value::Map(_)));
    /// assert!(Value::try_map([("a", Value::Null), ("a", Value::Int(1))]).is_err());
    /// ```
    pub fn try_map(
        pairs: impl IntoIterator<Item = (impl Into<String>, Value)>,
    ) -> Result<Self, CoreError> {
        let mut map = BTreeMap::new();
        for (key, value) in pairs {
            if map.insert(key.into(), value).is_some() {
                return Err(CoreError::InvalidValue);
            }
        }
        Ok(Self::Map(map))
    }

    /// Type of a non-null value, or [`None`] for [`Value::Null`].
    ///
    /// # Examples
    ///
    /// ```
    /// use pizzlydb_core::{Value, ValueType};
    ///
    /// assert_eq!(Value::from("a").value_type(), Some(ValueType::String));
    /// assert_eq!(Value::Null.value_type(), None);
    /// ```
    #[must_use]
    pub const fn value_type(&self) -> Option<ValueType> {
        match self {
            Self::Null => None,
            Self::Bool(_) => Some(ValueType::Bool),
            Self::Int(_) => Some(ValueType::Int),
            Self::Float(_) => Some(ValueType::Float),
            Self::Decimal(_) => Some(ValueType::Decimal),
            Self::String(_) => Some(ValueType::String),
            Self::Bytes(_) => Some(ValueType::Bytes),
            Self::Timestamp(_) => Some(ValueType::Timestamp),
            Self::Date(_) => Some(ValueType::Date),
            Self::LocalDateTime(_) => Some(ValueType::LocalDateTime),
            Self::Duration(_) => Some(ValueType::Duration),
            Self::List(_) => Some(ValueType::List),
            Self::Map(_) => Some(ValueType::Map),
        }
    }
}

impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Self::Int(value)
    }
}

impl From<f64> for Value {
    fn from(value: f64) -> Self {
        Self::Float(value)
    }
}

impl From<Decimal> for Value {
    fn from(value: Decimal) -> Self {
        Self::Decimal(value)
    }
}

impl From<String> for Value {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

impl From<Vec<u8>> for Value {
    fn from(value: Vec<u8>) -> Self {
        Self::Bytes(value)
    }
}

impl From<&[u8]> for Value {
    fn from(value: &[u8]) -> Self {
        Self::Bytes(value.to_vec())
    }
}

impl From<Timestamp> for Value {
    fn from(value: Timestamp) -> Self {
        Self::Timestamp(value)
    }
}

impl From<Date> for Value {
    fn from(value: Date) -> Self {
        Self::Date(value)
    }
}

impl From<LocalDateTime> for Value {
    fn from(value: LocalDateTime) -> Self {
        Self::LocalDateTime(value)
    }
}

impl From<Duration> for Value {
    fn from(value: Duration) -> Self {
        Self::Duration(value)
    }
}

/// Type of a non-null [`Value`].
///
/// [`Self::tag`] is the cell tag of the type.
///
/// # Examples
///
/// ```
/// use pizzlydb_core::ValueType;
///
/// assert_eq!(ValueType::from_tag(ValueType::Float.tag()), Ok(ValueType::Float));
/// assert!(ValueType::from_tag(0).is_err());
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ValueType {
    /// [`Value::Bool`].
    Bool,
    /// [`Value::Int`].
    Int,
    /// [`Value::Float`].
    Float,
    /// [`Value::Decimal`].
    Decimal,
    /// [`Value::String`].
    String,
    /// [`Value::Bytes`].
    Bytes,
    /// [`Value::Timestamp`].
    Timestamp,
    /// [`Value::Date`].
    Date,
    /// [`Value::LocalDateTime`].
    LocalDateTime,
    /// [`Value::Duration`].
    Duration,
    /// [`Value::List`].
    List,
    /// [`Value::Map`].
    Map,
}

impl ValueType {
    /// Cell tag of this type.
    ///
    /// # Examples
    ///
    /// ```
    /// use pizzlydb_core::ValueType;
    ///
    /// assert!(ValueType::Bool.tag() < ValueType::Bytes.tag());
    /// ```
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            Self::Bool => TAG_BOOL,
            Self::Int => TAG_INT,
            Self::Float => TAG_FLOAT,
            Self::Decimal => TAG_DECIMAL,
            Self::String => TAG_STRING,
            Self::Bytes => TAG_BYTES,
            Self::Timestamp => TAG_TIMESTAMP,
            Self::Date => TAG_DATE,
            Self::LocalDateTime => TAG_LOCAL,
            Self::Duration => TAG_DURATION,
            Self::List => TAG_LIST,
            Self::Map => TAG_MAP,
        }
    }

    /// Type named by a cell tag.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::InvalidValue`] for the null tag and for a byte
    /// that is not a tag.
    ///
    /// # Examples
    ///
    /// ```
    /// use pizzlydb_core::{CoreError, ValueType};
    ///
    /// assert_eq!(ValueType::from_tag(3), Ok(ValueType::Int));
    /// assert_eq!(ValueType::from_tag(1), Err(CoreError::InvalidValue));
    /// ```
    pub const fn from_tag(tag: u8) -> Result<Self, CoreError> {
        match tag {
            TAG_BOOL => Ok(Self::Bool),
            TAG_INT => Ok(Self::Int),
            TAG_FLOAT => Ok(Self::Float),
            TAG_DECIMAL => Ok(Self::Decimal),
            TAG_STRING => Ok(Self::String),
            TAG_BYTES => Ok(Self::Bytes),
            TAG_TIMESTAMP => Ok(Self::Timestamp),
            TAG_DATE => Ok(Self::Date),
            TAG_LOCAL => Ok(Self::LocalDateTime),
            TAG_DURATION => Ok(Self::Duration),
            TAG_LIST => Ok(Self::List),
            TAG_MAP => Ok(Self::Map),
            _ => Err(CoreError::InvalidValue),
        }
    }
}

/// Total order of index keys.
///
/// Numeric values compare by exact mathematical value. `Int(1)`, `Float(1.0)`,
/// and `Decimal` `1` compare equal. Null sorts first. NaN compares greater
/// than every encodable value and equal to NaN; it still cannot be encoded.
///
/// # Examples
///
/// ```
/// use std::cmp::Ordering;
///
/// use pizzlydb_core::{Decimal, Value, index_cmp};
///
/// assert_eq!(index_cmp(&Value::Float(1.5), &Value::Int(2)), Ordering::Less);
/// let decimal = Value::Decimal(Decimal::from_str_exact("1.50").unwrap());
/// assert_eq!(index_cmp(&decimal, &Value::Int(1)), Ordering::Greater);
/// assert_eq!(index_cmp(&Value::Int(1), &Value::Float(1.0)), Ordering::Equal);
/// ```
#[must_use]
pub fn index_cmp(left: &Value, right: &Value) -> Ordering {
    match (value_key(left), value_key(right)) {
        (Ok(left), Ok(right)) => left.cmp(&right),
        (Err(_), Err(_)) => Ordering::Equal,
        (Err(_), Ok(_)) => Ordering::Greater,
        (Ok(_), Err(_)) => Ordering::Less,
    }
}

/// Three-valued comparison.
///
/// [`None`] means unknown: either side is null, the values are in different
/// comparison groups, or a float is NaN. Integers, floats, and decimals form
/// one group. Every other type compares only with itself. List and map order
/// is the index order, including null elements.
///
/// # Examples
///
/// ```
/// use std::cmp::Ordering;
///
/// use pizzlydb_core::{Value, predicate_cmp};
///
/// assert_eq!(predicate_cmp(&Value::Int(2), &Value::Float(1.5)), Some(Ordering::Greater));
/// assert_eq!(predicate_cmp(&Value::Null, &Value::Int(1)), None);
/// assert_eq!(predicate_cmp(&Value::Int(1), &Value::from("1")), None);
/// ```
#[must_use]
pub fn predicate_cmp(left: &Value, right: &Value) -> Option<Ordering> {
    if matches!(left, Value::Null) || matches!(right, Value::Null) || !same_group(left, right) {
        return None;
    }
    match (value_key(left), value_key(right)) {
        (Ok(left), Ok(right)) => Some(left.cmp(&right)),
        _ => None,
    }
}

/// Encode `value` so that unsigned byte order equals numeric order.
///
/// The sign bit is flipped and the result is written big-endian.
///
/// # Examples
///
/// ```
/// use pizzlydb_core::encode_i64_key;
///
/// assert!(encode_i64_key(-1) < encode_i64_key(0));
/// assert_eq!(encode_i64_key(i64::MIN), [0; 8]);
/// assert_eq!(encode_i64_key(i64::MAX), [0xff; 8]);
/// ```
#[must_use]
pub const fn encode_i64_key(value: i64) -> [u8; 8] {
    (value.cast_unsigned() ^ SIGN_BIT).to_be_bytes()
}

/// Decode bytes written by [`encode_i64_key`].
///
/// # Examples
///
/// ```
/// use pizzlydb_core::{decode_i64_key, encode_i64_key};
///
/// assert_eq!(decode_i64_key(encode_i64_key(-5)), -5);
/// ```
#[must_use]
pub const fn decode_i64_key(bytes: [u8; 8]) -> i64 {
    (u64::from_be_bytes(bytes) ^ SIGN_BIT).cast_signed()
}

/// Encode `value` so that unsigned byte order equals numeric order, from
/// negative infinity to positive infinity.
///
/// `-0.0` encodes as `+0.0`.
///
/// # Errors
///
/// Returns [`CoreError::InvalidValue`] for NaN.
///
/// # Examples
///
/// ```
/// use pizzlydb_core::encode_f64_key;
///
/// assert!(encode_f64_key(-1.5).unwrap() < encode_f64_key(0.25).unwrap());
/// assert_eq!(encode_f64_key(-0.0), encode_f64_key(0.0));
/// assert!(encode_f64_key(f64::NAN).is_err());
/// ```
pub fn encode_f64_key(value: f64) -> Result<[u8; 8], CoreError> {
    if value.is_nan() {
        return Err(CoreError::InvalidValue);
    }
    let canonical = if value == 0.0 { 0.0 } else { value };
    let bits = canonical.to_bits();
    let ordered = if bits & SIGN_BIT == 0 {
        bits ^ SIGN_BIT
    } else {
        !bits
    };
    Ok(ordered.to_be_bytes())
}

/// Decode bytes written by [`encode_f64_key`].
///
/// # Errors
///
/// Returns [`CoreError::InvalidValue`] when the bytes decode to NaN or to
/// `-0.0`, which [`encode_f64_key`] never writes.
///
/// # Examples
///
/// ```
/// use pizzlydb_core::{decode_f64_key, encode_f64_key};
///
/// let bytes = encode_f64_key(f64::NEG_INFINITY).unwrap();
/// assert_eq!(decode_f64_key(bytes), Ok(f64::NEG_INFINITY));
/// assert!(decode_f64_key([0xff; 8]).is_err());
/// ```
pub fn decode_f64_key(bytes: [u8; 8]) -> Result<f64, CoreError> {
    let ordered = u64::from_be_bytes(bytes);
    let bits = if ordered & SIGN_BIT == 0 {
        !ordered
    } else {
        ordered ^ SIGN_BIT
    };
    let value = f64::from_bits(bits);
    if value.is_nan() || bits == (-0.0_f64).to_bits() {
        return Err(CoreError::InvalidValue);
    }
    Ok(value)
}

/// Encode `value` in the cell format described in the module docs.
///
/// # Errors
///
/// Returns [`CoreError::InvalidValue`] for NaN, a string or byte string longer
/// than `u32::MAX` bytes, a decimal that cannot be represented, or a value
/// nested deeper than [`MAX_VALUE_DEPTH`].
///
/// # Examples
///
/// ```
/// use pizzlydb_core::{Value, decode_value, encode_value};
///
/// let bytes = encode_value(&Value::from("hi")).unwrap();
/// assert_eq!(bytes, [5, 0, 0, 0, 2, b'h', b'i']);
/// assert_eq!(decode_value(&bytes), Ok(Value::from("hi")));
/// assert_eq!(encode_value(&Value::Null).unwrap(), [1]);
/// ```
pub fn encode_value(value: &Value) -> Result<Vec<u8>, CoreError> {
    let mut out = Vec::new();
    write_cell(value, &mut out, 1)?;
    Ok(out)
}

/// Decode one value written by [`encode_value`].
///
/// # Errors
///
/// Returns [`CoreError::Truncated`] when `bytes` ends inside the value, and
/// [`CoreError::InvalidValue`] for an unknown tag, a bool other than `0` or
/// `1`, a float that is NaN or `-0.0`, invalid UTF-8, a malformed collection,
/// or trailing bytes.
///
/// # Examples
///
/// ```
/// use pizzlydb_core::{CoreError, Value, decode_value, encode_value};
///
/// let bytes = encode_value(&Value::Int(-3)).unwrap();
/// assert_eq!(decode_value(&bytes), Ok(Value::Int(-3)));
/// assert_eq!(decode_value(&bytes[..4]), Err(CoreError::Truncated));
/// assert_eq!(decode_value(&[5, 0xff, 0xff, 0xff, 0xff]), Err(CoreError::Truncated));
/// ```
pub fn decode_value(bytes: &[u8]) -> Result<Value, CoreError> {
    let (value, used) = read_cell(bytes, 1)?;
    if used != bytes.len() {
        return Err(CoreError::InvalidValue);
    }
    Ok(value)
}

/// Append the key format of `value` to `out`.
///
/// On error `out` is unchanged.
///
/// # Errors
///
/// Returns [`CoreError::InvalidValue`] for NaN or a value nested deeper than
/// [`MAX_VALUE_DEPTH`].
///
/// # Examples
///
/// ```
/// use pizzlydb_core::{Value, encode_value_key};
///
/// let mut a = Vec::new();
/// let mut ab = Vec::new();
/// encode_value_key(&Value::from("a"), &mut a).unwrap();
/// encode_value_key(&Value::from("ab"), &mut ab).unwrap();
/// assert!(a < ab);
/// assert!(!ab.starts_with(&a));
/// ```
pub fn encode_value_key(value: &Value, out: &mut Vec<u8>) -> Result<(), CoreError> {
    let mut encoded = Vec::new();
    write_key(value, &mut encoded, 1)?;
    out.extend_from_slice(&encoded);
    Ok(())
}

/// Decode one value written by [`encode_value_key`] at the start of `bytes`.
///
/// Returns the value and the number of bytes it used. A numeric key decodes
/// to the canonical integer, decimal, or float of that exact value.
///
/// # Errors
///
/// Returns [`CoreError::Truncated`] when `bytes` ends inside the value, and
/// [`CoreError::InvalidValue`] for an unknown tag, a bad escape, or invalid
/// UTF-8.
///
/// # Examples
///
/// ```
/// use std::cmp::Ordering;
///
/// use pizzlydb_core::{Value, decode_value_key, encode_value_key, index_cmp};
///
/// let mut key = Vec::new();
/// encode_value_key(&Value::from("a\0b"), &mut key).unwrap();
/// let used = key.len();
/// key.extend_from_slice(b"suffix");
/// assert_eq!(decode_value_key(&key), Ok((Value::from("a\0b"), used)));
///
/// let mut number = Vec::new();
/// encode_value_key(&Value::Float(1.0), &mut number).unwrap();
/// let (decoded, used) = decode_value_key(&number).unwrap();
/// assert_eq!(used, number.len());
/// assert_eq!(index_cmp(&decoded, &Value::Int(1)), Ordering::Equal);
/// ```
pub fn decode_value_key(bytes: &[u8]) -> Result<(Value, usize), CoreError> {
    read_key(bytes, 1)
}

fn same_group(left: &Value, right: &Value) -> bool {
    matches!(
        (left, right),
        (
            Value::Int(_) | Value::Float(_) | Value::Decimal(_),
            Value::Int(_) | Value::Float(_) | Value::Decimal(_)
        )
    ) || left.value_type() == right.value_type()
}

fn value_key(value: &Value) -> Result<Vec<u8>, CoreError> {
    let mut out = Vec::new();
    encode_value_key(value, &mut out)?;
    Ok(out)
}

fn write_cell(value: &Value, out: &mut Vec<u8>, depth: u32) -> Result<(), CoreError> {
    if depth > MAX_VALUE_DEPTH {
        return Err(CoreError::InvalidValue);
    }
    match value {
        Value::Null => out.push(TAG_NULL),
        Value::Bool(bit) => out.extend_from_slice(&[TAG_BOOL, u8::from(*bit)]),
        Value::Int(int) => {
            out.push(TAG_INT);
            out.extend_from_slice(&encode_i64_key(*int));
        }
        Value::Float(float) => {
            out.push(TAG_FLOAT);
            out.extend_from_slice(&encode_f64_key(*float)?);
        }
        Value::Decimal(decimal) => {
            out.push(TAG_DECIMAL);
            out.extend_from_slice(&decimal.coefficient().to_be_bytes());
            out.push(decimal.scale());
        }
        Value::String(text) => write_bytes(out, TAG_STRING, text.as_bytes())?,
        Value::Bytes(bytes) => write_bytes(out, TAG_BYTES, bytes)?,
        Value::Timestamp(instant) => {
            out.push(TAG_TIMESTAMP);
            out.extend_from_slice(&instant.unix_micros().to_be_bytes());
        }
        Value::Date(date) => {
            out.push(TAG_DATE);
            out.extend_from_slice(&date.unix_days().to_be_bytes());
        }
        Value::LocalDateTime(local) => {
            out.push(TAG_LOCAL);
            out.extend_from_slice(&local.unix_micros().to_be_bytes());
        }
        Value::Duration(span) => {
            out.push(TAG_DURATION);
            out.extend_from_slice(&span.micros().to_be_bytes());
        }
        Value::List(items) => {
            let count = u32::try_from(items.len()).map_err(|_| CoreError::InvalidValue)?;
            out.push(TAG_LIST);
            out.extend_from_slice(&count.to_be_bytes());
            for item in items {
                write_cell(item, out, depth + 1)?;
            }
        }
        Value::Map(entries) => {
            let count = u32::try_from(entries.len()).map_err(|_| CoreError::InvalidValue)?;
            out.push(TAG_MAP);
            out.extend_from_slice(&count.to_be_bytes());
            for (key, item) in entries {
                write_cell(&Value::String(key.clone()), out, depth + 1)?;
                write_cell(item, out, depth + 1)?;
            }
        }
    }
    Ok(())
}

fn write_bytes(out: &mut Vec<u8>, tag: u8, payload: &[u8]) -> Result<(), CoreError> {
    let len = u32::try_from(payload.len()).map_err(|_| CoreError::InvalidValue)?;
    out.push(tag);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(payload);
    Ok(())
}

fn read_cell(bytes: &[u8], depth: u32) -> Result<(Value, usize), CoreError> {
    if depth > MAX_VALUE_DEPTH {
        return Err(CoreError::InvalidValue);
    }
    let (&tag, rest) = bytes.split_first().ok_or(CoreError::Truncated)?;
    match tag {
        TAG_NULL => Ok((Value::Null, 1)),
        TAG_BOOL => {
            let byte = *rest.first().ok_or(CoreError::Truncated)?;
            Ok((decode_bool(byte)?, 2))
        }
        TAG_INT => Ok((Value::Int(decode_i64_key(fixed8(rest)?)), 9)),
        TAG_FLOAT => Ok((Value::Float(decode_f64_key(fixed8(rest)?)?), 9)),
        TAG_DECIMAL => {
            if rest.len() < 17 {
                return Err(CoreError::Truncated);
            }
            let mut coeff = [0_u8; 16];
            coeff.copy_from_slice(&rest[..16]);
            let scale = rest[16];
            let decimal = Decimal::new(i128::from_be_bytes(coeff), scale)?;
            Ok((Value::Decimal(decimal), 18))
        }
        TAG_STRING => {
            let (payload, used) = length_prefixed(rest)?;
            let text = std::str::from_utf8(payload).map_err(|_| CoreError::InvalidValue)?;
            Ok((Value::String(text.to_owned()), 1 + LEN_PREFIX + used))
        }
        TAG_BYTES => {
            let (payload, used) = length_prefixed(rest)?;
            Ok((Value::Bytes(payload.to_vec()), 1 + LEN_PREFIX + used))
        }
        TAG_TIMESTAMP => Ok((
            Value::Timestamp(Timestamp::from_unix_micros(read_i64(rest)?)),
            9,
        )),
        TAG_DATE => Ok((Value::Date(Date::from_unix_days(read_i32(rest)?)), 5)),
        TAG_LOCAL => Ok((
            Value::LocalDateTime(LocalDateTime::from_unix_micros(read_i64(rest)?)),
            9,
        )),
        TAG_DURATION => Ok((Value::Duration(Duration::from_micros(read_i64(rest)?)), 9)),
        TAG_LIST => read_list(rest, depth),
        TAG_MAP => read_map(rest, depth),
        _ => Err(CoreError::InvalidValue),
    }
}

fn read_list(rest: &[u8], depth: u32) -> Result<(Value, usize), CoreError> {
    let count = read_count(rest)?;
    let mut at = LEN_PREFIX;
    let mut items = Vec::new();
    for _ in 0..count {
        let (item, used) = read_cell(rest.get(at..).ok_or(CoreError::Truncated)?, depth + 1)?;
        items.push(item);
        at = at.checked_add(used).ok_or(CoreError::InvalidValue)?;
    }
    Ok((Value::List(items), 1 + at))
}

fn read_map(rest: &[u8], depth: u32) -> Result<(Value, usize), CoreError> {
    let count = read_count(rest)?;
    let mut at = LEN_PREFIX;
    let mut entries = BTreeMap::new();
    let mut previous: Option<String> = None;
    for _ in 0..count {
        let (key, key_used) = read_cell(rest.get(at..).ok_or(CoreError::Truncated)?, depth + 1)?;
        at = at.checked_add(key_used).ok_or(CoreError::InvalidValue)?;
        let Value::String(key) = key else {
            return Err(CoreError::InvalidValue);
        };
        if previous
            .as_ref()
            .is_some_and(|seen| key.as_str() <= seen.as_str())
        {
            return Err(CoreError::InvalidValue);
        }
        let (item, item_used) = read_cell(rest.get(at..).ok_or(CoreError::Truncated)?, depth + 1)?;
        at = at.checked_add(item_used).ok_or(CoreError::InvalidValue)?;
        previous = Some(key.clone());
        entries.insert(key, item);
    }
    Ok((Value::Map(entries), 1 + at))
}

fn read_count(rest: &[u8]) -> Result<usize, CoreError> {
    let prefix = rest.get(..LEN_PREFIX).ok_or(CoreError::Truncated)?;
    let mut buf = [0_u8; 4];
    buf.copy_from_slice(prefix);
    let count = u32::from_be_bytes(buf) as usize;
    if count > rest.len() {
        return Err(CoreError::Truncated);
    }
    Ok(count)
}

fn write_key(value: &Value, out: &mut Vec<u8>, depth: u32) -> Result<(), CoreError> {
    if depth > MAX_VALUE_DEPTH {
        return Err(CoreError::InvalidValue);
    }
    match value {
        Value::Null => out.push(KEY_NULL),
        Value::Bool(bit) => out.extend_from_slice(&[KEY_BOOL, u8::from(*bit)]),
        Value::Int(int) => {
            out.push(KEY_NUMBER);
            encode_exact(&exact_i64(*int), out)?;
        }
        Value::Float(float) => {
            out.push(KEY_NUMBER);
            encode_exact(&exact_f64(*float)?, out)?;
        }
        Value::Decimal(decimal) => {
            out.push(KEY_NUMBER);
            encode_exact(&exact_decimal(*decimal), out)?;
        }
        Value::String(text) => write_escaped(out, KEY_STRING, text.as_bytes()),
        Value::Bytes(bytes) => write_escaped(out, KEY_BYTES, bytes),
        Value::Timestamp(instant) => {
            out.push(KEY_TIMESTAMP);
            out.extend_from_slice(&encode_i64_key(instant.unix_micros()));
        }
        Value::Date(date) => {
            out.push(KEY_DATE);
            out.extend_from_slice(&encode_i32_key(date.unix_days()));
        }
        Value::LocalDateTime(local) => {
            out.push(KEY_LOCAL);
            out.extend_from_slice(&encode_i64_key(local.unix_micros()));
        }
        Value::Duration(span) => {
            out.push(KEY_DURATION);
            out.extend_from_slice(&encode_i64_key(span.micros()));
        }
        Value::List(items) => {
            out.push(KEY_LIST);
            for item in items {
                write_key(item, out, depth + 1)?;
            }
            out.push(0);
        }
        Value::Map(entries) => {
            out.push(KEY_MAP);
            for (key, item) in entries {
                write_key(&Value::String(key.clone()), out, depth + 1)?;
                write_key(item, out, depth + 1)?;
            }
            out.push(0);
        }
    }
    Ok(())
}

fn write_escaped(out: &mut Vec<u8>, tag: u8, payload: &[u8]) {
    out.push(tag);
    for &byte in payload {
        out.push(byte);
        if byte == ESCAPE {
            out.push(ESCAPED_ZERO);
        }
    }
    out.extend_from_slice(&[ESCAPE, TERMINATOR]);
}

fn read_key(bytes: &[u8], depth: u32) -> Result<(Value, usize), CoreError> {
    if depth > MAX_VALUE_DEPTH {
        return Err(CoreError::InvalidValue);
    }
    let (&tag, body) = bytes.split_first().ok_or(CoreError::Truncated)?;
    match tag {
        KEY_NULL => Ok((Value::Null, 1)),
        KEY_BOOL => {
            let byte = *body.first().ok_or(CoreError::Truncated)?;
            Ok((decode_bool(byte)?, 2))
        }
        KEY_NUMBER => {
            let (exact, used) = decode_exact(body)?;
            let value = match exact_to_canonical(&exact)? {
                Canonical::Int(int) => Value::Int(int),
                Canonical::Decimal(decimal) => Value::Decimal(decimal),
                Canonical::Float(float) => Value::Float(float),
            };
            Ok((value, used + 1))
        }
        KEY_TIMESTAMP => Ok((
            Value::Timestamp(Timestamp::from_unix_micros(decode_i64_key(prefix8(body)?))),
            9,
        )),
        KEY_DATE => Ok((
            Value::Date(Date::from_unix_days(decode_i32_key(prefix4(body)?))),
            5,
        )),
        KEY_LOCAL => Ok((
            Value::LocalDateTime(LocalDateTime::from_unix_micros(decode_i64_key(prefix8(
                body,
            )?))),
            9,
        )),
        KEY_DURATION => Ok((
            Value::Duration(Duration::from_micros(decode_i64_key(prefix8(body)?))),
            9,
        )),
        KEY_STRING => {
            let (raw, used) = unescape(body)?;
            let text = String::from_utf8(raw).map_err(|_| CoreError::InvalidValue)?;
            Ok((Value::String(text), used + 1))
        }
        KEY_BYTES => {
            let (raw, used) = unescape(body)?;
            Ok((Value::Bytes(raw), used + 1))
        }
        KEY_LIST => read_key_list(body, depth),
        KEY_MAP => read_key_map(body, depth),
        _ => Err(CoreError::InvalidValue),
    }
}

fn read_key_list(body: &[u8], depth: u32) -> Result<(Value, usize), CoreError> {
    let mut items = Vec::new();
    let mut at = 0;
    while at < body.len() {
        if body[at] == 0 {
            return Ok((Value::List(items), at + 2));
        }
        let (item, used) = read_key(&body[at..], depth + 1)?;
        items.push(item);
        at = at.checked_add(used).ok_or(CoreError::InvalidValue)?;
    }
    Err(CoreError::Truncated)
}

fn read_key_map(body: &[u8], depth: u32) -> Result<(Value, usize), CoreError> {
    let mut entries = BTreeMap::new();
    let mut at = 0;
    let mut previous: Option<String> = None;
    while at < body.len() {
        if body[at] == 0 {
            return Ok((Value::Map(entries), at + 2));
        }
        let (key, key_used) = read_key(&body[at..], depth + 1)?;
        at = at.checked_add(key_used).ok_or(CoreError::InvalidValue)?;
        let Value::String(key) = key else {
            return Err(CoreError::InvalidValue);
        };
        if previous
            .as_ref()
            .is_some_and(|seen| key.as_str() <= seen.as_str())
        {
            return Err(CoreError::InvalidValue);
        }
        let (item, item_used) = read_key(body.get(at..).ok_or(CoreError::Truncated)?, depth + 1)?;
        at = at.checked_add(item_used).ok_or(CoreError::InvalidValue)?;
        previous = Some(key.clone());
        entries.insert(key, item);
    }
    Err(CoreError::Truncated)
}

fn length_prefixed(body: &[u8]) -> Result<(&[u8], usize), CoreError> {
    let prefix = body.get(..LEN_PREFIX).ok_or(CoreError::Truncated)?;
    let mut len_bytes = [0_u8; LEN_PREFIX];
    len_bytes.copy_from_slice(prefix);
    let len = u32::from_be_bytes(len_bytes) as usize;
    let payload = body.get(LEN_PREFIX..).ok_or(CoreError::Truncated)?;
    if len > payload.len() {
        return Err(CoreError::Truncated);
    }
    Ok((&payload[..len], len))
}

fn fixed8(body: &[u8]) -> Result<[u8; 8], CoreError> {
    if body.len() < 8 {
        return Err(CoreError::Truncated);
    }
    prefix8(body)
}

fn prefix8(body: &[u8]) -> Result<[u8; 8], CoreError> {
    let slice = body.get(..8).ok_or(CoreError::Truncated)?;
    let mut out = [0_u8; 8];
    out.copy_from_slice(slice);
    Ok(out)
}

fn prefix4(body: &[u8]) -> Result<[u8; 4], CoreError> {
    let slice = body.get(..4).ok_or(CoreError::Truncated)?;
    let mut out = [0_u8; 4];
    out.copy_from_slice(slice);
    Ok(out)
}

const fn decode_bool(byte: u8) -> Result<Value, CoreError> {
    match byte {
        0 => Ok(Value::Bool(false)),
        1 => Ok(Value::Bool(true)),
        _ => Err(CoreError::InvalidValue),
    }
}

fn unescape(body: &[u8]) -> Result<(Vec<u8>, usize), CoreError> {
    let mut out = Vec::new();
    let mut index = 0;
    while index < body.len() {
        let byte = body[index];
        if byte != ESCAPE {
            out.push(byte);
            index += 1;
            continue;
        }
        match body.get(index + 1) {
            Some(&ESCAPED_ZERO) => out.push(0),
            Some(&TERMINATOR) => return Ok((out, index + 2)),
            Some(_) => return Err(CoreError::InvalidValue),
            None => return Err(CoreError::Truncated),
        }
        index += 2;
    }
    Err(CoreError::Truncated)
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use super::{
        Value, decode_f64_key, decode_i64_key, decode_value, decode_value_key, encode_f64_key,
        encode_i64_key, encode_value, encode_value_key, index_cmp,
    };
    use crate::error::CoreError;
    use crate::numeric::Decimal;
    use crate::temporal::{Date, Duration, LocalDateTime, Timestamp};

    fn samples() -> Vec<Value> {
        vec![
            Value::Null,
            Value::Bool(false),
            Value::Bool(true),
            Value::Int(i64::MIN),
            Value::Int(-1),
            Value::Int(0),
            Value::Int(i64::MAX),
            Value::Float(f64::NEG_INFINITY),
            Value::Float(-1.5),
            Value::Float(0.0),
            Value::Float(f64::MIN_POSITIVE),
            Value::Float(f64::INFINITY),
            Value::Decimal(Decimal::from_str_exact("-1.50").unwrap()),
            Value::Decimal(Decimal::from_str_exact("2").unwrap()),
            Value::from(""),
            Value::from("a\0b"),
            Value::from("é"),
            Value::Bytes(vec![]),
            Value::Bytes(vec![0, 0, 0xff, 1]),
            Value::Timestamp(Timestamp::from_unix_micros(-1)),
            Value::Date(Date::from_unix_days(-10)),
            Value::LocalDateTime(LocalDateTime::from_unix_micros(5)),
            Value::Duration(Duration::from_micros(-20)),
            Value::List(vec![Value::Int(1), Value::from("a")]),
            Value::try_map([("a", Value::Int(1)), ("b", Value::Null)]).unwrap(),
        ]
    }

    fn keys_equal(left: &Value, right: &Value) -> bool {
        index_cmp(left, right) == Ordering::Equal
    }

    #[test]
    fn cell_round_trip() {
        for value in samples() {
            let bytes = encode_value(&value).unwrap();
            assert_eq!(decode_value(&bytes), Ok(value));
        }
    }

    #[test]
    fn key_round_trip_matches_the_index_order() {
        for value in samples() {
            let mut bytes = Vec::new();
            encode_value_key(&value, &mut bytes).unwrap();
            let used = bytes.len();
            bytes.extend_from_slice(&[0xAA, 0x00]);
            let (decoded, len) = decode_value_key(&bytes).unwrap();
            assert_eq!(len, used);
            assert!(keys_equal(&decoded, &value), "{value:?} -> {decoded:?}");
        }
    }

    #[test]
    fn mixed_numeric_order_matches_the_key_bytes() {
        let values = [
            Value::Float(f64::NEG_INFINITY),
            Value::Int(i64::MIN),
            Value::Decimal(Decimal::from_str_exact("-1.50").unwrap()),
            Value::Float(-1.5),
            Value::Int(-1),
            Value::Float(0.0),
            Value::Int(0),
            Value::Decimal(Decimal::from_str_exact("1.00").unwrap()),
            Value::Float(1.0),
            Value::Int(1),
            Value::Float(1.5),
            Value::Decimal(Decimal::from_str_exact("1.50").unwrap()),
            Value::Int(2),
            Value::Float(f64::INFINITY),
        ];
        for left in &values {
            for right in &values {
                let mut left_key = Vec::new();
                let mut right_key = Vec::new();
                encode_value_key(left, &mut left_key).unwrap();
                encode_value_key(right, &mut right_key).unwrap();
                assert_eq!(index_cmp(left, right), left_key.cmp(&right_key));
            }
        }
        assert_eq!(
            index_cmp(&Value::Int(2), &Value::Float(1.5)),
            Ordering::Greater
        );
        assert_eq!(
            index_cmp(
                &Value::Decimal(Decimal::from_str_exact("1.50").unwrap()),
                &Value::Int(1)
            ),
            Ordering::Greater
        );
    }

    #[test]
    fn negative_zero_is_canonical() {
        assert_eq!(
            encode_value(&Value::Float(-0.0)).unwrap(),
            encode_value(&Value::Float(0.0)).unwrap()
        );
        let decoded = decode_value(&encode_value(&Value::Float(-0.0)).unwrap()).unwrap();
        assert!(matches!(decoded, Value::Float(value) if value.to_bits() == 0));
        let negative_zero = !(-0.0_f64).to_bits();
        assert_eq!(
            decode_f64_key(negative_zero.to_be_bytes()),
            Err(CoreError::InvalidValue)
        );
    }

    #[test]
    fn nan_is_rejected_everywhere() {
        let nan = Value::Float(f64::NAN);
        assert_eq!(encode_value(&nan), Err(CoreError::InvalidValue));
        let mut out = vec![9];
        assert_eq!(
            encode_value_key(&nan, &mut out),
            Err(CoreError::InvalidValue)
        );
        assert_eq!(out, vec![9]);
        assert_eq!(encode_f64_key(f64::NAN), Err(CoreError::InvalidValue));
    }

    #[test]
    fn i64_extremes_round_trip() {
        for value in [i64::MIN, -1, 0, 1, i64::MAX] {
            assert_eq!(decode_i64_key(encode_i64_key(value)), value);
        }
    }

    #[test]
    fn malformed_cells_are_errors() {
        let cases: [(&[u8], CoreError); 12] = [
            (&[], CoreError::Truncated),
            (&[0], CoreError::InvalidValue),
            (&[14], CoreError::InvalidValue),
            (&[1, 0], CoreError::InvalidValue),
            (&[2], CoreError::Truncated),
            (&[2, 2], CoreError::InvalidValue),
            (&[2, 1, 0], CoreError::InvalidValue),
            (&[3, 0, 0], CoreError::Truncated),
            (&[3, 0, 0, 0, 0, 0, 0, 0, 0, 0], CoreError::InvalidValue),
            (&[5, 0, 0], CoreError::Truncated),
            (&[5, 0, 0, 0, 1, 0xff], CoreError::InvalidValue),
            (&[7], CoreError::Truncated),
        ];
        for (bytes, expected) in cases {
            assert_eq!(decode_value(bytes), Err(expected), "input {bytes:?}");
        }
    }
}
