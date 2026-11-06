//! Scalar values and their byte encodings.
//!
//! [`Value`] is the scalar set stored in a property cell: null, boolean, `i64`,
//! finite `f64`, UTF-8 string, and bytes. Property absence has no encoding.
//! Callers use `Option<Value>`, where `None` is absent and
//! `Some(Value::Null)` is an explicit null, and they omit the key for `None`.
//!
//! Every encoder rejects NaN and writes `-0.0` as `+0.0`. Strings and bytes
//! compare as unsigned bytes. There is no Unicode normalization and no case
//! folding.
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
//!
//! Tag `0` and tags above `6` are invalid, so a zeroed buffer does not decode.
//! [`decode_value`] reads exactly one value. It checks a length prefix against
//! the remaining input before it allocates, and it rejects trailing bytes.
//!
//! # Key format
//!
//! [`encode_value_key`] writes the same tag byte, then a body whose unsigned
//! byte order equals value order within one type:
//!
//! - null: no body.
//! - bool, int, and float: the same body as the cell format.
//! - string and bytes: each `0x00` byte becomes `0x00 0xFF`, and `0x00 0x01`
//!   ends the body.
//!
//! No encoded key is a prefix of another, so bytes appended after an encoded
//! key do not change the order of the values. Values of different types order
//! by tag: null, bool, int, float, string, bytes.

use crate::error::CoreError;

const TAG_NULL: u8 = 1;
const TAG_BOOL: u8 = 2;
const TAG_INT: u8 = 3;
const TAG_FLOAT: u8 = 4;
const TAG_STRING: u8 = 5;
const TAG_BYTES: u8 = 6;

const SIGN_BIT: u64 = 1 << 63;
const LEN_PREFIX: usize = 4;
const ESCAPE: u8 = 0x00;
const ESCAPED_ZERO: u8 = 0xFF;
const TERMINATOR: u8 = 0x01;

/// One scalar property value.
///
/// `Float` holds any `f64`. Encoders reject NaN, so a NaN value cannot be
/// stored.
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
    /// UTF-8 string.
    String(String),
    /// Byte string.
    Bytes(Vec<u8>),
}

impl Value {
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
            Self::String(_) => Some(ValueType::String),
            Self::Bytes(_) => Some(ValueType::Bytes),
        }
    }

    const fn tag(&self) -> u8 {
        match self.value_type() {
            None => TAG_NULL,
            Some(value_type) => value_type.tag(),
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

/// Type of a non-null [`Value`].
///
/// [`Self::tag`] is the tag byte that both encodings write for the type.
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
    /// [`Value::String`].
    String,
    /// [`Value::Bytes`].
    Bytes,
}

impl ValueType {
    /// Tag byte of this type.
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
            Self::String => TAG_STRING,
            Self::Bytes => TAG_BYTES,
        }
    }

    /// Type named by `tag`.
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
            TAG_STRING => Ok(Self::String),
            TAG_BYTES => Ok(Self::Bytes),
            _ => Err(CoreError::InvalidValue),
        }
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
/// Returns [`CoreError::InvalidValue`] for NaN and for a string or byte string
/// longer than `u32::MAX` bytes.
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
    let body = Body::of(value)?;
    let mut out = vec![value.tag()];

    match body {
        Body::Empty => {}
        Body::Byte(byte) => out.push(byte),
        Body::Fixed(fixed) => out.extend_from_slice(&fixed),
        Body::Variable(payload) => {
            let len = u32::try_from(payload.len()).map_err(|_| CoreError::InvalidValue)?;
            out.reserve(LEN_PREFIX + payload.len());
            out.extend_from_slice(&len.to_be_bytes());
            out.extend_from_slice(payload);
        }
    }

    Ok(out)
}

/// Decode one value written by [`encode_value`].
///
/// # Errors
///
/// Returns [`CoreError::Truncated`] when `bytes` ends inside the value, and
/// [`CoreError::InvalidValue`] for an unknown tag, a bool other than `0` or
/// `1`, a float that is NaN or `-0.0`, invalid UTF-8, or trailing bytes.
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
    let (&tag, body) = bytes.split_first().ok_or(CoreError::Truncated)?;
    match tag {
        TAG_NULL => {
            exact(body, 0)?;
            Ok(Value::Null)
        }
        TAG_BOOL => {
            exact(body, 1)?;
            decode_bool(body[0])
        }
        TAG_INT => Ok(Value::Int(decode_i64_key(fixed8(body)?))),
        TAG_FLOAT => Ok(Value::Float(decode_f64_key(fixed8(body)?)?)),
        TAG_STRING => {
            let payload = length_prefixed(body)?;
            let text = std::str::from_utf8(payload).map_err(|_| CoreError::InvalidValue)?;
            Ok(Value::String(text.to_owned()))
        }
        TAG_BYTES => Ok(Value::Bytes(length_prefixed(body)?.to_vec())),
        _ => Err(CoreError::InvalidValue),
    }
}

