//! Ordered storage keys.
//!
//! Every key starts with a [`NamespaceId`] and a one-byte kind tag. The
//! namespace is fixed width, so a scan bounded by one namespace prefix never
//! yields a key of another namespace. Fields after the kind are fixed-width
//! identifiers, single bytes, values in the key format of
//! [`encode_value_key`], or a raw byte string as the final field.

use crate::error::CoreError;
use crate::id::{ID_LEN, Identifier, NamespaceId};
use crate::value::{Value, encode_value_key};

/// Builder for one storage key.
///
/// # Examples
///
/// ```
/// use pizzlydb_core::{KeyBuf, NamespaceId, NodeId, Value};
///
/// let namespace = NamespaceId::from_u128(1).unwrap();
/// let mut key = KeyBuf::new(namespace, 7);
/// key.push_id(NodeId::from_u128(2).unwrap()).push_u8(1);
/// key.push_value(&Value::Int(-4)).unwrap();
///
/// assert!(key.as_bytes().starts_with(namespace.as_bytes()));
/// assert_eq!(key.as_bytes()[KeyBuf::PREFIX_LEN - 1], 7);
/// assert!(key.len() > KeyBuf::PREFIX_LEN + 16 + 1);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct KeyBuf {
    bytes: Vec<u8>,
}

impl KeyBuf {
    /// Length of the namespace and kind prefix.
    pub const PREFIX_LEN: usize = ID_LEN + 1;

    /// Start a key with `namespace` and `kind`.
    ///
    /// # Examples
    ///
    /// ```
    /// use pizzlydb_core::{KeyBuf, NamespaceId};
    ///
    /// let a = KeyBuf::new(NamespaceId::from_u128(1).unwrap(), 9);
    /// let b = KeyBuf::new(NamespaceId::from_u128(2).unwrap(), 0);
    /// assert!(a.as_bytes() < b.as_bytes());
    /// assert!(!b.as_bytes().starts_with(a.as_bytes()));
    /// ```
    #[must_use]
    pub fn new(namespace: NamespaceId, kind: u8) -> Self {
        let mut bytes = Vec::with_capacity(64);
        bytes.extend_from_slice(namespace.as_bytes());
        bytes.push(kind);
        Self { bytes }
    }

    /// Append one byte.
    ///
    /// # Examples
    ///
    /// ```
    /// use pizzlydb_core::{KeyBuf, NamespaceId};
    ///
    /// let mut key = KeyBuf::new(NamespaceId::from_u128(1).unwrap(), 1);
    /// key.push_u8(4);
    /// assert_eq!(key.as_bytes().last(), Some(&4));
    /// ```
    pub fn push_u8(&mut self, byte: u8) -> &mut Self {
        self.bytes.push(byte);
        self
    }

    /// Append the bytes of `id`.
    ///
    /// # Examples
    ///
    /// ```
    /// use pizzlydb_core::{KeyBuf, LabelId, NamespaceId};
    ///
    /// let label = LabelId::from_u128(5).unwrap();
    /// let mut key = KeyBuf::new(NamespaceId::from_u128(1).unwrap(), 1);
    /// key.push_id(label);
    /// assert!(key.as_bytes().ends_with(label.as_bytes()));
    /// ```
    pub fn push_id<I: Identifier>(&mut self, id: I) -> &mut Self {
        self.bytes.extend_from_slice(id.as_bytes());
        self
    }

    /// Append `value` in the order-preserving key format.
    ///
    /// On error the key is unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::InvalidValue`] for NaN.
    ///
    /// # Examples
    ///
    /// ```
    /// use pizzlydb_core::{KeyBuf, NamespaceId, Value};
    ///
    /// let namespace = NamespaceId::from_u128(1).unwrap();
    /// let mut low = KeyBuf::new(namespace, 1);
    /// let mut high = KeyBuf::new(namespace, 1);
    /// low.push_value(&Value::Int(-10)).unwrap();
    /// high.push_value(&Value::Int(3)).unwrap();
    /// assert!(low.as_bytes() < high.as_bytes());
    /// assert!(low.push_value(&Value::Float(f64::NAN)).is_err());
    /// ```
    pub fn push_value(&mut self, value: &Value) -> Result<&mut Self, CoreError> {
        encode_value_key(value, &mut self.bytes)?;
        Ok(self)
    }

    /// Append raw bytes.
    ///
    /// Raw bytes have no terminator, so they are only ordered correctly as
    /// the last field of a key.
    ///
    /// # Examples
    ///
    /// ```
    /// use pizzlydb_core::{KeyBuf, NamespaceId};
    ///
    /// let mut key = KeyBuf::new(NamespaceId::from_u128(1).unwrap(), 1);
    /// key.push_bytes(b"name");
    /// assert!(key.as_bytes().ends_with(b"name"));
    /// ```
    pub fn push_bytes(&mut self, bytes: &[u8]) -> &mut Self {
        self.bytes.extend_from_slice(bytes);
        self
    }

    /// Borrow the key bytes.
    ///
    /// # Examples
    ///
    /// ```
    /// use pizzlydb_core::{KeyBuf, NamespaceId};
    ///
    /// let key = KeyBuf::new(NamespaceId::from_u128(1).unwrap(), 2);
    /// assert_eq!(key.as_bytes().len(), KeyBuf::PREFIX_LEN);
    /// ```
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Number of key bytes.
    ///
    /// # Examples
    ///
    /// ```
    /// use pizzlydb_core::{KeyBuf, NamespaceId};
    ///
    /// let key = KeyBuf::new(NamespaceId::from_u128(1).unwrap(), 2);
    /// assert_eq!(key.len(), KeyBuf::PREFIX_LEN);
    /// ```
    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Always `false`: a key holds at least the namespace and kind.
    ///
    /// # Examples
    ///
    /// ```
    /// use pizzlydb_core::{KeyBuf, NamespaceId};
    ///
    /// assert!(!KeyBuf::new(NamespaceId::from_u128(1).unwrap(), 2).is_empty());
    /// ```
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Take the key bytes.
    ///
    /// # Examples
    ///
    /// ```
    /// use pizzlydb_core::{KeyBuf, NamespaceId};
    ///
    /// let bytes = KeyBuf::new(NamespaceId::from_u128(1).unwrap(), 2).into_vec();
    /// assert_eq!(bytes.len(), KeyBuf::PREFIX_LEN);
    /// ```
    #[must_use]
    pub fn into_vec(self) -> Vec<u8> {
        self.bytes
    }
}

impl AsRef<[u8]> for KeyBuf {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

impl From<KeyBuf> for Vec<u8> {
    fn from(key: KeyBuf) -> Self {
        key.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::KeyBuf;
    use crate::id::NamespaceId;
    use crate::value::Value;

    #[test]
    fn failed_value_push_leaves_key_unchanged() {
        let mut key = KeyBuf::new(NamespaceId::from_u128(3).unwrap(), 1);
        let before = key.clone();
        assert!(key.push_value(&Value::Float(f64::NAN)).is_err());
        assert_eq!(key, before);
    }

    #[test]
    fn namespace_prefix_excludes_neighbouring_namespaces() {
        let low = NamespaceId::from_u128(0xff).unwrap();
        let high = NamespaceId::from_u128(0x100).unwrap();
        let mut key = KeyBuf::new(high, 0);
        key.push_bytes(&[0; 32]);
        assert!(!key.as_bytes().starts_with(low.as_bytes()));
        assert!(KeyBuf::new(low, 0xff).as_bytes() < key.as_bytes());
    }
}
