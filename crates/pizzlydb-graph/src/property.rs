//! Property cells and declarations.
//!
//! A property cell is one key per owner and property key. Its value is the
//! cell format of [`encode_value`]. A missing key is absence; a stored
//! [`Value::Null`] is null.
//!
//! A declaration gives a property key a value type, a nullable flag, and a
//! required flag within a label or relationship-type scope. A node must
//! satisfy the declarations of every label it carries. An edge must satisfy
//! the declaration on its relationship type. Required properties are checked
//! at commit. Type and null checks on a write fail immediately.
//!
//! Tightening a declaration validates the members of its scope in the caller's
//! transaction. Loosening one does not rewrite cells.
//!
//! A cell value whose leaf entry does not fit one storage page is written to
//! the storage engine's overflow pages. An encoded cell longer than
//! [`MAX_VALUE_LEN`](pizzlydb_storage::MAX_VALUE_LEN) is rejected with
//! [`GraphError::ValueTooLarge`]. An index key longer than
//! [`MAX_KEY_LEN`](pizzlydb_storage::MAX_KEY_LEN) is rejected with
//! [`GraphError::KeyTooLarge`] at write time. The index does not store a prefix.

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

/// Value type, null rule, and presence rule of a declared property.
///
/// A node satisfies every declaration of its labels. An edge satisfies the
/// declaration of its relationship type. Conflicting value types for one
/// property key are rejected when the second declaration is created.
/// `required` is checked at commit, so a transaction may create a node and
/// set the property before `commit`.
///
/// # Examples
///
/// ```
/// use pizzlydb_core::{Value, ValueType};
/// use pizzlydb_graph::PropertyDeclaration;
///
/// let age = PropertyDeclaration::new(ValueType::Int, false);
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
    /// `true` when the property must be present at commit.
    pub required: bool,
}

impl PropertyDeclaration {
    /// Declaration that is not required.
    #[must_use]
    pub const fn new(value_type: ValueType, nullable: bool) -> Self {
        Self {
            value_type,
            nullable,
            required: false,
        }
    }

    /// `true` when `value` satisfies this declaration.
    ///
    /// # Examples
    ///
    /// ```
    /// use pizzlydb_core::{Value, ValueType};
    /// use pizzlydb_graph::PropertyDeclaration;
    ///
    /// let nickname = PropertyDeclaration::new(ValueType::String, true);
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
        let mut flags = u8::from(self.nullable);
        if self.required {
            flags |= 0b10;
        }
        vec![DECLARATION_VERSION, self.value_type.tag(), flags]
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, GraphError> {
        match bytes {
            [DECLARATION_VERSION, tag, flags] if flags & !0b11 == 0 => Ok(Self {
                value_type: ValueType::from_tag(*tag).map_err(corrupt)?,
                nullable: flags & 0b01 == 0b01,
                required: flags & 0b10 == 0b10,
            }),
            _ => Err(GraphError::Corrupt),
        }
    }

    fn image(self) -> pizzlydb_core::DeclarationImage {
        pizzlydb_core::DeclarationImage {
            value_type: self.value_type,
            nullable: self.nullable,
            required: self.required,
        }
    }
}

impl GraphStore {
    /// Declare property `key` within `scope`.
    ///
    /// Repeating the current declaration stages nothing. Loosening nullability
    /// or `required` updates the declaration. Tightening, including a type
    /// change, validates every current member in this transaction and fails
    /// with [`GraphError::ConstraintViolation`] when one does not satisfy the
    /// new declaration. A different value type for the same key on another
    /// label or relationship type is rejected.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::ConstraintViolation`] when existing data or
    /// another declaration conflicts, [`GraphError::InvalidIdentity`] when the
    /// scope or `key` was not issued, and [`GraphError::Storage`] or
    /// [`GraphError::Corrupt`] on a failed read.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_core::{LabelId, NamespaceId, PropertyKeyId, ValueType};
    /// use pizzlydb_graph::{ConstraintKind, GraphBatch, GraphError, GraphStore, PropertyDeclaration, Scope};
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
    /// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
    ///
    /// let mut batch = GraphBatch::begin(&storage)?;
    /// let person: LabelId = graph.intern(&mut batch, "Person")?;
    /// let place: LabelId = graph.intern(&mut batch, "Place")?;
    /// let age: PropertyKeyId = graph.intern(&mut batch, "age")?;
    /// let scope = Scope::Label(person);
    ///
    /// let required = PropertyDeclaration::new(ValueType::Int, false);
    /// graph.declare(&mut batch, scope, age, required)?;
    /// graph.declare(&mut batch, scope, age, required)?;
    /// assert!(batch.mutations().iter().any(|mutation| {
    ///     matches!(mutation, pizzlydb_core::GraphMutation::DeclarationChange { .. })
    /// }));
    /// let text = PropertyDeclaration::new(ValueType::String, false);
    /// assert!(matches!(
    ///     graph.declare(&mut batch, Scope::Label(place), age, text),
    ///     Err(GraphError::ConstraintViolation { kind: ConstraintKind::DeclarationConflict, .. })
    /// ));
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
        self.ensure(batch)?;
        scope.ensure_issued(batch, namespace)?;
        catalog::ensure_issued(batch, namespace, key)?;
        reject_type_conflict(batch, namespace, scope, key, declaration.value_type)?;

