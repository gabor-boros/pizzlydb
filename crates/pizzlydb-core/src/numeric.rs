//! Exact numeric order shared by integers, floats, and decimals.
//!
//! Index keys use one numeric encoding so `Int(2)`, `Float(1.5)`, and
//! `Decimal` values sort by mathematical value. Equal values share one key.
//! The cell format still stores the original type.

use crate::error::CoreError;

/// Maximum fractional digits of a [`Decimal`].
pub const DECIMAL_SCALE_MAX: u8 = 28;

/// Maximum significant digits of a [`Decimal`] coefficient.
pub const DECIMAL_DIGITS_MAX: u32 = 38;

const BIAS: i32 = 8_192;
const CLASS_NEG_INF: u8 = 1;
const CLASS_NEG: u8 = 2;
const CLASS_ZERO: u8 = 3;
const CLASS_POS: u8 = 4;
const CLASS_POS_INF: u8 = 5;

/// Base-10 exact number: `sign * coefficient / 10^scale`.
///
/// The coefficient is canonical: while `scale > 0` and the coefficient is
/// divisible by ten, both are reduced. `1.50` and `1.5` are the same value.
///
/// # Examples
///
/// ```
/// use pizzlydb_core::Decimal;
///
/// let a = Decimal::from_str_exact("1.50").unwrap();
/// let b = Decimal::from_str_exact("1.5").unwrap();
/// assert_eq!(a, b);
/// assert_eq!(a.coefficient(), 15);
/// assert_eq!(a.scale(), 1);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Decimal {
    coefficient: i128,
    scale: u8,
}

impl Decimal {
    /// Canonical decimal with `coefficient / 10^scale`.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::InvalidValue`] when `scale` exceeds
    /// [`DECIMAL_SCALE_MAX`] or the coefficient has more than
    /// [`DECIMAL_DIGITS_MAX`] digits.
    ///
    /// # Examples
    ///
    /// ```
    /// use pizzlydb_core::Decimal;
    ///
    /// let value = Decimal::new(150, 2).unwrap();
    /// assert_eq!(value.coefficient(), 15);
    /// assert_eq!(value.scale(), 1);
    /// assert!(Decimal::new(1, 29).is_err());
    /// ```
    pub fn new(coefficient: i128, scale: u8) -> Result<Self, CoreError> {
        if scale > DECIMAL_SCALE_MAX || digit_count(coefficient) > DECIMAL_DIGITS_MAX {
            return Err(CoreError::InvalidValue);
        }
        Ok(canonicalize(coefficient, scale))
    }

    /// Parse a decimal without an exponent.
    ///
    /// The text is an optional sign, digits, and an optional fractional part.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::InvalidValue`] for an empty, non-canonical, or
    /// out-of-range literal.
    ///
    /// # Examples
    ///
    /// ```
    /// use pizzlydb_core::Decimal;
    ///
    /// assert_eq!(
    ///     Decimal::from_str_exact("-0.10").unwrap(),
    ///     Decimal::new(-1, 1).unwrap()
    /// );
    /// assert!(Decimal::from_str_exact("1e2").is_err());
    /// ```
    pub fn from_str_exact(text: &str) -> Result<Self, CoreError> {
        let bytes = text.as_bytes();
        if bytes.is_empty() {
            return Err(CoreError::InvalidValue);
        }
        let (negative, rest) = match bytes[0] {
            b'+' => (false, &bytes[1..]),
            b'-' => (true, &bytes[1..]),
            _ => (false, bytes),
        };
        if rest.is_empty() || rest == b"." {
            return Err(CoreError::InvalidValue);
        }

        let mut coefficient: i128 = 0;
        let mut scale: u32 = 0;
        let mut seen_digit = false;
        let mut fractional = false;
        for &byte in rest {
            match byte {
                b'0'..=b'9' => {
                    seen_digit = true;
                    coefficient = coefficient
                        .checked_mul(10)
                        .and_then(|value| value.checked_add(i128::from(byte - b'0')))
                        .ok_or(CoreError::InvalidValue)?;
                    if fractional {
                        scale = scale.saturating_add(1);
                    }
                }
                b'.' if !fractional => fractional = true,
                _ => return Err(CoreError::InvalidValue),
            }
        }
        if !seen_digit || scale > u32::from(DECIMAL_SCALE_MAX) {
            return Err(CoreError::InvalidValue);
        }
        let coefficient = if negative { -coefficient } else { coefficient };
        Self::new(
            coefficient,
            u8::try_from(scale).map_err(|_| CoreError::InvalidValue)?,
        )
    }

