//! Logical graph mutations shared by the writer and the consumers.
//!
//! The encoding is versioned, length-prefixed, and independent of map
//! iteration order. [`encode_mutation`] writes one record.
//! [`encode_mutation_list`] writes a counted sequence of those records.

use crate::error::CoreError;
use crate::id::{EdgeId, ID_LEN, IndexId, LabelId, NamespaceId, NodeId, PropertyKeyId, TypeId};
use crate::value::{Value, ValueType, decode_value, encode_value};

const VERSION: u8 = 1;

const NODE_CREATE: u8 = 1;
const NODE_DELETE: u8 = 2;
const EDGE_CREATE: u8 = 3;
const EDGE_DELETE: u8 = 4;
const LABEL_ADD: u8 = 5;
const LABEL_REMOVE: u8 = 6;
const PROPERTY_SET: u8 = 7;
const PROPERTY_CLEAR: u8 = 8;
const CATALOG_CREATE: u8 = 9;
const CATALOG_RENAME: u8 = 10;
const DECLARATION_CHANGE: u8 = 11;
const INDEX_CREATE: u8 = 12;
const INDEX_READY: u8 = 13;
const INDEX_DROP: u8 = 14;
const NAMESPACE_CREATE: u8 = 15;
const NAMESPACE_DROP: u8 = 16;
const SCHEMA_CHANGED: u8 = 17;
const NAMESPACE_RENAME: u8 = 18;

const OWNER_NODE: u8 = 1;
const OWNER_EDGE: u8 = 2;

/// Catalog identifier kind stored in a mutation.
///
/// The values match the graph directory kinds.
pub mod catalog_kind {
    /// Node identifiers.
    pub const NODE: u8 = 1;
    /// Edge identifiers.
    pub const EDGE: u8 = 2;
    /// Label names.
    pub const LABEL: u8 = 3;
    /// Relationship-type names.
    pub const TYPE: u8 = 4;
    /// Property-key names.
    pub const PROPERTY_KEY: u8 = 5;
    /// Index identifiers.
    pub const INDEX: u8 = 6;
}

/// Kind of a [`GraphMutation::SchemaChanged`] record.
pub mod schema_change {
    /// A catalog name was created.
    pub const CATALOG: u8 = 1;
    /// A catalog name was renamed.
    pub const RENAME: u8 = 2;
    /// A property declaration was added or replaced.
    pub const DECLARATION: u8 = 3;
    /// A property declaration was removed.
    pub const UNDECLARE: u8 = 4;
    /// An index build started.
    pub const INDEX_START: u8 = 5;
    /// An index became ready.
    pub const INDEX_READY: u8 = 6;
    /// An index started dropping.
    pub const INDEX_DROP: u8 = 7;
}
/// Scope kind stored on an index or declaration mutation.
pub mod scope_kind {
    /// Label scope.
    pub const LABEL: u8 = 1;
    /// Relationship-type scope.
    pub const TYPE: u8 = 2;
}

/// Before-image of a property declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeclarationImage {
    /// Required value type.
    pub value_type: ValueType,
    /// Whether [`Value::Null`](crate::Value::Null) is accepted.
    pub nullable: bool,
    /// Whether the property must be present at commit.
    pub required: bool,
}

/// Before-image of an index definition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexImage {
    /// [`scope_kind::LABEL`] or [`scope_kind::TYPE`].
    pub scope_kind: u8,
    /// Label or relationship-type identifier.
    pub scope: [u8; ID_LEN],
    /// Indexed property.
    pub property: PropertyKeyId,
    /// Whether the index rejects duplicate values.
    pub unique: bool,
}

