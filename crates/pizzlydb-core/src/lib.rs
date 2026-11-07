//! Identifiers, scalar values, and ordered key encodings.
//!
//! [`NodeId`], [`EdgeId`], and the other identifier types are opaque 128-bit
//! values. Zero is invalid. [`Value`] is the scalar set stored in a property.
//! [`encode_value`] writes the stored form of a value, and
//! [`encode_value_key`], [`encode_i64_key`], and [`encode_f64_key`] write
//! bytes whose unsigned order equals value order. [`KeyBuf`] builds keys that
//! start with a namespace and a kind tag.
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
mod sequence;
mod value;

pub use error::CoreError;
pub use id::{
    EdgeId, ID_LEN, Identifier, IndexId, LabelId, NamespaceId, NodeId, PropertyKeyId, TypeId,
};
pub use key::KeyBuf;
pub use mutation::{
    DeclarationImage, GraphMutation, IndexImage, catalog_kind, decode_mutation,
    decode_mutation_list, encode_mutation, encode_mutation_list, scope_kind,
};
pub use sequence::CommitSeq;
pub use value::{
    Value, ValueType, decode_f64_key, decode_i64_key, decode_value, decode_value_key,
    encode_f64_key, encode_i64_key, encode_value, encode_value_key,
};