    /// Signed coefficient after canonicalization.
    #[must_use]
    pub const fn coefficient(self) -> i128 {
        self.coefficient
    }

    /// Digits to the right of the decimal point.
    #[must_use]
    pub const fn scale(self) -> u8 {
        self.scale
    }
}

impl std::str::FromStr for Decimal {
    type Err = CoreError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::from_str_exact(text)
    }
}

impl std::fmt::Display for Decimal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.coefficient < 0 {
            write!(f, "-")?;
        }
        let digits = self.coefficient.unsigned_abs().to_string();
        let scale = usize::from(self.scale);
        if scale == 0 {
            return write!(f, "{digits}");
        }
        if digits.len() <= scale {
            let zeros = scale - digits.len();
            write!(f, "0.")?;
            for _ in 0..zeros {
                write!(f, "0")?;
            }
            write!(f, "{digits}")
        } else {
            let split = digits.len() - scale;
            write!(f, "{}.{}", &digits[..split], &digits[split..])
        }
    }
}

/// Normalized exact value used for comparison and key bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Exact {
    kind: ExactKind,
    /// Most-significant digit first. Empty for zero and infinities.
    digits: Vec<u8>,
    /// Power of ten of the least significant digit.
    exp: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExactKind {
    NegInf,
    Neg,
    Zero,
    Pos,
    PosInf,
}

pub(crate) fn exact_i64(value: i64) -> Exact {
    exact_int(i128::from(value), 0)
}

pub(crate) fn exact_decimal(value: Decimal) -> Exact {
    exact_int(value.coefficient, i32::from(value.scale))
}

pub(crate) fn exact_f64(value: f64) -> Result<Exact, CoreError> {
    if value.is_nan() {
        return Err(CoreError::InvalidValue);
    }
    if value == 0.0 {
        return Ok(zero());
    }
    if value.is_infinite() {
        return Ok(Exact {
            kind: if value.is_sign_negative() {
                ExactKind::NegInf
            } else {
                ExactKind::PosInf
            },
            digits: Vec::new(),
            exp: 0,
        });
    }

    let bits = value.to_bits();
    let negative = bits >> 63 == 1;
    let raw_exp = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & ((1_u64 << 52) - 1);
    let (mantissa, exp2) = if raw_exp == 0 {
        (frac, -1022 - 52)
    } else {
        (frac | (1_u64 << 52), raw_exp - 1023 - 52)
    };

    let mut significand = BigUint::from_u64(mantissa);
    let exp10 = if exp2 >= 0 {
        significand.shl_bits(exp2 as u32);
        0
    } else {
        significand.mul_pow5((-exp2) as u32);
        exp2
    };
    Ok(finish(negative, significand, exp10))
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn exact_cmp(left: &Exact, right: &Exact) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let rank = |kind: ExactKind| match kind {
        ExactKind::NegInf => 0,
        ExactKind::Neg => 1,
        ExactKind::Zero => 2,
        ExactKind::Pos => 3,
        ExactKind::PosInf => 4,
    };
    match rank(left.kind).cmp(&rank(right.kind)) {
        Ordering::Equal => {}
        other => return other,
    }
    match left.kind {
        ExactKind::Neg => cmp_abs(left, right).reverse(),
        ExactKind::Pos => cmp_abs(left, right),
        _ => Ordering::Equal,
    }
}

pub(crate) fn encode_exact(value: &Exact, out: &mut Vec<u8>) -> Result<(), CoreError> {
    out.push(match value.kind {
        ExactKind::NegInf => CLASS_NEG_INF,
        ExactKind::Neg => CLASS_NEG,
        ExactKind::Zero => CLASS_ZERO,
        ExactKind::Pos => CLASS_POS,
        ExactKind::PosInf => CLASS_POS_INF,
    });
    if !matches!(value.kind, ExactKind::Neg | ExactKind::Pos) {
        return Ok(());
    }

    let magnitude = magnitude(value)?;
    let mut body = Vec::with_capacity(value.digits.len() + 3);
    let biased = magnitude.checked_add(BIAS).ok_or(CoreError::InvalidValue)?;
    if !(0..=u16::MAX as i32).contains(&biased) {
        return Err(CoreError::InvalidValue);
    }
    body.extend_from_slice(&(biased as u16).to_be_bytes());
    for digit in &value.digits {
        body.push(digit + 1);
    }
    body.push(0);
    if value.kind == ExactKind::Neg {
        for byte in &mut body {
            *byte ^= 0xff;
        }
    }
    out.extend_from_slice(&body);
    Ok(())
}