/// One logical graph change.
///
/// Deletes and overwrites carry the previous value so a consumer can retract
/// the change without reading an older snapshot.
#[derive(Clone, Debug, PartialEq)]
pub enum GraphMutation {
    /// A node record was created.
    NodeCreate {
        /// Namespace of the node.
        namespace: NamespaceId,
        /// New node.
        node: NodeId,
    },
    /// A node record was deleted.
    NodeDelete {
        /// Namespace of the node.
        namespace: NamespaceId,
        /// Deleted node. The identifier is the before-image.
        node: NodeId,
    },
    /// An edge record was created.
    EdgeCreate {
        /// Namespace of the edge.
        namespace: NamespaceId,
        /// New edge.
        edge: EdgeId,
        /// Source node.
        source: NodeId,
        /// Target node.
        target: NodeId,
        /// Relationship type.
        edge_type: TypeId,
    },
    /// An edge record was deleted. The endpoints are the before-image.
    EdgeDelete {
        /// Namespace of the edge.
        namespace: NamespaceId,
        /// Deleted edge.
        edge: EdgeId,
        /// Source node before the delete.
        source: NodeId,
        /// Target node before the delete.
        target: NodeId,
        /// Relationship type before the delete.
        edge_type: TypeId,
    },
    /// A label was added to a node.
    LabelAdd {
        /// Namespace of the node.
        namespace: NamespaceId,
        /// Node that gained the label.
        node: NodeId,
        /// Label that was added.
        label: LabelId,
    },
    /// A label was removed from a node.
    LabelRemove {
        /// Namespace of the node.
        namespace: NamespaceId,
        /// Node that lost the label.
        node: NodeId,
        /// Label that was removed.
        label: LabelId,
    },
    /// A property was set. `previous` is absent when the property was unset.
    PropertySet {
        /// Namespace of the owner.
        namespace: NamespaceId,
        /// Node or edge that owns the property.
        node: Option<NodeId>,
        /// Set when the owner is an edge.
        edge: Option<EdgeId>,
        /// Property key.
        key: PropertyKeyId,
        /// Value before the write.
        previous: Option<Value>,
        /// Value after the write.
        value: Value,
    },
    /// A property was cleared. `previous` is the removed value.
    PropertyClear {
        /// Namespace of the owner.
        namespace: NamespaceId,
        /// Node owner, when the owner is a node.
        node: Option<NodeId>,
        /// Edge owner, when the owner is an edge.
        edge: Option<EdgeId>,
        /// Property key.
        key: PropertyKeyId,
        /// Value before the clear.
        previous: Value,
    },
    /// A catalog name was created.
    CatalogCreate {
        /// Namespace of the name.
        namespace: NamespaceId,
        /// [`catalog_kind`] value.
        kind: u8,
        /// Issued identifier.
        id: [u8; ID_LEN],
        /// Name bytes.
        name: Vec<u8>,
    },
    /// A catalog name was renamed.
    CatalogRename {
        /// Namespace of the name.
        namespace: NamespaceId,
        /// [`catalog_kind`] value.
        kind: u8,
        /// Identifier that was renamed.
        id: [u8; ID_LEN],
        /// Previous name.
        previous: Vec<u8>,
        /// New name.
        name: Vec<u8>,
    },
    /// A declaration was added or replaced.
    DeclarationChange {
        /// Namespace of the declaration.
        namespace: NamespaceId,
        /// [`scope_kind`] value.
        scope_kind: u8,
        /// Scope identifier.
        scope: [u8; ID_LEN],
        /// Property key.
        key: PropertyKeyId,
        /// Declaration before the change, if one existed.
        previous: Option<DeclarationImage>,
        /// Declaration after the change. `None` is reserved for a future drop.
        declaration: Option<DeclarationImage>,
    },
    /// An index definition was created.
    IndexCreate {
        /// Namespace of the index.
        namespace: NamespaceId,
        /// Index identifier.
        index: IndexId,
        /// Definition.
        definition: IndexImage,
    },
    /// An index became ready to serve reads.
    IndexReady {
        /// Namespace of the index.
        namespace: NamespaceId,
        /// Index identifier.
        index: IndexId,
    },
    /// An index was dropped. `definition` is the before-image.
    IndexDrop {
        /// Namespace of the index.
        namespace: NamespaceId,
        /// Index identifier.
        index: IndexId,
        /// Definition before the drop.
        definition: IndexImage,
    },
    /// A namespace was registered.
    NamespaceCreate {
        /// New namespace.
        namespace: NamespaceId,
        /// Registry name.
        name: Vec<u8>,
    },
    /// A namespace was dropped.
    NamespaceDrop {
        /// Dropped namespace.
        namespace: NamespaceId,
        /// Registry name before the drop.
        name: Vec<u8>,
    },
    /// The per-namespace schema version changed.
    SchemaChanged {
        /// Namespace whose schema changed.
        namespace: NamespaceId,
        /// Schema version after the change.
        version: u64,
        /// [`schema_change`] code.
        kind: u8,
    },
    /// A namespace was renamed.
    NamespaceRename {
        /// Renamed namespace.
        namespace: NamespaceId,
        /// Previous name.
        previous: Vec<u8>,
        /// New name.
        name: Vec<u8>,
    },
}

