//! Identifiers, scalar values, and ordered key encodings.
//!
//! [`NodeId`], [`EdgeId`], and the other identifier types are opaque 128-bit
//! values. Zero is invalid. [`Value`] is the scalar set stored in a property.
//! [`encode_value`] writes the stored form of a value, and
//! [`encode_value_key`] writes bytes whose unsigned order equals
//! [`index_cmp`]. Integers, floats, and decimals share one numeric order.
//! [`KeyBuf`] builds keys that start with a namespace and a kind tag.
//!
//! This crate has no database-module dependencies.
//!
//! # Examples
//!
//! ```
//! use std::collections::BTreeMap;
//!
//! use pizzlydb_core::{KeyBuf, NamespaceId, Value};
//!
//! let namespace = NamespaceId::from_u128(1).unwrap();
//! let mut map = BTreeMap::new();
//! for number in [3_i64, -7, 0, i64::MIN] {
//!     let mut key = KeyBuf::new(namespace, 1);
//!     key.push_value(&Value::Int(number)).unwrap();
//!     map.insert(key.into_vec(), number);
//! }
//! let ordered: Vec<i64> = map.into_values().collect();
//! assert_eq!(ordered, [i64::MIN, -7, 0, 3]);
//! ```

mod error;
mod id;
mod key;
mod mutation;
mod numeric;
mod sequence;
mod temporal;
mod value;

pub use error::CoreError;
pub use id::{
    EdgeId, ID_LEN, Identifier, IndexId, LabelId, NamespaceId, NodeId, PropertyKeyId, TypeId,
};
pub use key::KeyBuf;
pub use mutation::{
    DeclarationImage, GraphMutation, IndexImage, catalog_kind, decode_mutation,
    decode_mutation_list, encode_mutation, encode_mutation_list, schema_change, scope_kind,
};
pub use numeric::{DECIMAL_DIGITS_MAX, DECIMAL_SCALE_MAX, Decimal};
pub use sequence::CommitSeq;
pub use temporal::{Date, Duration, LocalDateTime, Timestamp};
pub use value::{
    MAX_VALUE_DEPTH, Value, ValueType, decode_f64_key, decode_i64_key, decode_value,
    decode_value_key, encode_f64_key, encode_i64_key, encode_value, encode_value_key, index_cmp,
    predicate_cmp,
};
