//! Property cells and declarations.
//!
//! A property cell is one key per owner and property key. Its value is the
//! cell format of [`encode_value`]. A missing key is absence; a stored
//! [`Value::Null`] is null.
//!
//! A declaration gives a property key a value type and a nullable flag within
//! a label or relationship-type scope. The declaration that applies to a node
//! is the one on its lowest-id label that declares the key; for an edge it is
//! the declaration on its relationship type. A value set under a declaration
//! is stored in a declared cell and checked against it. Other values are
//! stored in an undeclared cell. A read returns the declared cell when it
//! exists, including a declared null, and the undeclared cell otherwise.
//! Setting or clearing a property removes the other cell, so a stale cell
//! cannot reappear.
//!
//! Adding or loosening a declaration does not rewrite existing cells.
//!
//! A cell value whose leaf entry does not fit one storage page is written to
//! the storage engine's overflow pages. An encoded cell longer than
//! [`MAX_VALUE_LEN`](pizzlydb_storage::MAX_VALUE_LEN) is rejected with
//! [`GraphError::ValueTooLarge`].

use pizzlydb_core::{
    EdgeId, ID_LEN, LabelId, NamespaceId, NodeId, PropertyKeyId, TypeId, Value, ValueType,
    decode_value, encode_value,
};
use pizzlydb_storage::Vfs;

use crate::access::{IdScan, KeyRead, id_at};
use crate::batch::GraphTxn;
use crate::catalog;
use crate::directory;
use crate::error::{GraphError, corrupt, invalid_value};
use crate::index;
use crate::keys;
use crate::record_store::GraphStore;

const DECLARATION_VERSION: u8 = 1;

/// Node or edge that holds properties.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use pizzlydb_core::{EdgeId, NodeId};
/// use pizzlydb_graph::Owner;
///
/// let node = NodeId::from_u128(1)?;
/// assert_eq!(Owner::from(node), Owner::Node(node));
/// assert_ne!(Owner::Node(node), Owner::Edge(EdgeId::from_u128(1)?));
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Owner {
    /// A node.
    Node(NodeId),
    /// An edge.
    Edge(EdgeId),
}

impl Owner {
    pub(crate) const fn kind(self) -> u8 {
        match self {
            Self::Node(_) => 1,
            Self::Edge(_) => 2,
        }
    }

    pub(crate) const fn id_bytes(&self) -> &[u8; ID_LEN] {
        match self {
            Self::Node(id) => id.as_bytes(),
            Self::Edge(id) => id.as_bytes(),
        }
    }
}

impl From<NodeId> for Owner {
    fn from(id: NodeId) -> Self {
        Self::Node(id)
    }
}

impl From<EdgeId> for Owner {
    fn from(id: EdgeId) -> Self {
        Self::Edge(id)
    }
}

/// Label or relationship type that scopes a declaration or an index.
///
/// A label scope covers nodes with the label. A type scope covers edges of
/// the type.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use pizzlydb_core::{LabelId, TypeId};
/// use pizzlydb_graph::Scope;
///
/// let label = LabelId::from_u128(1)?;
/// assert_eq!(Scope::from(label), Scope::Label(label));
/// assert_ne!(Scope::Label(label), Scope::Type(TypeId::from_u128(1)?));
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    /// Nodes with this label.
    Label(LabelId),
    /// Edges of this relationship type.
    Type(TypeId),
}

impl Scope {
    pub(crate) const fn kind(self) -> u8 {
        match self {
            Self::Label(_) => 1,
            Self::Type(_) => 2,
        }
    }

    pub(crate) const fn id_bytes(&self) -> &[u8; ID_LEN] {
        match self {
            Self::Label(id) => id.as_bytes(),
            Self::Type(id) => id.as_bytes(),
        }
    }

    pub(crate) fn decode(kind: u8, bytes: &[u8], offset: usize) -> Result<Self, GraphError> {
        match kind {
            1 => id_at(bytes, offset).map(Self::Label),
            2 => id_at(bytes, offset).map(Self::Type),
            _ => Err(GraphError::Corrupt),
        }
    }

    pub(crate) fn ensure_issued<R: KeyRead>(
        self,
        read: &R,
        namespace: NamespaceId,
    ) -> Result<(), GraphError> {
        match self {
            Self::Label(id) => catalog::ensure_issued(read, namespace, id),
            Self::Type(id) => catalog::ensure_issued(read, namespace, id),
        }
    }
}

impl From<LabelId> for Scope {
    fn from(id: LabelId) -> Self {
        Self::Label(id)
    }
}

impl From<TypeId> for Scope {
    fn from(id: TypeId) -> Self {
        Self::Type(id)
    }
}