pub(crate) fn decode_exact(bytes: &[u8]) -> Result<(Exact, usize), CoreError> {
    let (&class, rest) = bytes.split_first().ok_or(CoreError::Truncated)?;
    let kind = match class {
        CLASS_NEG_INF => ExactKind::NegInf,
        CLASS_NEG => ExactKind::Neg,
        CLASS_ZERO => ExactKind::Zero,
        CLASS_POS => ExactKind::Pos,
        CLASS_POS_INF => ExactKind::PosInf,
        _ => return Err(CoreError::InvalidValue),
    };
    if !matches!(kind, ExactKind::Neg | ExactKind::Pos) {
        return Ok((
            Exact {
                kind,
                digits: Vec::new(),
                exp: 0,
            },
            1,
        ));
    }

    if rest.len() < 3 {
        return Err(CoreError::Truncated);
    }
    let mut header = [0_u8; 2];
    header.copy_from_slice(&rest[..2]);
    let mut body = rest[2..].to_vec();
    if kind == ExactKind::Neg {
        for byte in &mut header {
            *byte ^= 0xff;
        }
        for byte in &mut body {
            *byte ^= 0xff;
        }
    }
    let magnitude = u16::from_be_bytes(header) as i32 - BIAS;
    let mut digits = Vec::new();
    let mut used = 2;
    for (index, byte) in body.iter().enumerate() {
        if *byte == 0 {
            used += index + 1;
            break;
        }
        if !(1..=10).contains(byte) {
            return Err(CoreError::InvalidValue);
        }
        digits.push(byte - 1);
        if index + 1 == body.len() {
            return Err(CoreError::Truncated);
        }
    }
    if digits.is_empty() || digits[0] == 0 {
        return Err(CoreError::InvalidValue);
    }
    let exp = magnitude - (digits.len() as i32 - 1);
    Ok((Exact { kind, digits, exp }, used + 1))
}

pub(crate) fn exact_to_canonical(value: &Exact) -> Result<Canonical, CoreError> {
    match value.kind {
        ExactKind::NegInf => Ok(Canonical::Float(f64::NEG_INFINITY)),
        ExactKind::PosInf => Ok(Canonical::Float(f64::INFINITY)),
        ExactKind::Zero => Ok(Canonical::Int(0)),
        ExactKind::Neg | ExactKind::Pos => canonical_finite(value),
    }
}

/// Value recovered from a numeric key.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Canonical {
    Int(i64),
    Decimal(Decimal),
    Float(f64),
}

fn canonical_finite(value: &Exact) -> Result<Canonical, CoreError> {
    if value.exp >= 0
        && let Some(int) = digits_to_i64(&value.digits, value.exp)
    {
        let signed = if value.kind == ExactKind::Neg {
            -int
        } else {
            int
        };
        return Ok(Canonical::Int(signed));
    }

    let scale = if value.exp < 0 { -value.exp } else { 0 };
    if (0..=i32::from(DECIMAL_SCALE_MAX)).contains(&scale)
        && let Some(coefficient) = digits_to_i128(&value.digits, value.exp.max(0))
    {
        let signed = if value.kind == ExactKind::Neg {
            -coefficient
        } else {
            coefficient
        };
        if let Ok(decimal) = Decimal::new(signed, scale as u8) {
            return Ok(Canonical::Decimal(decimal));
        }
    }

    let mut text = String::new();
    if value.kind == ExactKind::Neg {
        text.push('-');
    }
    text.push_str(&digits_text(&value.digits));
    if value.exp != 0 {
        text.push('e');
        text.push_str(&value.exp.to_string());
    }
    let parsed: f64 = text.parse().map_err(|_| CoreError::InvalidValue)?;
    if parsed.is_nan() || parsed.is_infinite() {
        return Err(CoreError::InvalidValue);
    }
    Ok(Canonical::Float(parsed))
}