        let existing = self.declaration(batch, scope, key)?;
        if let Some(current) = existing {
            if current == declaration {
                return Ok(());
            }
            if tightens(current, declaration) {
                validate_scope(batch, namespace, scope, key, declaration)?;
            }
        }

        batch.atomic(|batch| {
            batch.put(
                keys::declaration(namespace, scope, key),
                declaration.encode(),
            )?;
            batch.record(pizzlydb_core::GraphMutation::DeclarationChange {
                namespace,
                scope_kind: scope.kind(),
                scope: *scope.id_bytes(),
                key,
                previous: existing.map(PropertyDeclaration::image),
                declaration: Some(declaration.image()),
            });
            crate::schema::bump(batch, namespace, pizzlydb_core::schema_change::DECLARATION)?;
            Ok(())
        })
    }

    /// Remove the declaration of `key` within `scope`.
    ///
    /// Cells stay where they are. Removing an absent declaration stages nothing.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Storage`] or [`GraphError::Corrupt`] on a failed read.
    pub fn undeclare<V: Vfs>(
        &self,
        batch: &mut GraphTxn<V>,
        scope: Scope,
        key: PropertyKeyId,
    ) -> Result<(), GraphError> {
        let namespace = self.namespace();
        self.ensure(batch)?;
        let Some(existing) = self.declaration(batch, scope, key)? else {
            return Ok(());
        };
        batch.atomic(|batch| {
            batch.delete(keys::declaration(namespace, scope, key))?;
            batch.record(pizzlydb_core::GraphMutation::DeclarationChange {
                namespace,
                scope_kind: scope.kind(),
                scope: *scope.id_bytes(),
                key,
                previous: Some(existing.image()),
                declaration: None,
            });
            crate::schema::bump(batch, namespace, pizzlydb_core::schema_change::UNDECLARE)?;
            Ok(())
        })
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
        self.ensure(batch)?;
        directory::ensure_live(batch, namespace, owner)?;
        catalog::ensure_issued(batch, namespace, key)?;
        let rules = declarations_for(batch, namespace, owner, key)?;
        let encoded = encode_value(value).map_err(invalid_value)?;
        if let Some(violation) = rules
            .iter()
            .find_map(|rule| mismatch(owner, key, rule, value))
        {
            return Err(violation);
        }

        let previous = read_cell(batch, namespace, owner, key)?;
        let (node, edge) = owner_ids(owner);
        batch.atomic(|batch| {
            batch.put(keys::cell(namespace, owner, key), encoded)?;
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
            batch.note_owner(namespace, owner);
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
        self.ensure(batch)?;
        directory::ensure_live(batch, namespace, owner)?;
        batch.atomic(|batch| {
            clear_cell(batch, namespace, owner, key)?;
            batch.note_owner(namespace, owner);
            Ok(())
        })
    }
}

/// Remove every property of `owner`.
pub(crate) fn clear_all<V: Vfs>(
    batch: &mut GraphTxn<V>,
    namespace: NamespaceId,
    owner: Owner,
) -> Result<(), GraphError> {
    let mut property_keys = Vec::new();
    let ids: IdScan<'_, _, PropertyKeyId> = IdScan::new(batch, keys::cells(namespace, owner))?;
    for id in ids {
        property_keys.push(id?);
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
    batch.delete(keys::cell(namespace, owner, key))?;
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

/// Effective value of `key`.
pub(crate) fn read_cell<R: KeyRead>(
    read: &R,
    namespace: NamespaceId,
    owner: Owner,
    key: PropertyKeyId,
) -> Result<Option<Value>, GraphError> {
    match read.get(&keys::cell(namespace, owner, key))? {
        Some(bytes) => decode_value(&bytes).map(Some).map_err(corrupt),
        None => Ok(None),
    }
}

fn declarations_for<R: KeyRead>(
    read: &R,
    namespace: NamespaceId,
    owner: Owner,
    key: PropertyKeyId,
) -> Result<Vec<PropertyDeclaration>, GraphError> {
    let graph = GraphStore::new(namespace);
    let mut found = Vec::new();
    match owner {
        Owner::Node(node) => {
            let labels: IdScan<'_, R, LabelId> =
                IdScan::new(read, keys::node_labels(namespace, node))?;
            for label in labels {
                if let Some(declaration) = graph.declaration(read, Scope::Label(label?), key)? {
                    found.push(declaration);
                }
            }
        }
        Owner::Edge(edge) => {
            let record = directory::load_edge(read, namespace, edge)?;
            if let Some(declaration) =
                graph.declaration(read, Scope::Type(record.edge_type), key)?
            {
                found.push(declaration);
            }
        }
    }
    Ok(found)
}

fn mismatch(
    owner: Owner,
    key: PropertyKeyId,
    rule: &PropertyDeclaration,
    value: &Value,
) -> Option<GraphError> {
    if rule.accepts(value) {
        return None;
    }
    let kind = if value.value_type().is_none() {
        crate::error::ConstraintKind::NullForbidden
    } else {
        crate::error::ConstraintKind::TypeMismatch
    };
    Some(violation(
        kind,
        owner,
        Some(key),
        "value does not match the declaration",
    ))
}

pub(crate) fn violation(
    kind: crate::error::ConstraintKind,
    owner: Owner,
    key: Option<PropertyKeyId>,
    detail: &str,
) -> GraphError {
    GraphError::ConstraintViolation {
        kind,
        entity_kind: owner.kind(),
        entity: *owner.id_bytes(),
        key,
        detail: detail.to_owned(),
    }
}

fn tightens(current: PropertyDeclaration, next: PropertyDeclaration) -> bool {
    current.value_type != next.value_type
        || (current.nullable && !next.nullable)
        || (!current.required && next.required)
}

fn reject_type_conflict<R: KeyRead>(
    read: &R,
    namespace: NamespaceId,
    scope: Scope,
    key: PropertyKeyId,
    value_type: ValueType,
) -> Result<(), GraphError> {
    let prefix = keys::declaration_keys(namespace);
    for entry in crate::access::PrefixScan::new(read, prefix.clone())? {
        let (stored, bytes) = entry?;
        if stored.len() != prefix.len() + 1 + 32 {
            return Err(GraphError::Corrupt);
        }
        let kind = stored[prefix.len()];
        if kind != scope.kind() {
            continue;
        }
        let other = Scope::decode(kind, &stored, prefix.len() + 1)?;
        let property = id_at_key(&stored, prefix.len() + 1 + 16)?;
        if property != key || other == scope {
            continue;
        }
        let declaration = PropertyDeclaration::decode(&bytes)?;
        if declaration.value_type != value_type {
            return Err(GraphError::ConstraintViolation {
                kind: crate::error::ConstraintKind::DeclarationConflict,
                entity_kind: 0,
                entity: [0; 16],
                key: Some(key),
                detail: "another scope declares a different type for this key".to_owned(),
            });
        }
    }
    Ok(())
}

fn id_at_key(bytes: &[u8], offset: usize) -> Result<PropertyKeyId, GraphError> {
    crate::access::id_at(bytes, offset)
}

fn validate_scope<V: Vfs>(
    batch: &mut GraphTxn<V>,
    namespace: NamespaceId,
    scope: Scope,
    key: PropertyKeyId,
    declaration: PropertyDeclaration,
) -> Result<(), GraphError> {
    let graph = GraphStore::new(namespace);
    let owners: Vec<Owner> = match scope {
        Scope::Label(label) => graph
            .nodes_with_label(batch, label)?
            .map(|node| node.map(Owner::Node))
            .collect::<Result<Vec<_>, _>>()?,
        Scope::Type(edge_type) => graph
            .edges_with_type(batch, edge_type)?
            .map(|edge| edge.map(Owner::Edge))
            .collect::<Result<Vec<_>, _>>()?,
    };
    for owner in owners {
        match read_cell(batch, namespace, owner, key)? {
            None if declaration.required => {
                return Err(violation(
                    crate::error::ConstraintKind::RequiredMissing,
                    owner,
                    Some(key),
                    "required property is absent",
                ));
            }
            None => {}
            Some(value) => {
                if let Some(err) = mismatch(owner, key, &declaration, &value) {
                    return Err(err);
                }
            }
        }
    }
    Ok(())
}