/// Value type and null rule of a declared property.
///
/// # Examples
///
/// ```
/// use pizzlydb_core::{Value, ValueType};
/// use pizzlydb_graph::PropertyDeclaration;
///
/// let age = PropertyDeclaration { value_type: ValueType::Int, nullable: false };
/// assert!(age.accepts(&Value::Int(42)));
/// assert!(!age.accepts(&Value::Null));
/// assert!(!age.accepts(&Value::from("42")));
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PropertyDeclaration {
    /// Type of every non-null value.
    pub value_type: ValueType,
    /// `true` when [`Value::Null`] is accepted.
    pub nullable: bool,
}

impl PropertyDeclaration {
    /// `true` when `value` satisfies this declaration.
    ///
    /// # Examples
    ///
    /// ```
    /// use pizzlydb_core::{Value, ValueType};
    /// use pizzlydb_graph::PropertyDeclaration;
    ///
    /// let nickname = PropertyDeclaration { value_type: ValueType::String, nullable: true };
    /// assert!(nickname.accepts(&Value::Null));
    /// assert!(nickname.accepts(&Value::from("Bob")));
    /// ```
    #[must_use]
    pub fn accepts(&self, value: &Value) -> bool {
        match value.value_type() {
            None => self.nullable,
            Some(value_type) => value_type == self.value_type,
        }
    }

    pub(crate) fn encode(self) -> Vec<u8> {
        vec![
            DECLARATION_VERSION,
            self.value_type.tag(),
            u8::from(self.nullable),
        ]
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, GraphError> {
        match bytes {
            [DECLARATION_VERSION, tag, nullable @ (0 | 1)] => Ok(Self {
                value_type: ValueType::from_tag(*tag).map_err(corrupt)?,
                nullable: *nullable == 1,
            }),
            _ => Err(GraphError::Corrupt),
        }
    }
}

impl GraphStore {
    /// Declare property `key` within `scope`.
    ///
    /// Repeating the current declaration stages nothing. Changing a required
    /// declaration to nullable updates it. Existing cells are not rewritten.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::SchemaConflict`] when a declaration exists with a
    /// different value type, or when a nullable declaration would become
    /// required; [`GraphError::InvalidIdentity`] when the scope or `key` was
    /// not issued in this namespace; and [`GraphError::Storage`] or
    /// [`GraphError::Corrupt`] on a failed read.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_core::{LabelId, NamespaceId, PropertyKeyId, ValueType};
    /// use pizzlydb_graph::{GraphTxn, GraphBatch, GraphError, GraphStore, PropertyDeclaration, Scope};
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
    /// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
    ///
    /// let mut batch = GraphBatch::begin(&storage)?;
    /// let person: LabelId = graph.intern(&mut batch, "Person")?;
    /// let age: PropertyKeyId = graph.intern(&mut batch, "age")?;
    /// let scope = Scope::Label(person);
    ///
    /// let required = PropertyDeclaration { value_type: ValueType::Int, nullable: false };
    /// graph.declare(&mut batch, scope, age, required)?;
    /// let text = PropertyDeclaration { value_type: ValueType::String, nullable: false };
    /// assert!(matches!(
    ///     graph.declare(&mut batch, scope, age, text),
    ///     Err(GraphError::SchemaConflict)
    /// ));
    ///
    /// let optional = PropertyDeclaration { nullable: true, ..required };
    /// graph.declare(&mut batch, scope, age, optional)?;
    /// assert_eq!(graph.declaration(&batch, scope, age)?, Some(optional));
    /// # Ok(())
    /// # }
    /// ```
    pub fn declare<V: Vfs>(
        &self,
        batch: &mut GraphTxn<V>,
        scope: Scope,
        key: PropertyKeyId,
        declaration: PropertyDeclaration,
    ) -> Result<(), GraphError> {
        let namespace = self.namespace();
        scope.ensure_issued(batch, namespace)?;
        catalog::ensure_issued(batch, namespace, key)?;

        let existing = self.declaration(batch, scope, key)?;
        if let Some(current) = existing {
            if current == declaration {
                return Ok(());
            }

            if current.value_type != declaration.value_type
                || (current.nullable && !declaration.nullable)
            {
                return Err(GraphError::SchemaConflict);
            }
        }

        batch.put(
            keys::declaration(namespace, scope, key),
            declaration.encode(),
        )?;
        batch.record(pizzlydb_core::GraphMutation::DeclarationChange {
            namespace,
            scope_kind: scope.kind(),
            scope: *scope.id_bytes(),
            key,
            previous: existing.map(|current| pizzlydb_core::DeclarationImage {
                value_type: current.value_type,
                nullable: current.nullable,
            }),
            declaration: Some(pizzlydb_core::DeclarationImage {
                value_type: declaration.value_type,
                nullable: declaration.nullable,
            }),
        });

        Ok(())
    }