fn digits_to_i64(digits: &[u8], extra_zeros: i32) -> Option<i64> {
    let wide = digits_to_i128(digits, extra_zeros)?;
    i64::try_from(wide).ok()
}

fn digits_to_i128(digits: &[u8], extra_zeros: i32) -> Option<i128> {
    let mut value: i128 = 0;
    for digit in digits {
        value = value.checked_mul(10)?.checked_add(i128::from(*digit))?;
    }
    for _ in 0..extra_zeros {
        value = value.checked_mul(10)?;
    }
    Some(value)
}

fn digits_text(digits: &[u8]) -> String {
    digits
        .iter()
        .map(|digit| char::from(b'0' + digit))
        .collect()
}

fn exact_int(coefficient: i128, scale: i32) -> Exact {
    if coefficient == 0 {
        return zero();
    }
    let negative = coefficient < 0;
    let mut significand = BigUint::from_u128(coefficient.unsigned_abs());
    let mut exp = -scale;
    while significand.mod10() == 0 {
        significand.div10();
        exp += 1;
    }
    finish(negative, significand, exp)
}

fn finish(negative: bool, significand: BigUint, mut exp: i32) -> Exact {
    if significand.is_zero() {
        return zero();
    }
    let mut significand = significand;
    while significand.mod10() == 0 {
        significand.div10();
        exp += 1;
    }
    Exact {
        kind: if negative {
            ExactKind::Neg
        } else {
            ExactKind::Pos
        },
        digits: significand.to_digits(),
        exp,
    }
}

fn zero() -> Exact {
    Exact {
        kind: ExactKind::Zero,
        digits: Vec::new(),
        exp: 0,
    }
}

fn magnitude(value: &Exact) -> Result<i32, CoreError> {
    let len = i32::try_from(value.digits.len()).map_err(|_| CoreError::InvalidValue)?;
    value
        .exp
        .checked_add(len - 1)
        .ok_or(CoreError::InvalidValue)
}