impl GraphMutation {
    /// Namespace carried by this record.
    #[must_use]
    pub fn namespace(&self) -> NamespaceId {
        match self {
            Self::NodeCreate { namespace, .. }
            | Self::NodeDelete { namespace, .. }
            | Self::EdgeCreate { namespace, .. }
            | Self::EdgeDelete { namespace, .. }
            | Self::LabelAdd { namespace, .. }
            | Self::LabelRemove { namespace, .. }
            | Self::PropertySet { namespace, .. }
            | Self::PropertyClear { namespace, .. }
            | Self::CatalogCreate { namespace, .. }
            | Self::CatalogRename { namespace, .. }
            | Self::DeclarationChange { namespace, .. }
            | Self::IndexCreate { namespace, .. }
            | Self::IndexReady { namespace, .. }
            | Self::IndexDrop { namespace, .. }
            | Self::NamespaceCreate { namespace, .. }
            | Self::NamespaceDrop { namespace, .. }
            | Self::SchemaChanged { namespace, .. }
            | Self::NamespaceRename { namespace, .. } => *namespace,
        }
    }
}

/// Encode one mutation.
///
/// # Errors
///
/// Returns [`CoreError::InvalidValue`] when a property value cannot be encoded,
/// and [`CoreError::InvalidMutation`] when a name length does not fit in `u32`.
pub fn encode_mutation(mutation: &GraphMutation) -> Result<Vec<u8>, CoreError> {
    let mut out = Vec::new();
    out.push(VERSION);
    match mutation {
        GraphMutation::NodeCreate { namespace, node } => {
            out.push(NODE_CREATE);
            put_id(&mut out, namespace);
            put_id(&mut out, node);
        }
        GraphMutation::NodeDelete { namespace, node } => {
            out.push(NODE_DELETE);
            put_id(&mut out, namespace);
            put_id(&mut out, node);
        }
        GraphMutation::EdgeCreate {
            namespace,
            edge,
            source,
            target,
            edge_type,
        } => {
            out.push(EDGE_CREATE);
            put_id(&mut out, namespace);
            put_id(&mut out, edge);
            put_id(&mut out, source);
            put_id(&mut out, target);
            put_id(&mut out, edge_type);
        }
        GraphMutation::EdgeDelete {
            namespace,
            edge,
            source,
            target,
            edge_type,
        } => {
            out.push(EDGE_DELETE);
            put_id(&mut out, namespace);
            put_id(&mut out, edge);
            put_id(&mut out, source);
            put_id(&mut out, target);
            put_id(&mut out, edge_type);
        }
        GraphMutation::LabelAdd {
            namespace,
            node,
            label,
        } => {
            out.push(LABEL_ADD);
            put_id(&mut out, namespace);
            put_id(&mut out, node);
            put_id(&mut out, label);
        }
        GraphMutation::LabelRemove {
            namespace,
            node,
            label,
        } => {
            out.push(LABEL_REMOVE);
            put_id(&mut out, namespace);
            put_id(&mut out, node);
            put_id(&mut out, label);
        }
        GraphMutation::PropertySet {
            namespace,
            node,
            edge,
            key,
            previous,
            value,
        } => {
            out.push(PROPERTY_SET);
            put_id(&mut out, namespace);
            put_owner(&mut out, *node, *edge)?;
            put_id(&mut out, key);
            put_optional_value(&mut out, previous.as_ref())?;
            put_value(&mut out, value)?;
        }
        GraphMutation::PropertyClear {
            namespace,
            node,
            edge,
            key,
            previous,
        } => {
            out.push(PROPERTY_CLEAR);
            put_id(&mut out, namespace);
            put_owner(&mut out, *node, *edge)?;
            put_id(&mut out, key);
            put_value(&mut out, previous)?;
        }
        GraphMutation::CatalogCreate {
            namespace,
            kind,
            id,
            name,
        } => {
            out.push(CATALOG_CREATE);
            put_id(&mut out, namespace);
            out.push(*kind);
            out.extend_from_slice(id);
            put_bytes(&mut out, name)?;
        }
        GraphMutation::CatalogRename {
            namespace,
            kind,
            id,
            previous,
            name,
        } => {
            out.push(CATALOG_RENAME);
            put_id(&mut out, namespace);
            out.push(*kind);
            out.extend_from_slice(id);
            put_bytes(&mut out, previous)?;
            put_bytes(&mut out, name)?;
        }
        GraphMutation::DeclarationChange {
            namespace,
            scope_kind,
            scope,
            key,
            previous,
            declaration,
        } => {
            out.push(DECLARATION_CHANGE);
            put_id(&mut out, namespace);
            out.push(*scope_kind);
            out.extend_from_slice(scope);
            put_id(&mut out, key);
            put_optional_declaration(&mut out, *previous)?;
            put_optional_declaration(&mut out, *declaration)?;
        }
        GraphMutation::IndexCreate {
            namespace,
            index,
            definition,
        } => {
            out.push(INDEX_CREATE);
            put_id(&mut out, namespace);
            put_id(&mut out, index);
            put_index(&mut out, definition)?;
        }
        GraphMutation::IndexReady { namespace, index } => {
            out.push(INDEX_READY);
            put_id(&mut out, namespace);
            put_id(&mut out, index);
        }
        GraphMutation::IndexDrop {
            namespace,
            index,
            definition,
        } => {
            out.push(INDEX_DROP);
            put_id(&mut out, namespace);
            put_id(&mut out, index);
            put_index(&mut out, definition)?;
        }
        GraphMutation::NamespaceCreate { namespace, name } => {
            out.push(NAMESPACE_CREATE);
            put_id(&mut out, namespace);
            put_bytes(&mut out, name)?;
        }
        GraphMutation::NamespaceDrop { namespace, name } => {
            out.push(NAMESPACE_DROP);
            put_id(&mut out, namespace);
            put_bytes(&mut out, name)?;
        }
        GraphMutation::SchemaChanged {
            namespace,
            version,
            kind,
        } => {
            out.push(SCHEMA_CHANGED);
            put_id(&mut out, namespace);
            out.extend_from_slice(&version.to_be_bytes());
            out.push(*kind);
        }
        GraphMutation::NamespaceRename {
            namespace,
            previous,
            name,
        } => {
            out.push(NAMESPACE_RENAME);
            put_id(&mut out, namespace);
            put_bytes(&mut out, previous)?;
            put_bytes(&mut out, name)?;
        }
    }
    Ok(out)
}

