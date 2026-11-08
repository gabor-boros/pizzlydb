//! Key layouts of the graph map.
//!
//! Every key starts with the 16-byte namespace and one kind byte. Identifiers
//! are 16 bytes. `value` is the key format of
//! [`encode_value_key`](pizzlydb_core::encode_value_key).
//!
//! - `0x01` counter: unused. Ids come from the system allocator.
//!   Older files may still contain `id_kind` to the last issued id.
//! - `0x02` node: `node` to a node record.
//! - `0x03` edge: `edge` to an edge record.
//! - `0x04` name: `id_kind || name` to the id.
//! - `0x05` name by id: `id_kind || id` to the name bytes.
//! - `0x06` adjacency: `node || direction || type || neighbor || edge` to an
//!   empty value.
//! - `0x07` node label: `node || label` to an empty value.
//! - `0x08` label member: `label || node` to an empty value.
//! - `0x09` type member: `type || edge` to an empty value.
//! - `0x0A` declaration: `scope_kind || scope || property_key` to a
//!   declaration.
//! - `0x0B` property cell: `owner_kind || owner || property_key` to a cell
//!   value. Kind `0x0C` is reserved and unused.
//! - `0x0D` index definition: `index` to a definition.
//! - `0x0E` index target: `scope_kind || scope || property_key || unique` to
//!   the index id.
//! - `0x0F` unique entry: `index || value` to the owner id.
//! - `0x10` property index entry: `index || value || owner` to an empty
//!   value.
//! - `0x11` schema version: no further fields, value is a big-endian `u64`.

use pizzlydb_core::{
    EdgeId, ID_LEN, IndexId, KeyBuf, LabelId, NamespaceId, NodeId, PropertyKeyId, TypeId, Value,
};

use crate::error::{GraphError, invalid_value};
use crate::property::{Owner, Scope};

/// Unused kind. Ids come from the system allocator.
#[allow(dead_code)]
const COUNTER: u8 = 0x01;

#[cfg(test)]
mod layout_tests {
    use super::COUNTER;
    use pizzlydb_core::{KeyBuf, NamespaceId};

    #[test]
    fn counter_kind_stays_reserved() {
        let namespace = NamespaceId::from_u128(1).unwrap();
        let key = super::counter(namespace, 1);
        assert_eq!(key[KeyBuf::PREFIX_LEN - 1], COUNTER);
    }
}
const NODE: u8 = 0x02;
const EDGE: u8 = 0x03;
const NAME: u8 = 0x04;
const NAME_BY_ID: u8 = 0x05;
const ADJACENCY: u8 = 0x06;
const NODE_LABEL: u8 = 0x07;
const LABEL_MEMBER: u8 = 0x08;
const TYPE_MEMBER: u8 = 0x09;
const DECLARATION: u8 = 0x0A;
const DECLARED_CELL: u8 = 0x0B;
const UNDECLARED_CELL: u8 = 0x0C;
const _: u8 = UNDECLARED_CELL;
const INDEX_DEFINITION: u8 = 0x0D;
const INDEX_TARGET: u8 = 0x0E;
const UNIQUE_ENTRY: u8 = 0x0F;
const PROPERTY_ENTRY: u8 = 0x10;
const SCHEMA: u8 = 0x11;

/// Offset of the first field after the namespace and kind prefix.
const BODY: usize = KeyBuf::PREFIX_LEN;

/// Identifier kinds. Each kind has its own counter, and catalog kinds have
/// their own name space.
pub(crate) mod id_kind {
    pub(crate) const NODE: u8 = 1;
    pub(crate) const EDGE: u8 = 2;
    pub(crate) const LABEL: u8 = 3;
    pub(crate) const TYPE: u8 = 4;
    pub(crate) const PROPERTY_KEY: u8 = 5;
    pub(crate) const INDEX: u8 = 6;
}

#[allow(dead_code)]
pub(crate) fn counter(namespace: NamespaceId, id_kind: u8) -> Vec<u8> {
    let mut key = KeyBuf::new(namespace, COUNTER);
    key.push_u8(id_kind);
    key.into_vec()
}

pub(crate) fn node(namespace: NamespaceId, node: NodeId) -> Vec<u8> {
    let mut key = KeyBuf::new(namespace, NODE);
    key.push_id(node);
    key.into_vec()
}