#[cfg_attr(not(test), allow(dead_code))]
fn cmp_abs(left: &Exact, right: &Exact) -> std::cmp::Ordering {
    let left_mag = magnitude(left).unwrap_or(i32::MIN);
    let right_mag = magnitude(right).unwrap_or(i32::MIN);
    match left_mag.cmp(&right_mag) {
        std::cmp::Ordering::Equal => {}
        other => return other,
    }
    let width = left.digits.len().max(right.digits.len());
    for index in 0..width {
        let left_digit = left.digits.get(index).copied().unwrap_or(0);
        let right_digit = right.digits.get(index).copied().unwrap_or(0);
        match left_digit.cmp(&right_digit) {
            std::cmp::Ordering::Equal => {}
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}

fn canonicalize(mut coefficient: i128, mut scale: u8) -> Decimal {
    while scale > 0 && coefficient % 10 == 0 {
        coefficient /= 10;
        scale -= 1;
    }
    Decimal { coefficient, scale }
}

fn digit_count(value: i128) -> u32 {
    let value = value.unsigned_abs();
    if value == 0 { 1 } else { value.ilog10() + 1 }
}

#[derive(Clone, Debug)]
struct BigUint {
    limbs: Vec<u32>,
}

impl BigUint {
    fn from_u64(value: u64) -> Self {
        if value == 0 {
            Self { limbs: Vec::new() }
        } else {
            let mut limbs = vec![value as u32];
            let high = (value >> 32) as u32;
            if high != 0 {
                limbs.push(high);
            }
            Self { limbs }
        }
    }

    fn from_u128(value: u128) -> Self {
        let mut out = Self::from_u64(value as u64);
        let high = (value >> 64) as u64;
        if high != 0 {
            let mut upper = Self::from_u64(high);
            upper.shl_bits(64);
            out.add_assign(&upper);
        }
        out
    }

    fn is_zero(&self) -> bool {
        self.limbs.is_empty()
    }

    fn shl_bits(&mut self, bits: u32) {
        if self.is_zero() || bits == 0 {
            return;
        }
        let words = (bits / 32) as usize;
        let rem = bits % 32;
        if rem != 0 {
            let mut carry = 0_u32;
            for limb in &mut self.limbs {
                let next_carry = *limb >> (32 - rem);
                *limb = limb.wrapping_shl(rem) | carry;
                carry = next_carry;
            }
            if carry != 0 {
                self.limbs.push(carry);
            }
        }
        if words > 0 {
            let mut next = vec![0; words];
            next.append(&mut self.limbs);
            self.limbs = next;
        }
    }

    fn mul_small(&mut self, factor: u32) {
        if factor == 0 || self.is_zero() {
            self.limbs.clear();
            return;
        }
        if factor == 1 {
            return;
        }
        let mut carry = 0_u64;
        for limb in &mut self.limbs {
            let product = u64::from(*limb) * u64::from(factor) + carry;
            *limb = product as u32;
            carry = product >> 32;
        }
        if carry != 0 {
            self.limbs.push(carry as u32);
        }
    }

    fn mul_pow5(&mut self, power: u32) {
        for _ in 0..power {
            self.mul_small(5);
        }
    }

    fn add_assign(&mut self, other: &Self) {
        let width = self.limbs.len().max(other.limbs.len());
        self.limbs.resize(width, 0);
        let mut carry = 0_u64;
        for index in 0..width {
            let right = u64::from(other.limbs.get(index).copied().unwrap_or(0));
            let sum = u64::from(self.limbs[index]) + right + carry;
            self.limbs[index] = sum as u32;
            carry = sum >> 32;
        }
        if carry != 0 {
            self.limbs.push(carry as u32);
        }
    }

    fn mod10(&self) -> u8 {
        let mut rem = 0_u64;
        for limb in self.limbs.iter().rev() {
            rem = ((rem << 32) | u64::from(*limb)) % 10;
        }
        rem as u8
    }

    fn div10(&mut self) {
        let mut rem = 0_u64;
        for limb in self.limbs.iter_mut().rev() {
            let cur = (rem << 32) | u64::from(*limb);
            *limb = (cur / 10) as u32;
            rem = cur % 10;
        }
        self.trim();
    }

    fn to_digits(&self) -> Vec<u8> {
        if self.is_zero() {
            return vec![0];
        }
        let mut copy = self.clone();
        let mut digits = Vec::new();
        while !copy.is_zero() {
            digits.push(copy.mod10());
            copy.div10();
        }
        digits.reverse();
        digits
    }

    fn trim(&mut self) {
        while self.limbs.last().is_some_and(|limb| *limb == 0) {
            self.limbs.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Decimal, decode_exact, encode_exact, exact_cmp, exact_decimal, exact_f64, exact_i64,
    };

    #[test]
    fn int_two_sorts_after_float_one_point_five() {
        let int = exact_i64(2);
        let float = exact_f64(1.5).unwrap();
        assert_eq!(exact_cmp(&float, &int), std::cmp::Ordering::Less);
        let mut left = Vec::new();
        let mut right = Vec::new();
        encode_exact(&float, &mut left).unwrap();
        encode_exact(&int, &mut right).unwrap();
        assert!(left < right);
    }

    #[test]
    fn equal_numbers_share_a_key() {
        let mut int = Vec::new();
        let mut float = Vec::new();
        let mut decimal = Vec::new();
        encode_exact(&exact_i64(1), &mut int).unwrap();
        encode_exact(&exact_f64(1.0).unwrap(), &mut float).unwrap();
        encode_exact(
            &exact_decimal(Decimal::from_str_exact("1.00").unwrap()),
            &mut decimal,
        )
        .unwrap();
        assert_eq!(int, float);
        assert_eq!(int, decimal);
    }

    #[test]
    fn decimal_one_point_five_is_above_one() {
        let decimal = exact_decimal(Decimal::from_str_exact("1.50").unwrap());
        assert_eq!(
            exact_cmp(&decimal, &exact_i64(1)),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn numeric_keys_round_trip_the_order() {
        let samples = [
            exact_i64(i64::MIN),
            exact_i64(-1),
            exact_i64(0),
            exact_i64(1),
            exact_f64(f64::NEG_INFINITY).unwrap(),
            exact_f64(-1.5).unwrap(),
            exact_f64(0.1).unwrap(),
            exact_f64(f64::INFINITY).unwrap(),
            exact_decimal(Decimal::from_str_exact("-12.3400").unwrap()),
        ];
        for value in &samples {
            let mut bytes = Vec::new();
            encode_exact(value, &mut bytes).unwrap();
            let (decoded, used) = decode_exact(&bytes).unwrap();
            assert_eq!(used, bytes.len());
            assert_eq!(exact_cmp(value, &decoded), std::cmp::Ordering::Equal);
        }
    }
}