/// Decode one mutation.
///
/// The returned length is the number of bytes consumed. Trailing bytes are
/// not an error; callers that want an exact buffer use [`decode_mutation_list`].
///
/// # Errors
///
/// Returns [`CoreError::Truncated`] or [`CoreError::InvalidMutation`] for a
/// malformed record, and [`CoreError::InvalidValue`] for a bad property value.
pub fn decode_mutation(bytes: &[u8]) -> Result<(GraphMutation, usize), CoreError> {
    let mut cur = Cursor::new(bytes);
    let version = cur.u8()?;
    if version != VERSION {
        return Err(CoreError::InvalidMutation);
    }
    let tag = cur.u8()?;
    let mutation = match tag {
        NODE_CREATE => GraphMutation::NodeCreate {
            namespace: cur.namespace()?,
            node: cur.node()?,
        },
        NODE_DELETE => GraphMutation::NodeDelete {
            namespace: cur.namespace()?,
            node: cur.node()?,
        },
        EDGE_CREATE => GraphMutation::EdgeCreate {
            namespace: cur.namespace()?,
            edge: cur.edge()?,
            source: cur.node()?,
            target: cur.node()?,
            edge_type: cur.edge_type()?,
        },
        EDGE_DELETE => GraphMutation::EdgeDelete {
            namespace: cur.namespace()?,
            edge: cur.edge()?,
            source: cur.node()?,
            target: cur.node()?,
            edge_type: cur.edge_type()?,
        },
        LABEL_ADD => GraphMutation::LabelAdd {
            namespace: cur.namespace()?,
            node: cur.node()?,
            label: cur.label()?,
        },
        LABEL_REMOVE => GraphMutation::LabelRemove {
            namespace: cur.namespace()?,
            node: cur.node()?,
            label: cur.label()?,
        },
        PROPERTY_SET => GraphMutation::PropertySet {
            namespace: cur.namespace()?,
            node: cur.owner_node()?,
            edge: cur.owner_edge_after_node()?,
            key: cur.property()?,
            previous: cur.optional_value()?,
            value: cur.value()?,
        },
        PROPERTY_CLEAR => GraphMutation::PropertyClear {
            namespace: cur.namespace()?,
            node: cur.owner_node()?,
            edge: cur.owner_edge_after_node()?,
            key: cur.property()?,
            previous: cur.value()?,
        },
        CATALOG_CREATE => GraphMutation::CatalogCreate {
            namespace: cur.namespace()?,
            kind: cur.u8()?,
            id: cur.raw_id()?,
            name: cur.bytes()?,
        },
        CATALOG_RENAME => GraphMutation::CatalogRename {
            namespace: cur.namespace()?,
            kind: cur.u8()?,
            id: cur.raw_id()?,
            previous: cur.bytes()?,
            name: cur.bytes()?,
        },
        DECLARATION_CHANGE => GraphMutation::DeclarationChange {
            namespace: cur.namespace()?,
            scope_kind: cur.u8()?,
            scope: cur.raw_id()?,
            key: cur.property()?,
            previous: cur.optional_declaration()?,
            declaration: cur.optional_declaration()?,
        },
        INDEX_CREATE => GraphMutation::IndexCreate {
            namespace: cur.namespace()?,
            index: cur.index()?,
            definition: cur.index_image()?,
        },
        INDEX_READY => GraphMutation::IndexReady {
            namespace: cur.namespace()?,
            index: cur.index()?,
        },
        INDEX_DROP => GraphMutation::IndexDrop {
            namespace: cur.namespace()?,
            index: cur.index()?,
            definition: cur.index_image()?,
        },
        NAMESPACE_CREATE => GraphMutation::NamespaceCreate {
            namespace: cur.namespace()?,
            name: cur.bytes()?,
        },
        NAMESPACE_DROP => GraphMutation::NamespaceDrop {
            namespace: cur.namespace()?,
            name: cur.bytes()?,
        },
        SCHEMA_CHANGED => GraphMutation::SchemaChanged {
            namespace: cur.namespace()?,
            version: cur.u64()?,
            kind: cur.u8()?,
        },
        NAMESPACE_RENAME => GraphMutation::NamespaceRename {
            namespace: cur.namespace()?,
            previous: cur.bytes()?,
            name: cur.bytes()?,
        },
        _ => return Err(CoreError::InvalidMutation),
    };
    Ok((mutation, cur.at))
}