/// Append the key format of `value` to `out`.
///
/// On error `out` is unchanged.
///
/// # Errors
///
/// Returns [`CoreError::InvalidValue`] for NaN.
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
    let body = Body::of(value)?;
    out.push(value.tag());
    match body {
        Body::Empty => {}
        Body::Byte(byte) => out.push(byte),
        Body::Fixed(fixed) => out.extend_from_slice(&fixed),
        Body::Variable(payload) => {
            out.reserve(payload.len() + 2);
            for &byte in payload {
                out.push(byte);
                if byte == ESCAPE {
                    out.push(ESCAPED_ZERO);
                }
            }
            out.extend_from_slice(&[ESCAPE, TERMINATOR]);
        }
    }
    Ok(())
}

/// Decode one value written by [`encode_value_key`] at the start of `bytes`.
///
/// Returns the value and the number of bytes it used. Bytes after that are
/// not read.
///
/// # Errors
///
/// Returns [`CoreError::Truncated`] when `bytes` ends inside the value, and
/// [`CoreError::InvalidValue`] for an unknown tag, a bool other than `0` or
/// `1`, a float that is NaN or `-0.0`, an invalid escape, or invalid UTF-8.
///
/// # Examples
///
/// ```
/// use pizzlydb_core::{Value, decode_value_key, encode_value_key};
///
/// let mut key = Vec::new();
/// encode_value_key(&Value::from("a\0b"), &mut key).unwrap();
/// let used = key.len();
/// key.extend_from_slice(b"suffix");
/// assert_eq!(decode_value_key(&key), Ok((Value::from("a\0b"), used)));
/// ```
pub fn decode_value_key(bytes: &[u8]) -> Result<(Value, usize), CoreError> {
    let (&tag, body) = bytes.split_first().ok_or(CoreError::Truncated)?;
    match tag {
        TAG_NULL => Ok((Value::Null, 1)),
        TAG_BOOL => {
            let byte = *body.first().ok_or(CoreError::Truncated)?;
            Ok((decode_bool(byte)?, 2))
        }
        TAG_INT => {
            let fixed = prefix8(body)?;
            Ok((Value::Int(decode_i64_key(fixed)), 9))
        }
        TAG_FLOAT => {
            let fixed = prefix8(body)?;
            Ok((Value::Float(decode_f64_key(fixed)?), 9))
        }
        TAG_STRING => {
            let (raw, used) = unescape(body)?;
            let text = String::from_utf8(raw).map_err(|_| CoreError::InvalidValue)?;
            Ok((Value::String(text), used + 1))
        }
        TAG_BYTES => {
            let (raw, used) = unescape(body)?;
            Ok((Value::Bytes(raw), used + 1))
        }
        _ => Err(CoreError::InvalidValue),
    }
}

/// Body bytes shared by both formats, computed before anything is written.
enum Body<'a> {
    Empty,
    Byte(u8),
    Fixed([u8; 8]),
    Variable(&'a [u8]),
}

impl<'a> Body<'a> {
    fn of(value: &'a Value) -> Result<Self, CoreError> {
        Ok(match value {
            Value::Null => Self::Empty,
            Value::Bool(bit) => Self::Byte(u8::from(*bit)),
            Value::Int(int) => Self::Fixed(encode_i64_key(*int)),
            Value::Float(float) => Self::Fixed(encode_f64_key(*float)?),
            Value::String(text) => Self::Variable(text.as_bytes()),
            Value::Bytes(bytes) => Self::Variable(bytes),
        })
    }
}

fn length_prefixed(body: &[u8]) -> Result<&[u8], CoreError> {
    let prefix = body.get(..LEN_PREFIX).ok_or(CoreError::Truncated)?;
    let mut len_bytes = [0u8; LEN_PREFIX];
    len_bytes.copy_from_slice(prefix);

    let len =
        usize::try_from(u32::from_be_bytes(len_bytes)).map_err(|_| CoreError::InvalidValue)?;

    let payload = &body[LEN_PREFIX..];
    if len > payload.len() {
        return Err(CoreError::Truncated);
    }

    exact(payload, len)?;
    Ok(payload)
}