    /// Declaration of `key` within `scope`, or [`None`].
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Storage`] or [`GraphError::Corrupt`] on a failed
    /// read.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_core::{NamespaceId, PropertyKeyId, TypeId};
    /// use pizzlydb_graph::{GraphBatch, GraphStore, Scope};
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
    /// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
    ///
    /// let mut batch = GraphBatch::begin(&storage)?;
    /// let knows: TypeId = graph.intern(&mut batch, "KNOWS")?;
    /// let since: PropertyKeyId = graph.intern(&mut batch, "since")?;
    /// assert_eq!(graph.declaration(&batch, Scope::Type(knows), since)?, None);
    /// # Ok(())
    /// # }
    /// ```
    pub fn declaration<R: KeyRead>(
        &self,
        read: &R,
        scope: Scope,
        key: PropertyKeyId,
    ) -> Result<Option<PropertyDeclaration>, GraphError> {
        read.get(&keys::declaration(self.namespace(), scope, key))?
            .map(|bytes| PropertyDeclaration::decode(&bytes))
            .transpose()
    }

    /// Set property `key` of a live node or edge to `value`.
    ///
    /// The cell and the entries of every index on the property are staged
    /// together. [`Value::Null`] is stored as a present null.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::NotFound`] or [`GraphError::Deleted`] when the
    /// owner is not live, [`GraphError::InvalidIdentity`] when `key` was not
    /// issued in this namespace, [`GraphError::InvalidValue`] for NaN,
    /// [`GraphError::SchemaConflict`] when the value does not satisfy the
    /// declaration that applies, [`GraphError::ValueTooLarge`] for an encoded
    /// value longer than [`MAX_VALUE_LEN`](pizzlydb_storage::MAX_VALUE_LEN),
    /// [`GraphError::UniqueConflict`] when a unique index already holds the
    /// value for another owner, and [`GraphError::Storage`] or
    /// [`GraphError::Corrupt`] on a failed read.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_core::{NamespaceId, PropertyKeyId, Value};
    /// use pizzlydb_graph::{GraphBatch, GraphError, GraphStore};
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
    /// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
    ///
    /// let mut batch = GraphBatch::begin(&storage)?;
    /// let name: PropertyKeyId = graph.intern(&mut batch, "name")?;
    /// let node = graph.create_node(&mut batch)?;
    ///
    /// graph.set_property(&mut batch, node, name, &Value::from("Alice"))?;
    /// assert_eq!(graph.get_property(&batch, node, name)?, Some(Value::from("Alice")));
    /// assert!(matches!(
    ///     graph.set_property(&mut batch, node, name, &Value::Float(f64::NAN)),
    ///     Err(GraphError::InvalidValue)
    /// ));
    /// # Ok(())
    /// # }
    /// ```
    pub fn set_property<V: Vfs>(
        &self,
        batch: &mut GraphTxn<V>,
        owner: impl Into<Owner>,
        key: PropertyKeyId,
        value: &Value,
    ) -> Result<(), GraphError> {
        let owner = owner.into();
        let namespace = self.namespace();
        let declaration = applicable_declaration(batch, namespace, owner, key)?;
        catalog::ensure_issued(batch, namespace, key)?;

        let encoded = encode_value(value).map_err(invalid_value)?;
        if declaration.is_some_and(|declaration| !declaration.accepts(value)) {
            return Err(GraphError::SchemaConflict);
        }

        let previous = read_cell(batch, namespace, owner, key)?;
        let declared = declaration.is_some();
        let (node, edge) = owner_ids(owner);
        batch.atomic(|batch| {
            batch.put(keys::cell(namespace, declared, owner, key), encoded)?;
            batch.delete(keys::cell(namespace, !declared, owner, key))?;
            index::apply_property_index(
                batch,
                namespace,
                owner,
                key,
                previous.as_ref(),
                Some(value),
            )?;
            batch.record(pizzlydb_core::GraphMutation::PropertySet {
                namespace,
                node,
                edge,
                key,
                previous: previous.clone(),
                value: value.clone(),
            });
            Ok(())
        })
    }

    /// Value of property `key`, or [`None`] when it is absent.
    ///
    /// A missing or deleted owner has no properties.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Storage`] or [`GraphError::Corrupt`] on a failed
    /// read.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_core::{NamespaceId, PropertyKeyId, Value};
    /// use pizzlydb_graph::{GraphBatch, GraphStore};
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
    /// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
    ///
    /// let mut batch = GraphBatch::begin(&storage)?;
    /// let nickname: PropertyKeyId = graph.intern(&mut batch, "nickname")?;
    /// let node = graph.create_node(&mut batch)?;
    ///
    /// assert_eq!(graph.get_property(&batch, node, nickname)?, None);
    /// graph.set_property(&mut batch, node, nickname, &Value::Null)?;
    /// assert_eq!(graph.get_property(&batch, node, nickname)?, Some(Value::Null));
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_property<R: KeyRead>(
        &self,
        read: &R,
        owner: impl Into<Owner>,
        key: PropertyKeyId,
    ) -> Result<Option<Value>, GraphError> {
        read_cell(read, self.namespace(), owner.into(), key)
    }