pub(crate) fn edge(namespace: NamespaceId, edge: EdgeId) -> Vec<u8> {
    let mut key = KeyBuf::new(namespace, EDGE);
    key.push_id(edge);
    key.into_vec()
}

pub(crate) fn name(namespace: NamespaceId, id_kind: u8, name: &[u8]) -> Vec<u8> {
    let mut key = KeyBuf::new(namespace, NAME);
    key.push_u8(id_kind).push_bytes(name);
    key.into_vec()
}

pub(crate) fn name_by_id(namespace: NamespaceId, id_kind: u8, id: &[u8; ID_LEN]) -> Vec<u8> {
    let mut key = KeyBuf::new(namespace, NAME_BY_ID);
    key.push_u8(id_kind).push_bytes(id);
    key.into_vec()
}

/// Adjacency key. Field offsets are fixed: see [`ADJACENCY_LEN`].
pub(crate) fn adjacency(
    namespace: NamespaceId,
    node: NodeId,
    direction: u8,
    edge_type: TypeId,
    neighbor: NodeId,
    edge: EdgeId,
) -> Vec<u8> {
    let mut key = KeyBuf::new(namespace, ADJACENCY);
    key.push_id(node)
        .push_u8(direction)
        .push_id(edge_type)
        .push_id(neighbor)
        .push_id(edge);

    key.into_vec()
}

pub(crate) const ADJACENCY_TYPE: usize = BODY + ID_LEN + 1;
pub(crate) const ADJACENCY_NEIGHBOR: usize = ADJACENCY_TYPE + ID_LEN;
pub(crate) const ADJACENCY_EDGE: usize = ADJACENCY_NEIGHBOR + ID_LEN;
pub(crate) const ADJACENCY_LEN: usize = ADJACENCY_EDGE + ID_LEN;

/// Prefix of every adjacency key of `node`, in both directions.
pub(crate) fn adjacency_of(namespace: NamespaceId, node: NodeId) -> Vec<u8> {
    let mut key = KeyBuf::new(namespace, ADJACENCY);
    key.push_id(node);
    key.into_vec()
}

/// Prefix of the adjacency keys of `node` in one direction, optionally of one
/// type.
pub(crate) fn adjacency_toward(
    namespace: NamespaceId,
    node: NodeId,
    direction: u8,
    edge_type: Option<TypeId>,
) -> Vec<u8> {
    let mut key = KeyBuf::new(namespace, ADJACENCY);
    key.push_id(node).push_u8(direction);
    if let Some(edge_type) = edge_type {
        key.push_id(edge_type);
    }

    key.into_vec()
}

pub(crate) fn node_label(namespace: NamespaceId, node: NodeId, label: LabelId) -> Vec<u8> {
    let mut key = KeyBuf::new(namespace, NODE_LABEL);
    key.push_id(node).push_id(label);
    key.into_vec()
}

pub(crate) fn node_labels(namespace: NamespaceId, node: NodeId) -> Vec<u8> {
    let mut key = KeyBuf::new(namespace, NODE_LABEL);
    key.push_id(node);
    key.into_vec()
}

pub(crate) fn label_member(namespace: NamespaceId, label: LabelId, node: NodeId) -> Vec<u8> {
    let mut key = KeyBuf::new(namespace, LABEL_MEMBER);
    key.push_id(label).push_id(node);
    key.into_vec()
}

pub(crate) fn label_members(namespace: NamespaceId, label: LabelId) -> Vec<u8> {
    let mut key = KeyBuf::new(namespace, LABEL_MEMBER);
    key.push_id(label);
    key.into_vec()
}

pub(crate) fn type_member(namespace: NamespaceId, edge_type: TypeId, edge: EdgeId) -> Vec<u8> {
    let mut key = KeyBuf::new(namespace, TYPE_MEMBER);
    key.push_id(edge_type).push_id(edge);
    key.into_vec()
}

pub(crate) fn type_members(namespace: NamespaceId, edge_type: TypeId) -> Vec<u8> {
    let mut key = KeyBuf::new(namespace, TYPE_MEMBER);
    key.push_id(edge_type);
    key.into_vec()
}

pub(crate) fn declaration(
    namespace: NamespaceId,
    scope: Scope,
    property: PropertyKeyId,
) -> Vec<u8> {
    let mut key = KeyBuf::new(namespace, DECLARATION);
    key.push_u8(scope.kind())
        .push_bytes(scope.id_bytes())
        .push_id(property);

    key.into_vec()
}