/// Encode a counted list of mutations.
///
/// # Errors
///
/// Returns the errors of [`encode_mutation`], or [`CoreError::InvalidMutation`]
/// when the count does not fit in `u32`.
pub fn encode_mutation_list(mutations: &[GraphMutation]) -> Result<Vec<u8>, CoreError> {
    let count = u32::try_from(mutations.len()).map_err(|_| CoreError::InvalidMutation)?;
    let mut out = Vec::new();
    out.extend_from_slice(&count.to_be_bytes());
    for mutation in mutations {
        let encoded = encode_mutation(mutation)?;
        put_bytes(&mut out, &encoded)?;
    }
    Ok(out)
}

/// Decode a counted list. The input must be consumed exactly.
///
/// # Errors
///
/// Returns [`CoreError::Truncated`] or [`CoreError::InvalidMutation`] when the
/// bytes are not a list produced by [`encode_mutation_list`].
pub fn decode_mutation_list(bytes: &[u8]) -> Result<Vec<GraphMutation>, CoreError> {
    let mut cur = Cursor::new(bytes);
    let count = cur.u32()? as usize;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let encoded = cur.bytes()?;
        let (mutation, used) = decode_mutation(&encoded)?;
        if used != encoded.len() {
            return Err(CoreError::InvalidMutation);
        }
        out.push(mutation);
    }
    if cur.at != bytes.len() {
        return Err(CoreError::InvalidMutation);
    }
    Ok(out)
}