    /// Remove property `key` of a live node or edge.
    ///
    /// Both cells and the property's index entries are removed together.
    /// Clearing an absent property stages nothing.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::NotFound`] or [`GraphError::Deleted`] when the
    /// owner is not live, and [`GraphError::Storage`] or
    /// [`GraphError::Corrupt`] on a failed read.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_core::{NamespaceId, PropertyKeyId, Value};
    /// use pizzlydb_graph::{GraphBatch, GraphStore};
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
    /// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
    ///
    /// let mut batch = GraphBatch::begin(&storage)?;
    /// let age: PropertyKeyId = graph.intern(&mut batch, "age")?;
    /// let node = graph.create_node(&mut batch)?;
    ///
    /// graph.set_property(&mut batch, node, age, &Value::Int(36))?;
    /// graph.clear_property(&mut batch, node, age)?;
    ///
    /// assert_eq!(graph.get_property(&batch, node, age)?, None);
    /// # Ok(())
    /// # }
    /// ```
    pub fn clear_property<V: Vfs>(
        &self,
        batch: &mut GraphTxn<V>,
        owner: impl Into<Owner>,
        key: PropertyKeyId,
    ) -> Result<(), GraphError> {
        let owner = owner.into();
        let namespace = self.namespace();
        directory::ensure_live(batch, namespace, owner)?;
        batch.atomic(|batch| clear_cell(batch, namespace, owner, key))
    }
}

/// Remove every property of `owner`.
pub(crate) fn clear_all<V: Vfs>(
    batch: &mut GraphTxn<V>,
    namespace: NamespaceId,
    owner: Owner,
) -> Result<(), GraphError> {
    let mut property_keys = Vec::new();
    for declared in [true, false] {
        let ids: IdScan<'_, _, PropertyKeyId> =
            IdScan::new(batch, keys::cells(namespace, declared, owner))?;
        for id in ids {
            property_keys.push(id?);
        }
    }

    property_keys.sort_unstable();
    property_keys.dedup();

    for key in property_keys {
        clear_cell(batch, namespace, owner, key)?;
    }

    Ok(())
}

fn clear_cell<V: Vfs>(
    batch: &mut GraphTxn<V>,
    namespace: NamespaceId,
    owner: Owner,
    key: PropertyKeyId,
) -> Result<(), GraphError> {
    let Some(previous) = read_cell(batch, namespace, owner, key)? else {
        return Ok(());
    };

    for declared in [true, false] {
        batch.delete(keys::cell(namespace, declared, owner, key))?;
    }

    let (node, edge) = owner_ids(owner);
    batch.record(pizzlydb_core::GraphMutation::PropertyClear {
        namespace,
        node,
        edge,
        key,
        previous: previous.clone(),
    });
    index::apply_property_index(batch, namespace, owner, key, Some(&previous), None)
}

fn owner_ids(owner: Owner) -> (Option<NodeId>, Option<EdgeId>) {
    match owner {
        Owner::Node(node) => (Some(node), None),
        Owner::Edge(edge) => (None, Some(edge)),
    }
}

/// Effective value of `key`: the declared cell, else the undeclared cell.
pub(crate) fn read_cell<R: KeyRead>(
    read: &R,
    namespace: NamespaceId,
    owner: Owner,
    key: PropertyKeyId,
) -> Result<Option<Value>, GraphError> {
    for declared in [true, false] {
        if let Some(bytes) = read.get(&keys::cell(namespace, declared, owner, key))? {
            return decode_value(&bytes).map(Some).map_err(corrupt);
        }
    }

    Ok(None)
}

/// Declaration of `key` that applies to a live `owner`.
fn applicable_declaration<R: KeyRead>(
    read: &R,
    namespace: NamespaceId,
    owner: Owner,
    key: PropertyKeyId,
) -> Result<Option<PropertyDeclaration>, GraphError> {
    let graph = GraphStore::new(namespace);
    match owner {
        Owner::Node(node) => {
            directory::load_node(read, namespace, node)?;
            let labels: IdScan<'_, R, LabelId> =
                IdScan::new(read, keys::node_labels(namespace, node))?;
            for label in labels {
                if let Some(declaration) = graph.declaration(read, Scope::Label(label?), key)? {
                    return Ok(Some(declaration));
                }
            }
            Ok(None)
        }
        Owner::Edge(edge) => {
            let record = directory::load_edge(read, namespace, edge)?;
            graph.declaration(read, Scope::Type(record.edge_type), key)
        }
    }
}