pub(crate) fn cell(namespace: NamespaceId, owner: Owner, property: PropertyKeyId) -> Vec<u8> {
    let mut key = cells(namespace, owner);
    key.extend_from_slice(property.as_bytes());
    key
}

/// Prefix of every property cell owned by `owner`.
pub(crate) fn cells(namespace: NamespaceId, owner: Owner) -> Vec<u8> {
    let mut key = KeyBuf::new(namespace, DECLARED_CELL);
    key.push_u8(owner.kind()).push_bytes(owner.id_bytes());
    key.into_vec()
}

/// Key of the namespace schema version. The value is a big-endian `u64`.
pub(crate) fn schema_version(namespace: NamespaceId) -> Vec<u8> {
    KeyBuf::new(namespace, SCHEMA).into_vec()
}

/// Exclusive end of a prefix, or [`None`] when `prefix` is empty or all `0xff`.
pub(crate) fn prefix_end(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut end = prefix.to_vec();
    while let Some(byte) = end.pop() {
        if byte < u8::MAX {
            end.push(byte + 1);
            return Some(end);
        }
    }
    None
}

pub(crate) fn index_definition(namespace: NamespaceId, index: IndexId) -> Vec<u8> {
    let mut key = KeyBuf::new(namespace, INDEX_DEFINITION);
    key.push_id(index);
    key.into_vec()
}

pub(crate) fn index_target(
    namespace: NamespaceId,
    scope: Scope,
    property: PropertyKeyId,
    unique: bool,
) -> Vec<u8> {
    let mut key = index_targets(namespace, scope);
    key.extend_from_slice(property.as_bytes());
    key.push(u8::from(unique));
    key
}

/// Prefix of every index target of `scope`.
pub(crate) fn index_targets(namespace: NamespaceId, scope: Scope) -> Vec<u8> {
    let mut key = KeyBuf::new(namespace, INDEX_TARGET);
    key.push_u8(scope.kind()).push_bytes(scope.id_bytes());
    key.into_vec()
}

/// Prefix of every entry of `index`.
pub(crate) fn index_entries(namespace: NamespaceId, index: IndexId, unique: bool) -> Vec<u8> {
    let kind = if unique { UNIQUE_ENTRY } else { PROPERTY_ENTRY };
    let mut key = KeyBuf::new(namespace, kind);
    key.push_id(index);
    key.into_vec()
}

pub(crate) fn node_keys(namespace: NamespaceId) -> Vec<u8> {
    KeyBuf::new(namespace, NODE).into_vec()
}

pub(crate) fn edge_keys(namespace: NamespaceId) -> Vec<u8> {
    KeyBuf::new(namespace, EDGE).into_vec()
}

pub(crate) fn adjacency_keys(namespace: NamespaceId) -> Vec<u8> {
    KeyBuf::new(namespace, ADJACENCY).into_vec()
}

pub(crate) fn node_label_keys(namespace: NamespaceId) -> Vec<u8> {
    KeyBuf::new(namespace, NODE_LABEL).into_vec()
}

pub(crate) fn label_member_keys(namespace: NamespaceId) -> Vec<u8> {
    KeyBuf::new(namespace, LABEL_MEMBER).into_vec()
}

pub(crate) fn type_member_keys(namespace: NamespaceId) -> Vec<u8> {
    KeyBuf::new(namespace, TYPE_MEMBER).into_vec()
}

pub(crate) fn declaration_keys(namespace: NamespaceId) -> Vec<u8> {
    KeyBuf::new(namespace, DECLARATION).into_vec()
}

pub(crate) fn index_keys(namespace: NamespaceId) -> Vec<u8> {
    KeyBuf::new(namespace, INDEX_DEFINITION).into_vec()
}

pub(crate) fn name_id_keys(namespace: NamespaceId, id_kind: u8) -> Vec<u8> {
    let mut key = KeyBuf::new(namespace, NAME_BY_ID);
    key.push_u8(id_kind);
    key.into_vec()
}

/// Append the key format of `value` to `key`.
pub(crate) fn push_value(key: &mut Vec<u8>, value: &Value) -> Result<(), GraphError> {
    pizzlydb_core::encode_value_key(value, key).map_err(invalid_value)
}