fn put_id(out: &mut Vec<u8>, id: &impl crate::id::Identifier) {
    out.extend_from_slice(id.as_bytes());
}

fn put_owner(
    out: &mut Vec<u8>,
    node: Option<NodeId>,
    edge: Option<EdgeId>,
) -> Result<(), CoreError> {
    match (node, edge) {
        (Some(node), None) => {
            out.push(OWNER_NODE);
            put_id(out, &node);
            Ok(())
        }
        (None, Some(edge)) => {
            out.push(OWNER_EDGE);
            put_id(out, &edge);
            Ok(())
        }
        _ => Err(CoreError::InvalidMutation),
    }
}

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), CoreError> {
    let len = u32::try_from(bytes.len()).map_err(|_| CoreError::InvalidMutation)?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(bytes);
    Ok(())
}

fn put_value(out: &mut Vec<u8>, value: &Value) -> Result<(), CoreError> {
    let encoded = encode_value(value)?;
    put_bytes(out, &encoded)
}

fn put_optional_value(out: &mut Vec<u8>, value: Option<&Value>) -> Result<(), CoreError> {
    match value {
        None => {
            out.push(0);
            Ok(())
        }
        Some(value) => {
            out.push(1);
            put_value(out, value)
        }
    }
}

fn put_declaration(out: &mut Vec<u8>, image: &DeclarationImage) {
    out.push(image.value_type.tag());
    out.push(u8::from(image.nullable));
    out.push(u8::from(image.required));
}

fn put_optional_declaration(
    out: &mut Vec<u8>,
    image: Option<DeclarationImage>,
) -> Result<(), CoreError> {
    match image {
        None => out.push(0),
        Some(image) => {
            out.push(1);
            put_declaration(out, &image);
        }
    }
    Ok(())
}