fn exact(body: &[u8], len: usize) -> Result<(), CoreError> {
    match body.len().cmp(&len) {
        std::cmp::Ordering::Less => Err(CoreError::Truncated),
        std::cmp::Ordering::Equal => Ok(()),
        std::cmp::Ordering::Greater => Err(CoreError::InvalidValue),
    }
}

fn fixed8(body: &[u8]) -> Result<[u8; 8], CoreError> {
    exact(body, 8)?;
    prefix8(body)
}

fn prefix8(body: &[u8]) -> Result<[u8; 8], CoreError> {
    let slice = body.get(..8).ok_or(CoreError::Truncated)?;
    let mut out = [0u8; 8];
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
    use super::{
        Value, decode_f64_key, decode_i64_key, decode_value, decode_value_key, encode_f64_key,
        encode_i64_key, encode_value, encode_value_key,
    };
    use crate::error::CoreError;

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
            Value::from(""),
            Value::from("a\0b"),
            Value::from("é"),
            Value::Bytes(vec![]),
            Value::Bytes(vec![0, 0, 0xff, 1]),
        ]
    }

    #[test]
    fn cell_round_trip() {
        for value in samples() {
            let bytes = encode_value(&value).unwrap();
            assert_eq!(decode_value(&bytes), Ok(value));
        }
    }

    #[test]
    fn key_round_trip_reports_length() {
        for value in samples() {
            let mut bytes = Vec::new();
            encode_value_key(&value, &mut bytes).unwrap();
            let used = bytes.len();
            bytes.extend_from_slice(&[0xAA, 0x00]);
            assert_eq!(decode_value_key(&bytes), Ok((value, used)));
        }
    }

    #[test]
    fn negative_zero_is_canonical() {
        assert_eq!(
            encode_value(&Value::Float(-0.0)).unwrap(),
            encode_value(&Value::Float(0.0)).unwrap()
        );
        let decoded = decode_value(&encode_value(&Value::Float(-0.0)).unwrap()).unwrap();
        assert!(matches!(decoded, Value::Float(f) if f.to_bits() == 0));
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
        let infinity = encode_f64_key(f64::INFINITY).unwrap();
        let bumped = u64::from_be_bytes(infinity) + 1;
        assert_eq!(
            decode_f64_key(bumped.to_be_bytes()),
            Err(CoreError::InvalidValue)
        );
    }

    #[test]
    fn i64_extremes_round_trip() {
        for value in [i64::MIN, -1, 0, 1, i64::MAX] {
            assert_eq!(decode_i64_key(encode_i64_key(value)), value);
        }
    }

    #[test]
    fn malformed_cells_are_errors() {
        let cases: [(&[u8], CoreError); 14] = [
            (&[], CoreError::Truncated),
            (&[0], CoreError::InvalidValue),
            (&[7], CoreError::InvalidValue),
            (&[1, 0], CoreError::InvalidValue),
            (&[2], CoreError::Truncated),
            (&[2, 2], CoreError::InvalidValue),
            (&[2, 1, 0], CoreError::InvalidValue),
            (&[3, 0, 0], CoreError::Truncated),
            (&[3, 0, 0, 0, 0, 0, 0, 0, 0, 0], CoreError::InvalidValue),
            (&[5, 0, 0], CoreError::Truncated),
            (&[5, 0, 0, 0, 3, b'a'], CoreError::Truncated),
            (&[5, 0, 0, 0, 1, b'a', b'b'], CoreError::InvalidValue),
            (&[5, 0, 0, 0, 1, 0xff], CoreError::InvalidValue),
            (&[6, 0xff, 0xff, 0xff, 0xff], CoreError::Truncated),
        ];
        for (bytes, expected) in cases {
            assert_eq!(decode_value(bytes), Err(expected), "input {bytes:?}");
        }
    }

    #[test]
    fn malformed_keys_are_errors() {
        let cases: [(&[u8], CoreError); 9] = [
            (&[], CoreError::Truncated),
            (&[0], CoreError::InvalidValue),
            (&[2], CoreError::Truncated),
            (&[2, 5], CoreError::InvalidValue),
            (&[3, 1, 2], CoreError::Truncated),
            (&[5, b'a'], CoreError::Truncated),
            (&[5, b'a', 0], CoreError::Truncated),
            (&[5, b'a', 0, 7], CoreError::InvalidValue),
            (&[5, 0xff, 0, 1], CoreError::InvalidValue),
        ];
        for (bytes, expected) in cases {
            assert_eq!(decode_value_key(bytes), Err(expected), "input {bytes:?}");
        }
    }
}