fn put_index(out: &mut Vec<u8>, image: &IndexImage) -> Result<(), CoreError> {
    if image.scope_kind != scope_kind::LABEL && image.scope_kind != scope_kind::TYPE {
        return Err(CoreError::InvalidMutation);
    }
    out.push(image.scope_kind);
    out.extend_from_slice(&image.scope);
    put_id(out, &image.property);
    out.push(u8::from(image.unique));
    Ok(())
}

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
    pending_edge: Option<EdgeId>,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            at: 0,
            pending_edge: None,
        }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], CoreError> {
        let end = self.at.checked_add(len).ok_or(CoreError::InvalidMutation)?;
        if end > self.bytes.len() {
            return Err(CoreError::Truncated);
        }
        let slice = &self.bytes[self.at..end];
        self.at = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, CoreError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, CoreError> {
        let bytes = self.take(4)?;
        Ok(u32::from_be_bytes(
            bytes.try_into().map_err(|_| CoreError::Truncated)?,
        ))
    }

    fn u64(&mut self) -> Result<u64, CoreError> {
        let bytes = self.take(8)?;
        Ok(u64::from_be_bytes(
            bytes.try_into().map_err(|_| CoreError::Truncated)?,
        ))
    }

    fn raw_id(&mut self) -> Result<[u8; ID_LEN], CoreError> {
        let bytes = self.take(ID_LEN)?;
        bytes.try_into().map_err(|_| CoreError::InvalidMutation)
    }

    fn id<T: crate::id::Identifier>(&mut self) -> Result<T, CoreError> {
        T::from_bytes(self.raw_id()?).map_err(|_| CoreError::InvalidMutation)
    }

    fn namespace(&mut self) -> Result<NamespaceId, CoreError> {
        self.id()
    }

    fn node(&mut self) -> Result<NodeId, CoreError> {
        self.id()
    }

    fn edge(&mut self) -> Result<EdgeId, CoreError> {
        self.id()
    }

    fn label(&mut self) -> Result<LabelId, CoreError> {
        self.id()
    }

    fn edge_type(&mut self) -> Result<TypeId, CoreError> {
        self.id()
    }

    fn property(&mut self) -> Result<PropertyKeyId, CoreError> {
        self.id()
    }

    fn index(&mut self) -> Result<IndexId, CoreError> {
        self.id()
    }

    fn bytes(&mut self) -> Result<Vec<u8>, CoreError> {
        let len = self.u32()? as usize;
        Ok(self.take(len)?.to_vec())
    }

    fn value(&mut self) -> Result<Value, CoreError> {
        let bytes = self.bytes()?;
        decode_value(&bytes)
    }

    fn optional_value(&mut self) -> Result<Option<Value>, CoreError> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.value()?)),
            _ => Err(CoreError::InvalidMutation),
        }
    }

    fn owner_node(&mut self) -> Result<Option<NodeId>, CoreError> {
        let kind = self.u8()?;
        let id = self.raw_id()?;
        match kind {
            OWNER_NODE => Ok(Some(
                NodeId::from_bytes(id).map_err(|_| CoreError::InvalidMutation)?,
            )),
            OWNER_EDGE => {
                self.pending_edge =
                    Some(EdgeId::from_bytes(id).map_err(|_| CoreError::InvalidMutation)?);
                Ok(None)
            }
            _ => Err(CoreError::InvalidMutation),
        }
    }

    fn owner_edge_after_node(&mut self) -> Result<Option<EdgeId>, CoreError> {
        Ok(self.pending_edge.take())
    }

    fn optional_declaration(&mut self) -> Result<Option<DeclarationImage>, CoreError> {
        match self.u8()? {
            0 => Ok(None),
            1 => {
                let tag = self.u8()?;
                let nullable = match self.u8()? {
                    0 => false,
                    1 => true,
                    _ => return Err(CoreError::InvalidMutation),
                };
                let required = match self.u8()? {
                    0 => false,
                    1 => true,
                    _ => return Err(CoreError::InvalidMutation),
                };
                let value_type = ValueType::from_tag(tag)?;
                Ok(Some(DeclarationImage {
                    value_type,
                    nullable,
                    required,
                }))
            }
            _ => Err(CoreError::InvalidMutation),
        }
    }

    fn index_image(&mut self) -> Result<IndexImage, CoreError> {
        let scope_kind = self.u8()?;
        if scope_kind != scope_kind::LABEL && scope_kind != scope_kind::TYPE {
            return Err(CoreError::InvalidMutation);
        }
        let scope = self.raw_id()?;
        if scope == [0; ID_LEN] {
            return Err(CoreError::InvalidMutation);
        }
        let property = self.property()?;
        let unique = match self.u8()? {
            0 => false,
            1 => true,
            _ => return Err(CoreError::InvalidMutation),
        };
        Ok(IndexImage {
            scope_kind,
            scope,
            property,
            unique,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DeclarationImage, GraphMutation, IndexImage, catalog_kind, decode_mutation_list,
        encode_mutation, encode_mutation_list, schema_change, scope_kind,
    };
    use crate::error::CoreError;
    use crate::id::{EdgeId, IndexId, LabelId, NamespaceId, NodeId, PropertyKeyId, TypeId};
    use crate::value::Value;

    fn ns() -> NamespaceId {
        NamespaceId::from_u128(7).unwrap()
    }

    fn samples() -> Vec<GraphMutation> {
        let namespace = ns();
        let node = NodeId::from_u128(3).unwrap();
        let edge = EdgeId::from_u128(4).unwrap();
        let source = NodeId::from_u128(1).unwrap();
        let target = NodeId::from_u128(2).unwrap();
        let edge_type = TypeId::from_u128(5).unwrap();
        let label = LabelId::from_u128(6).unwrap();
        let key = PropertyKeyId::from_u128(8).unwrap();
        let index = IndexId::from_u128(9).unwrap();
        let definition = IndexImage {
            scope_kind: scope_kind::LABEL,
            scope: *label.as_bytes(),
            property: key,
            unique: true,
        };
        let declaration = DeclarationImage {
            value_type: crate::value::ValueType::Int,
            nullable: false,
            required: true,
        };
        vec![
            GraphMutation::NodeCreate { namespace, node },
            GraphMutation::NodeDelete { namespace, node },
            GraphMutation::EdgeCreate {
                namespace,
                edge,
                source,
                target,
                edge_type,
            },
            GraphMutation::EdgeDelete {
                namespace,
                edge,
                source,
                target,
                edge_type,
            },
            GraphMutation::LabelAdd {
                namespace,
                node,
                label,
            },
            GraphMutation::LabelRemove {
                namespace,
                node,
                label,
            },
            GraphMutation::PropertySet {
                namespace,
                node: Some(node),
                edge: None,
                key,
                previous: None,
                value: Value::from("a"),
            },
            GraphMutation::PropertySet {
                namespace,
                node: None,
                edge: Some(edge),
                key,
                previous: Some(Value::Int(1)),
                value: Value::Int(2),
            },
            GraphMutation::PropertyClear {
                namespace,
                node: Some(node),
                edge: None,
                key,
                previous: Value::Bool(true),
            },
            GraphMutation::CatalogCreate {
                namespace,
                kind: catalog_kind::LABEL,
                id: label.to_bytes(),
                name: b"Person".to_vec(),
            },
            GraphMutation::CatalogRename {
                namespace,
                kind: catalog_kind::TYPE,
                id: edge_type.to_bytes(),
                previous: b"KNOWS".to_vec(),
                name: b"LIKES".to_vec(),
            },
            GraphMutation::DeclarationChange {
                namespace,
                scope_kind: scope_kind::TYPE,
                scope: edge_type.to_bytes(),
                key,
                previous: None,
                declaration: Some(declaration),
            },
            GraphMutation::IndexCreate {
                namespace,
                index,
                definition,
            },
            GraphMutation::IndexReady { namespace, index },
            GraphMutation::IndexDrop {
                namespace,
                index,
                definition,
            },
            GraphMutation::NamespaceCreate {
                namespace,
                name: b"main".to_vec(),
            },
            GraphMutation::NamespaceDrop {
                namespace,
                name: b"main".to_vec(),
            },
            GraphMutation::SchemaChanged {
                namespace,
                version: 3,
                kind: schema_change::DECLARATION,
            },
            GraphMutation::NamespaceRename {
                namespace,
                previous: b"main".to_vec(),
                name: b"tenant".to_vec(),
            },
        ]
    }

    #[test]
    fn every_kind_round_trips() {
        let list = samples();
        let encoded = encode_mutation_list(&list).unwrap();
        assert_eq!(decode_mutation_list(&encoded).unwrap(), list);
        let again = encode_mutation_list(&list).unwrap();
        assert_eq!(encoded, again);
    }

    #[test]
    fn truncated_input_is_an_error() {
        let encoded = encode_mutation(&samples()[0]).unwrap();
        assert_eq!(
            super::decode_mutation(&encoded[..encoded.len() - 1]),
            Err(CoreError::Truncated)
        );
        assert_eq!(
            decode_mutation_list(&[0, 0, 0, 1]),
            Err(CoreError::Truncated)
        );
    }
}
