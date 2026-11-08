//! In-memory graph oracle.
//!
//! The model follows the graph crate's public rules: identifiers start at 1
//! and are never reused, names are not case-folded, self-loops and parallel
//! edges are allowed, and a unique index ignores null. It stores maps, not
//! pages.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use pizzlydb_core::{NodeId, Value, ValueType, encode_value, encode_value_key};

/// Name kind for a label.
pub const NAME_LABEL: u8 = 1;
/// Name kind for a relationship type.
pub const NAME_TYPE: u8 = 2;
/// Name kind for a property key.
pub const NAME_KEY: u8 = 3;
/// Scope kind for a label.
pub const SCOPE_LABEL: u8 = 1;
/// Scope kind for a relationship type.
pub const SCOPE_TYPE: u8 = 2;
/// Owner kind for a node.
pub const OWNER_NODE: u8 = 1;
/// Owner kind for an edge.
pub const OWNER_EDGE: u8 = 2;
/// Outgoing adjacency.
pub const DIR_OUT: u8 = 1;
/// Incoming adjacency.
pub const DIR_IN: u8 = 2;

const MAX_NAME_LEN: usize = 256;

/// Failure from one graph step.
///
/// Variants match the graph crate's recoverable errors. [`StepError::Other`]
/// is a storage or invariant failure.
///
/// # Examples
///
/// ```
/// use pizzlydb_testkit::{Operation, ReferenceGraph, StepError};
///
/// let mut graph = ReferenceGraph::new();
/// let err = graph
///     .apply(&Operation::DeleteNode { namespace: 1, node: 1 })
///     .unwrap_err();
/// assert_eq!(err, StepError::NotFound);
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StepError {
    /// The node, edge, or name was never created.
    NotFound,
    /// The node or edge was deleted.
    Deleted,
    /// A catalog or index identifier was not issued, or an identifier is zero.
    InvalidIdentity,
    /// An edge endpoint is missing or deleted.
    EndpointNotLive,
    /// The node still has an incident edge.
    EndpointInUse,
    /// The name is empty, longer than 256 bytes, or contains a NUL.
    InvalidName,
    /// Another identifier of the same kind already has the name.
    NameConflict,
    /// A declaration or value is incompatible.
    SchemaConflict,
    /// A unique index already holds the value.
    UniqueConflict,
    /// The value cannot be encoded.
    InvalidValue,
    /// A failure outside the graph error set.
    Other(String),
}

impl fmt::Display for StepError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => write!(f, "not found"),
            Self::Deleted => write!(f, "deleted"),
            Self::InvalidIdentity => write!(f, "invalid identity"),
            Self::EndpointNotLive => write!(f, "endpoint is not live"),
            Self::EndpointInUse => write!(f, "endpoint is in use"),
            Self::InvalidName => write!(f, "invalid name"),
            Self::NameConflict => write!(f, "name conflict"),
            Self::SchemaConflict => write!(f, "schema conflict"),
            Self::UniqueConflict => write!(f, "unique conflict"),
            Self::InvalidValue => write!(f, "invalid value"),
            Self::Other(message) => write!(f, "{message}"),
        }
    }
}

/// Label or relationship type that scopes a declaration or an index.
///
/// # Examples
///
/// ```
/// use pizzlydb_testkit::ModelScope;
///
/// assert_ne!(ModelScope::Label(1), ModelScope::Type(1));
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ModelScope {
    /// Nodes that carry this label.
    Label(u128),
    /// Edges of this relationship type.
    Type(u128),
}

/// One graph mutation.
///
/// Identifiers are plain integers. Zero is rejected. The model assigns node,
/// edge, label, type, key, and index identifiers itself, starting at 1.
///
/// # Examples
///
/// ```
/// use pizzlydb_core::Value;
/// use pizzlydb_testkit::{Operation, ReferenceGraph};
///
/// let mut graph = ReferenceGraph::new();
/// graph
///     .apply(&Operation::InternKey { namespace: 1, name: "age".to_owned() })
///     .unwrap();
/// graph.apply(&Operation::CreateNode { namespace: 1 }).unwrap();
/// graph
///     .apply(&Operation::SetNodeProperty {
///         namespace: 1,
///         node: 1,
///         key: 1,
///         value: Value::Int(3),
///     })
///     .unwrap();
/// ```
#[derive(Clone, Debug, PartialEq)]
pub enum Operation {
    /// Allocate a node.
    CreateNode {
        /// Namespace that receives the node.
        namespace: u128,
    },
    /// Delete a node that has no incident edges.
    DeleteNode {
        /// Namespace of the node.
        namespace: u128,
        /// Node identifier.
        node: u128,
    },
    /// Allocate a directed edge.
    CreateEdge {
        /// Namespace of the edge.
        namespace: u128,
        /// Source node.
        source: u128,
        /// Target node.
        target: u128,
        /// Issued relationship type.
        edge_type: u128,
    },
    /// Delete an edge.
    DeleteEdge {
        /// Namespace of the edge.
        namespace: u128,
        /// Edge identifier.
        edge: u128,
    },
    /// Intern a label name.
    InternLabel {
        /// Namespace of the catalog.
        namespace: u128,
        /// Label name.
        name: String,
    },
    /// Intern a relationship-type name.
    InternType {
        /// Namespace of the catalog.
        namespace: u128,
        /// Type name.
        name: String,
    },
    /// Intern a property-key name.
    InternKey {
        /// Namespace of the catalog.
        namespace: u128,
        /// Key name.
        name: String,
    },
    /// Rename a label.
    RenameLabel {
        /// Namespace of the catalog.
        namespace: u128,
        /// Issued label.
        id: u128,
        /// New name.
        name: String,
    },
    /// Rename a relationship type.
    RenameType {
        /// Namespace of the catalog.
        namespace: u128,
        /// Issued type.
        id: u128,
        /// New name.
        name: String,
    },
    /// Rename a property key.
    RenameKey {
        /// Namespace of the catalog.
        namespace: u128,
        /// Issued key.
        id: u128,
        /// New name.
        name: String,
    },
    /// Add a label to a live node.
    AddLabel {
        /// Namespace of the node.
        namespace: u128,
        /// Node identifier.
        node: u128,
        /// Issued label.
        label: u128,
    },
    /// Remove a label. Removing an absent label succeeds.
    RemoveLabel {
        /// Namespace of the node.
        namespace: u128,
        /// Node identifier.
        node: u128,
        /// Label identifier. It does not have to be issued when it is absent.
        label: u128,
    },
    /// Set a property on a live node.
    SetNodeProperty {
        /// Namespace of the node.
        namespace: u128,
        /// Node identifier.
        node: u128,
        /// Issued property key.
        key: u128,
        /// Cell value, including null.
        value: Value,
    },
    /// Set a property on a live edge.
    SetEdgeProperty {
        /// Namespace of the edge.
        namespace: u128,
        /// Edge identifier.
        edge: u128,
        /// Issued property key.
        key: u128,
        /// Cell value, including null.
        value: Value,
    },
    /// Remove a node property. Clearing an absent property succeeds.
    ClearNodeProperty {
        /// Namespace of the node.
        namespace: u128,
        /// Node identifier.
        node: u128,
        /// Property key.
        key: u128,
    },
    /// Remove an edge property. Clearing an absent property succeeds.
    ClearEdgeProperty {
        /// Namespace of the edge.
        namespace: u128,
        /// Edge identifier.
        edge: u128,
        /// Property key.
        key: u128,
    },
    /// Declare a property inside a scope.
    Declare {
        /// Namespace of the declaration.
        namespace: u128,
        /// Label or relationship type.
        scope: ModelScope,
        /// Issued property key.
        key: u128,
        /// Required type of a non-null value.
        value_type: ValueType,
        /// Whether null is accepted.
        nullable: bool,
        /// Whether the property must be present.
        required: bool,
    },
    /// Create an index, or return the existing identical index.
    CreateIndex {
        /// Namespace of the index.
        namespace: u128,
        /// Label or relationship type.
        scope: ModelScope,
        /// Indexed property key.
        key: u128,
        /// Reject a repeated non-null value when true.
        unique: bool,
    },
}

/// Seed and the operations produced from it.
///
/// # Examples
///
/// ```
/// use pizzlydb_testkit::{History, Operation};
///
/// let history = History {
///     seed: 1,
///     ops: vec![Operation::CreateNode { namespace: 1 }],
/// };
/// assert_eq!(history.ops.len(), 1);
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct History {
    /// Seed passed to [`crate::generate_history`], or a hash of a proptest case.
    pub seed: u64,
    /// Operations in application order.
    pub ops: Vec<Operation>,
}

/// Counters for one namespace. The next identifier to allocate is the value.
///
/// # Examples
///
/// ```
/// use pizzlydb_testkit::Counters;
///
/// let counters = Counters {
///     next_node: 2,
///     next_edge: 1,
///     next_label: 1,
///     next_type: 1,
///     next_key: 1,
///     next_index: 1,
/// };
/// assert_eq!(counters.next_node, 2);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Counters {
    /// Next node identifier.
    pub next_node: u128,
    /// Next edge identifier.
    pub next_edge: u128,
    /// Next label identifier.
    pub next_label: u128,
    /// Next relationship-type identifier.
    pub next_type: u128,
    /// Next property-key identifier.
    pub next_key: u128,
    /// Next index identifier.
    pub next_index: u128,
}

/// Live edge endpoints.
///
/// # Examples
///
/// ```
/// use pizzlydb_testkit::EdgeFact;
///
/// let edge = EdgeFact { source: 1, target: 1, edge_type: 1 };
/// assert_eq!(edge.source, edge.target);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EdgeFact {
    /// Source node.
    pub source: u128,
    /// Target node.
    pub target: u128,
    /// Relationship type.
    pub edge_type: u128,
}

/// Stored declaration.
///
/// # Examples
///
/// ```
/// use pizzlydb_core::ValueType;
/// use pizzlydb_testkit::DeclFact;
///
/// let decl = DeclFact { type_tag: ValueType::Int.tag(), nullable: false, required: false };
/// assert_eq!(decl.type_tag, ValueType::Int.tag());
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DeclFact {
    /// [`ValueType::tag`](pizzlydb_core::ValueType::tag).
    pub type_tag: u8,
    /// Whether null is accepted.
    pub nullable: bool,
    /// Whether the property must be present.
    pub required: bool,
}

/// Index definition.
///
/// # Examples
///
/// ```
/// use pizzlydb_testkit::{IndexFact, SCOPE_LABEL};
///
/// let index = IndexFact { scope_kind: SCOPE_LABEL, scope: 1, key: 1, unique: true };
/// assert!(index.unique);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexFact {
    /// [`SCOPE_LABEL`] or [`SCOPE_TYPE`].
    pub scope_kind: u8,
    /// Label or type identifier.
    pub scope: u128,
    /// Property key.
    pub key: u128,
    /// Whether the index is unique.
    pub unique: bool,
}

/// One adjacency entry.
///
/// # Examples
///
/// ```
/// use pizzlydb_testkit::{DIR_OUT, NeighborFact};
///
/// let fact = NeighborFact {
///     namespace: 1,
///     node: 1,
///     direction: DIR_OUT,
///     edge_type: 1,
///     neighbor: 2,
///     edge: 1,
/// };
/// assert_eq!(fact.direction, DIR_OUT);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct NeighborFact {
    /// Namespace.
    pub namespace: u128,
    /// Node that owns the adjacency key.
    pub node: u128,
    /// [`DIR_OUT`] or [`DIR_IN`].
    pub direction: u8,
    /// Relationship type.
    pub edge_type: u128,
    /// The other endpoint.
    pub neighbor: u128,
    /// Edge identifier.
    pub edge: u128,
}

/// One index entry.
///
/// `value` is the cell encoding of [`encode_value`](pizzlydb_core::encode_value).
/// A unique index has no entry for null.
///
/// # Examples
///
/// ```
/// use pizzlydb_testkit::{IndexHit, OWNER_NODE};
///
/// let hit = IndexHit {
///     namespace: 1,
///     index: 1,
///     value: vec![1],
///     owner_kind: OWNER_NODE,
///     owner: 1,
/// };
/// assert_eq!(hit.owner_kind, OWNER_NODE);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct IndexHit {
    /// Namespace.
    pub namespace: u128,
    /// Index identifier.
    pub index: u128,
    /// Encoded property value.
    pub value: Vec<u8>,
    /// [`OWNER_NODE`] or [`OWNER_EDGE`].
    pub owner_kind: u8,
    /// Owner identifier.
    pub owner: u128,
}

/// Observable graph state.
///
/// Property bytes are [`encode_value`](pizzlydb_core::encode_value) output.
/// Name keys are `(namespace, name kind, identifier)`.
///
/// # Examples
///
/// ```
/// use pizzlydb_testkit::{Operation, ReferenceGraph};
///
/// let mut graph = ReferenceGraph::new();
/// graph.apply(&Operation::CreateNode { namespace: 1 }).unwrap();
/// let snapshot = graph.snapshot().unwrap();
/// assert!(snapshot.nodes.contains(&(1, 1)));
/// assert_eq!(snapshot.counters[&1].next_node, 2);
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphSnapshot {
    /// Next identifiers for namespaces that have been written.
    pub counters: BTreeMap<u128, Counters>,
    /// Catalog names.
    pub names: BTreeMap<(u128, u8, u128), String>,
    /// Live nodes, as `(namespace, node)`.
    pub nodes: BTreeSet<(u128, u128)>,
    /// Deleted nodes.
    pub node_tombstones: BTreeSet<(u128, u128)>,
    /// Live edges.
    pub edges: BTreeMap<(u128, u128), EdgeFact>,
    /// Deleted edges.
    pub edge_tombstones: BTreeSet<(u128, u128)>,
    /// Node labels, as `(namespace, node, label)`.
    pub labels: BTreeSet<(u128, u128, u128)>,
    /// Properties, as `(namespace, owner kind, owner, key)` to cell bytes.
    pub properties: BTreeMap<(u128, u8, u128, u128), Vec<u8>>,
    /// Declarations, as `(namespace, scope kind, scope, key)`.
    pub declarations: BTreeMap<(u128, u8, u128, u128), DeclFact>,
    /// Index definitions, as `(namespace, index)`.
    pub indexes: BTreeMap<(u128, u128), IndexFact>,
    /// Adjacency of every live edge.
    pub neighbors: BTreeSet<NeighborFact>,
    /// Index entries of every live owner in scope.
    pub index_hits: BTreeSet<IndexHit>,
}

/// Reference graph.
///
/// Apply one [`Operation`] at a time. A failed call leaves the model unchanged.
///
/// # Examples
///
/// ```
/// use pizzlydb_testkit::{Operation, ReferenceGraph, StepError};
///
/// let mut graph = ReferenceGraph::new();
/// graph
///     .apply(&Operation::InternType { namespace: 1, name: "KNOWS".to_owned() })
///     .unwrap();
/// graph.apply(&Operation::CreateNode { namespace: 1 }).unwrap();
/// graph
///     .apply(&Operation::CreateEdge {
///         namespace: 1,
///         source: 1,
///         target: 1,
///         edge_type: 1,
///     })
///     .unwrap();
/// assert_eq!(
///     graph.apply(&Operation::DeleteNode { namespace: 1, node: 1 }),
///     Err(StepError::EndpointInUse)
/// );
/// ```
#[derive(Clone, Debug, Default)]
pub struct ReferenceGraph {
    namespaces: BTreeMap<u128, Ns>,
}

#[derive(Clone, Debug)]
struct Ns {
    next_node: u128,
    next_edge: u128,
    next_label: u128,
    next_type: u128,
    next_key: u128,
    next_index: u128,
    labels: BTreeMap<u128, String>,
    types: BTreeMap<u128, String>,
    keys: BTreeMap<u128, String>,
    nodes: BTreeMap<u128, NodeState>,
    node_tombstones: BTreeSet<u128>,
    edges: BTreeMap<u128, EdgeState>,
    edge_tombstones: BTreeSet<u128>,
    indexes: BTreeMap<u128, IndexState>,
    declarations: BTreeMap<(u8, u128, u128), Decl>,
}

#[derive(Clone, Debug, Default)]
struct NodeState {
    labels: BTreeSet<u128>,
    props: BTreeMap<u128, Value>,
}

#[derive(Clone, Debug)]
struct EdgeState {
    source: u128,
    target: u128,
    edge_type: u128,
    props: BTreeMap<u128, Value>,
}

#[derive(Clone, Debug)]
struct IndexState {
    scope_kind: u8,
    scope: u128,
    key: u128,
    unique: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Decl {
    type_tag: u8,
    nullable: bool,
    required: bool,
}

impl Ns {
    fn new() -> Self {
        Self {
            next_node: 1,
            next_edge: 1,
            next_label: 1,
            next_type: 1,
            next_key: 1,
            next_index: 1,
            labels: BTreeMap::new(),
            types: BTreeMap::new(),
            keys: BTreeMap::new(),
            nodes: BTreeMap::new(),
            node_tombstones: BTreeSet::new(),
            edges: BTreeMap::new(),
            edge_tombstones: BTreeSet::new(),
            indexes: BTreeMap::new(),
            declarations: BTreeMap::new(),
        }
    }
}

impl Default for Ns {
    fn default() -> Self {
        Self::new()
    }
}

impl ReferenceGraph {
    /// Empty graph.
    ///
    /// # Examples
    ///
    /// ```
    /// use pizzlydb_testkit::ReferenceGraph;
    ///
    /// let graph = ReferenceGraph::new();
    /// assert!(graph.snapshot().unwrap().nodes.is_empty());
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self {
            namespaces: BTreeMap::new(),
        }
    }

    /// Drop every namespace.
    ///
    /// # Examples
    ///
    /// ```
    /// use pizzlydb_testkit::{Operation, ReferenceGraph};
    ///
    /// let mut graph = ReferenceGraph::new();
    /// graph.apply(&Operation::CreateNode { namespace: 1 }).unwrap();
    /// graph.reset();
    /// assert!(graph.snapshot().unwrap().nodes.is_empty());
    /// ```
    pub fn reset(&mut self) {
        self.namespaces.clear();
    }

    /// Apply `op`. On error the model is unchanged.
    ///
    /// # Errors
    ///
    /// Returns the graph error for `op`.
    ///
    /// # Examples
    ///
    /// ```
    /// use pizzlydb_testkit::{Operation, ReferenceGraph};
    ///
    /// let mut graph = ReferenceGraph::new();
    /// assert!(graph.apply(&Operation::CreateNode { namespace: 0 }).is_err());
    /// assert!(graph.snapshot().unwrap().counters.is_empty());
    /// ```
    pub fn apply(&mut self, op: &Operation) -> Result<(), StepError> {
        let mut trial = self.clone();
        trial.apply_mut(op)?;
        *self = trial;
        Ok(())
    }

    /// Live edge `(source, target, type)`, if `id` is live.
    ///
    /// # Examples
    ///
    /// ```
    /// use pizzlydb_testkit::{Operation, ReferenceGraph};
    ///
    /// let mut graph = ReferenceGraph::new();
    /// assert_eq!(graph.edge(1, 1), None);
    /// ```
    #[must_use]
    pub fn edge(&self, namespace: u128, id: u128) -> Option<(u128, u128, u128)> {
        let edge = self.namespaces.get(&namespace)?.edges.get(&id)?;
        Some((edge.source, edge.target, edge.edge_type))
    }

    /// Observable state.
    ///
    /// # Errors
    ///
    /// Returns [`StepError::InvalidValue`] when a stored value cannot be encoded.
    ///
    /// # Examples
    ///
    /// ```
    /// use pizzlydb_testkit::ReferenceGraph;
    ///
    /// assert!(ReferenceGraph::new().snapshot().unwrap().neighbors.is_empty());
    /// ```
    pub fn snapshot(&self) -> Result<GraphSnapshot, StepError> {
        let mut snapshot = GraphSnapshot {
            counters: BTreeMap::new(),
            names: BTreeMap::new(),
            nodes: BTreeSet::new(),
            node_tombstones: BTreeSet::new(),
            edges: BTreeMap::new(),
            edge_tombstones: BTreeSet::new(),
            labels: BTreeSet::new(),
            properties: BTreeMap::new(),
            declarations: BTreeMap::new(),
            indexes: BTreeMap::new(),
            neighbors: BTreeSet::new(),
            index_hits: BTreeSet::new(),
        };

        for (namespace, ns) in &self.namespaces {
            snapshot.counters.insert(
                *namespace,
                Counters {
                    next_node: ns.next_node,
                    next_edge: ns.next_edge,
                    next_label: ns.next_label,
                    next_type: ns.next_type,
                    next_key: ns.next_key,
                    next_index: ns.next_index,
                },
            );

            copy_names(&mut snapshot, *namespace, NAME_LABEL, &ns.labels);
            copy_names(&mut snapshot, *namespace, NAME_TYPE, &ns.types);
            copy_names(&mut snapshot, *namespace, NAME_KEY, &ns.keys);

            for id in ns.nodes.keys() {
                snapshot.nodes.insert((*namespace, *id));
            }

            for id in &ns.node_tombstones {
                snapshot.node_tombstones.insert((*namespace, *id));
            }

            for id in &ns.edge_tombstones {
                snapshot.edge_tombstones.insert((*namespace, *id));
            }

            for (id, edge) in &ns.edges {
                snapshot.edges.insert(
                    (*namespace, *id),
                    EdgeFact {
                        source: edge.source,
                        target: edge.target,
                        edge_type: edge.edge_type,
                    },
                );
                snapshot.neighbors.insert(NeighborFact {
                    namespace: *namespace,
                    node: edge.source,
                    direction: DIR_OUT,
                    edge_type: edge.edge_type,
                    neighbor: edge.target,
                    edge: *id,
                });
                snapshot.neighbors.insert(NeighborFact {
                    namespace: *namespace,
                    node: edge.target,
                    direction: DIR_IN,
                    edge_type: edge.edge_type,
                    neighbor: edge.source,
                    edge: *id,
                });
                copy_props(&mut snapshot, *namespace, OWNER_EDGE, *id, &edge.props)?;
            }

            for (id, node) in &ns.nodes {
                for label in &node.labels {
                    snapshot.labels.insert((*namespace, *id, *label));
                }
                copy_props(&mut snapshot, *namespace, OWNER_NODE, *id, &node.props)?;
            }

            for ((kind, scope, key), decl) in &ns.declarations {
                snapshot.declarations.insert(
                    (*namespace, *kind, *scope, *key),
                    DeclFact {
                        type_tag: decl.type_tag,
                        nullable: decl.nullable,
                        required: decl.required,
                    },
                );
            }

            for (id, index) in &ns.indexes {
                snapshot.indexes.insert(
                    (*namespace, *id),
                    IndexFact {
                        scope_kind: index.scope_kind,
                        scope: index.scope,
                        key: index.key,
                        unique: index.unique,
                    },
                );

                for (owner_kind, owner) in members(ns, index) {
                    let Some(value) = property(ns, owner_kind, owner, index.key) else {
                        continue;
                    };
                    if index.unique && *value == Value::Null {
                        continue;
                    }
                    snapshot.index_hits.insert(IndexHit {
                        namespace: *namespace,
                        index: *id,
                        value: cell_bytes(value)?,
                        owner_kind,
                        owner,
                    });
                }
            }
        }

        Ok(snapshot)
    }

    fn apply_mut(&mut self, op: &Operation) -> Result<(), StepError> {
        match op {
            Operation::CreateNode { namespace } => self.create_node(*namespace),
            Operation::DeleteNode { namespace, node } => self.delete_node(*namespace, *node),
            Operation::CreateEdge {
                namespace,
                source,
                target,
                edge_type,
            } => self.create_edge(*namespace, *source, *target, *edge_type),
            Operation::DeleteEdge { namespace, edge } => self.delete_edge(*namespace, *edge),
            Operation::InternLabel { namespace, name } => self.intern(*namespace, NAME_LABEL, name),
            Operation::InternType { namespace, name } => self.intern(*namespace, NAME_TYPE, name),
            Operation::InternKey { namespace, name } => self.intern(*namespace, NAME_KEY, name),
            Operation::RenameLabel {
                namespace,
                id,
                name,
            } => self.rename(*namespace, NAME_LABEL, *id, name),
            Operation::RenameType {
                namespace,
                id,
                name,
            } => self.rename(*namespace, NAME_TYPE, *id, name),
            Operation::RenameKey {
                namespace,
                id,
                name,
            } => self.rename(*namespace, NAME_KEY, *id, name),
            Operation::AddLabel {
                namespace,
                node,
                label,
            } => self.add_label(*namespace, *node, *label),
            Operation::RemoveLabel {
                namespace,
                node,
                label,
            } => self.remove_label(*namespace, *node, *label),
            Operation::SetNodeProperty {
                namespace,
                node,
                key,
                value,
            } => self.set_property(*namespace, OWNER_NODE, *node, *key, value),
            Operation::SetEdgeProperty {
                namespace,
                edge,
                key,
                value,
            } => self.set_property(*namespace, OWNER_EDGE, *edge, *key, value),
            Operation::ClearNodeProperty {
                namespace,
                node,
                key,
            } => self.clear_property(*namespace, OWNER_NODE, *node, *key),
            Operation::ClearEdgeProperty {
                namespace,
                edge,
                key,
            } => self.clear_property(*namespace, OWNER_EDGE, *edge, *key),
            Operation::Declare {
                namespace,
                scope,
                key,
                value_type,
                nullable,
                required,
            } => self.declare(*namespace, *scope, *key, *value_type, *nullable, *required),
            Operation::CreateIndex {
                namespace,
                scope,
                key,
                unique,
            } => self.create_index(*namespace, *scope, *key, *unique),
        }
    }

    fn create_node(&mut self, namespace: u128) -> Result<(), StepError> {
        let ns = self.ns_mut(namespace)?;
        let id = alloc(&mut ns.next_node)?;
        ns.nodes.insert(id, NodeState::default());
        Ok(())
    }

    fn delete_node(&mut self, namespace: u128, node: u128) -> Result<(), StepError> {
        self.require_node(namespace, node)?;
        let ns = self.namespaces.get(&namespace).expect("node namespace");
        if ns
            .edges
            .values()
            .any(|edge| edge.source == node || edge.target == node)
        {
            return Err(StepError::EndpointInUse);
        }

        let ns = self.ns_mut(namespace)?;
        ns.nodes.remove(&node);
        ns.node_tombstones.insert(node);

        Ok(())
    }

    fn create_edge(
        &mut self,
        namespace: u128,
        source: u128,
        target: u128,
        edge_type: u128,
    ) -> Result<(), StepError> {
        self.require_catalog(namespace, NAME_TYPE, edge_type)?;
        self.require_endpoint(namespace, source)?;
        self.require_endpoint(namespace, target)?;
        let ns = self.ns_mut(namespace)?;
        let id = alloc(&mut ns.next_edge)?;

        ns.edges.insert(
            id,
            EdgeState {
                source,
                target,
                edge_type,
                props: BTreeMap::new(),
            },
        );

        Ok(())
    }

    fn delete_edge(&mut self, namespace: u128, edge: u128) -> Result<(), StepError> {
        self.require_edge(namespace, edge)?;
        let ns = self.ns_mut(namespace)?;
        ns.edges.remove(&edge);
        ns.edge_tombstones.insert(edge);
        Ok(())
    }

    fn intern(&mut self, namespace: u128, kind: u8, name: &str) -> Result<(), StepError> {
        id_ok(namespace)?;
        valid_name(name)?;

        let exists = self
            .namespaces
            .get(&namespace)
            .is_some_and(|ns| names(ns, kind).values().any(|current| current == name));

        if exists {
            return Ok(());
        }

        let ns = self.ns_mut(namespace)?;
        let id = alloc(counter_mut(ns, kind))?;
        names_mut(ns, kind).insert(id, name.to_owned());

        Ok(())
    }

    fn rename(&mut self, namespace: u128, kind: u8, id: u128, name: &str) -> Result<(), StepError> {
        id_ok(namespace)?;
        id_ok(id)?;
        valid_name(name)?;

        let ns = self.ns_mut(namespace)?;
        let names = names_mut(ns, kind);
        if !names.contains_key(&id) {
            return Err(StepError::InvalidIdentity);
        }

        if names.get(&id).is_some_and(|current| current == name) {
            return Ok(());
        }

        if names.values().any(|current| current == name) {
            return Err(StepError::NameConflict);
        }

        names.insert(id, name.to_owned());

        Ok(())
    }

    fn add_label(&mut self, namespace: u128, node: u128, label: u128) -> Result<(), StepError> {
        self.require_node(namespace, node)?;
        self.require_catalog(namespace, NAME_LABEL, label)?;

        if self.namespaces.get(&namespace).expect("namespace").nodes[&node]
            .labels
            .contains(&label)
        {
            return Ok(());
        }

        self.require_label_unique(namespace, node, label)?;
        self.ns_mut(namespace)?
            .nodes
            .get_mut(&node)
            .expect("node")
            .labels
            .insert(label);

        Ok(())
    }

    fn remove_label(&mut self, namespace: u128, node: u128, label: u128) -> Result<(), StepError> {
        self.require_node(namespace, node)?;
        id_ok(label)?;
        let ns = self.ns_mut(namespace)?;
        ns.nodes.get_mut(&node).expect("node").labels.remove(&label);
        Ok(())
    }

    fn set_property(
        &mut self,
        namespace: u128,
        owner_kind: u8,
        owner: u128,
        key: u128,
        value: &Value,
    ) -> Result<(), StepError> {
        self.require_owner(namespace, owner_kind, owner)?;
        self.require_catalog(namespace, NAME_KEY, key)?;
        reject_value(value)?;

        self.require_schema(namespace, owner_kind, owner, key, value)?;
        self.require_unique(namespace, owner_kind, owner, key, value)?;
        let ns = self.ns_mut(namespace)?;
        props_mut(ns, owner_kind, owner).insert(key, value.clone());

        Ok(())
    }

    fn clear_property(
        &mut self,
        namespace: u128,
        owner_kind: u8,
        owner: u128,
        key: u128,
    ) -> Result<(), StepError> {
        self.require_owner(namespace, owner_kind, owner)?;
        id_ok(key)?;
        let ns = self.ns_mut(namespace)?;
        props_mut(ns, owner_kind, owner).remove(&key);
        Ok(())
    }

    fn declare(
        &mut self,
        namespace: u128,
        scope: ModelScope,
        key: u128,
        value_type: ValueType,
        nullable: bool,
        required: bool,
    ) -> Result<(), StepError> {
        let (kind, scope_id) = scope_parts(scope);
        self.require_catalog(namespace, catalog_kind(kind), scope_id)?;
        self.require_catalog(namespace, NAME_KEY, key)?;
        let next = Decl {
            type_tag: value_type.tag(),
            nullable,
            required,
        };
        if let Some(ns) = self.namespaces.get(&namespace) {
            for ((other_kind, other_scope, other_key), existing) in &ns.declarations {
                if *other_kind == kind
                    && *other_key == key
                    && *other_scope != scope_id
                    && existing.type_tag != next.type_tag
                {
                    return Err(StepError::SchemaConflict);
                }
            }
        }

        let tightens = self
            .namespaces
            .get(&namespace)
            .and_then(|ns| ns.declarations.get(&(kind, scope_id, key)))
            .is_some_and(|current| {
                *current != next
                    && (current.type_tag != next.type_tag
                        || (current.nullable && !next.nullable)
                        || (!current.required && next.required))
            });
        if tightens
            && scope_violates(
                self.namespaces.get(&namespace).expect("namespace"),
                kind,
                scope_id,
                key,
                &next,
            )
        {
            return Err(StepError::SchemaConflict);
        }
        if self
            .namespaces
            .get(&namespace)
            .and_then(|ns| ns.declarations.get(&(kind, scope_id, key)))
            == Some(&next)
        {
            return Ok(());
        }

        self.ns_mut(namespace)?
            .declarations
            .insert((kind, scope_id, key), next);
        Ok(())
    }

    fn create_index(
        &mut self,
        namespace: u128,
        scope: ModelScope,
        key: u128,
        unique: bool,
    ) -> Result<(), StepError> {
        let (kind, scope_id) = scope_parts(scope);
        self.require_catalog(namespace, catalog_kind(kind), scope_id)?;
        self.require_catalog(namespace, NAME_KEY, key)?;
        let ns = self.namespaces.get(&namespace).expect("namespace");
        if ns.indexes.values().any(|index| {
            index.scope_kind == kind
                && index.scope == scope_id
                && index.key == key
                && index.unique == unique
        }) {
            return Ok(());
        }

        if unique {
            let mut seen: BTreeMap<Vec<u8>, ()> = BTreeMap::new();
            let index = IndexState {
                scope_kind: kind,
                scope: scope_id,
                key,
                unique,
            };

            for (owner_kind, owner) in members(ns, &index) {
                let Some(value) = property(ns, owner_kind, owner, key) else {
                    continue;
                };

                if *value == Value::Null {
                    continue;
                }

                let encoded = key_bytes(value)?;
                if seen.insert(encoded, ()).is_some() {
                    return Err(StepError::UniqueConflict);
                }
            }
        }

        let ns = self.ns_mut(namespace)?;
        let id = alloc(&mut ns.next_index)?;
        ns.indexes.insert(
            id,
            IndexState {
                scope_kind: kind,
                scope: scope_id,
                key,
                unique,
            },
        );

        Ok(())
    }

    fn require_node(&self, namespace: u128, node: u128) -> Result<(), StepError> {
        id_ok(namespace)?;
        id_ok(node)?;

        let Some(ns) = self.namespaces.get(&namespace) else {
            return Err(StepError::NotFound);
        };

        if ns.node_tombstones.contains(&node) {
            return Err(StepError::Deleted);
        }

        if ns.nodes.contains_key(&node) {
            Ok(())
        } else {
            Err(StepError::NotFound)
        }
    }

    fn require_edge(&self, namespace: u128, edge: u128) -> Result<(), StepError> {
        id_ok(namespace)?;
        id_ok(edge)?;

        let Some(ns) = self.namespaces.get(&namespace) else {
            return Err(StepError::NotFound);
        };

        if ns.edge_tombstones.contains(&edge) {
            return Err(StepError::Deleted);
        }

        if ns.edges.contains_key(&edge) {
            Ok(())
        } else {
            Err(StepError::NotFound)
        }
    }

    fn require_endpoint(&self, namespace: u128, node: u128) -> Result<(), StepError> {
        id_ok(node)?;

        let live = self
            .namespaces
            .get(&namespace)
            .is_some_and(|ns| ns.nodes.contains_key(&node));

        if live {
            Ok(())
        } else {
            Err(StepError::EndpointNotLive)
        }
    }

    fn require_owner(&self, namespace: u128, owner_kind: u8, owner: u128) -> Result<(), StepError> {
        if owner_kind == OWNER_NODE {
            self.require_node(namespace, owner)
        } else {
            self.require_edge(namespace, owner)
        }
    }

    fn require_catalog(&self, namespace: u128, kind: u8, id: u128) -> Result<(), StepError> {
        id_ok(namespace)?;
        id_ok(id)?;

        let issued = self
            .namespaces
            .get(&namespace)
            .is_some_and(|ns| names(ns, kind).contains_key(&id));

        if issued {
            Ok(())
        } else {
            Err(StepError::InvalidIdentity)
        }
    }

    fn require_schema(
        &self,
        namespace: u128,
        owner_kind: u8,
        owner: u128,
        key: u128,
        value: &Value,
    ) -> Result<(), StepError> {
        let Some(ns) = self.namespaces.get(&namespace) else {
            return Ok(());
        };
        for decl in applicable_all(ns, owner_kind, owner, key) {
            if !accepts(decl, value) {
                return Err(StepError::SchemaConflict);
            }
        }
        Ok(())
    }

    fn require_unique(
        &self,
        namespace: u128,
        owner_kind: u8,
        owner: u128,
        key: u128,
        value: &Value,
    ) -> Result<(), StepError> {
        if *value == Value::Null {
            return Ok(());
        }

        let Some(ns) = self.namespaces.get(&namespace) else {
            return Ok(());
        };

        let needle = key_bytes(value)?;
        for index in ns.indexes.values() {
            if !index.unique || index.key != key || !in_scope(ns, index, owner_kind, owner) {
                continue;
            }

            for (other_kind, other) in members(ns, index) {
                if other_kind == owner_kind && other == owner {
                    continue;
                }

                let Some(current) = property(ns, other_kind, other, key) else {
                    continue;
                };

                if *current != Value::Null && key_bytes(current)? == needle {
                    return Err(StepError::UniqueConflict);
                }
            }
        }
        Ok(())
    }

    fn require_label_unique(
        &self,
        namespace: u128,
        node: u128,
        label: u128,
    ) -> Result<(), StepError> {
        let ns = self.namespaces.get(&namespace).expect("namespace");
        let props = ns.nodes.get(&node).expect("node").props.clone();

        for (key, value) in props {
            if value == Value::Null {
                continue;
            }

            for index in ns.indexes.values() {
                if index.unique
                    && index.scope_kind == SCOPE_LABEL
                    && index.scope == label
                    && index.key == key
                    && value_taken(ns, index, OWNER_NODE, node, &value)?
                {
                    return Err(StepError::UniqueConflict);
                }
            }
        }
        Ok(())
    }

    fn ns_mut(&mut self, namespace: u128) -> Result<&mut Ns, StepError> {
        id_ok(namespace)?;
        Ok(self.namespaces.entry(namespace).or_default())
    }
}

fn copy_names(
    snapshot: &mut GraphSnapshot,
    namespace: u128,
    kind: u8,
    names: &BTreeMap<u128, String>,
) {
    for (id, name) in names {
        snapshot.names.insert((namespace, kind, *id), name.clone());
    }
}

fn copy_props(
    snapshot: &mut GraphSnapshot,
    namespace: u128,
    owner_kind: u8,
    owner: u128,
    props: &BTreeMap<u128, Value>,
) -> Result<(), StepError> {
    for (key, value) in props {
        snapshot
            .properties
            .insert((namespace, owner_kind, owner, *key), cell_bytes(value)?);
    }
    Ok(())
}

fn id_ok(id: u128) -> Result<(), StepError> {
    NodeId::from_u128(id)
        .map(|_| ())
        .map_err(|_| StepError::InvalidIdentity)
}

fn valid_name(name: &str) -> Result<(), StepError> {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_NAME_LEN || bytes.contains(&0) {
        Err(StepError::InvalidName)
    } else {
        Ok(())
    }
}

fn reject_value(value: &Value) -> Result<(), StepError> {
    cell_bytes(value).map(|_| ())
}

fn cell_bytes(value: &Value) -> Result<Vec<u8>, StepError> {
    encode_value(value).map_err(|_| StepError::InvalidValue)
}

fn key_bytes(value: &Value) -> Result<Vec<u8>, StepError> {
    let mut out = Vec::new();
    encode_value_key(value, &mut out).map_err(|_| StepError::InvalidValue)?;
    Ok(out)
}

fn alloc(next: &mut u128) -> Result<u128, StepError> {
    let id = *next;
    *next = next
        .checked_add(1)
        .ok_or_else(|| StepError::Other("identifier space is exhausted".to_owned()))?;
    Ok(id)
}

fn names(ns: &Ns, kind: u8) -> &BTreeMap<u128, String> {
    match kind {
        NAME_LABEL => &ns.labels,
        NAME_TYPE => &ns.types,
        _ => &ns.keys,
    }
}

fn names_mut(ns: &mut Ns, kind: u8) -> &mut BTreeMap<u128, String> {
    match kind {
        NAME_LABEL => &mut ns.labels,
        NAME_TYPE => &mut ns.types,
        _ => &mut ns.keys,
    }
}

fn counter_mut(ns: &mut Ns, kind: u8) -> &mut u128 {
    match kind {
        NAME_LABEL => &mut ns.next_label,
        NAME_TYPE => &mut ns.next_type,
        _ => &mut ns.next_key,
    }
}

fn props_mut(ns: &mut Ns, owner_kind: u8, owner: u128) -> &mut BTreeMap<u128, Value> {
    if owner_kind == OWNER_NODE {
        &mut ns.nodes.get_mut(&owner).expect("live node").props
    } else {
        &mut ns.edges.get_mut(&owner).expect("live edge").props
    }
}

fn property(ns: &Ns, owner_kind: u8, owner: u128, key: u128) -> Option<&Value> {
    if owner_kind == OWNER_NODE {
        ns.nodes.get(&owner)?.props.get(&key)
    } else {
        ns.edges.get(&owner)?.props.get(&key)
    }
}

fn members(ns: &Ns, index: &IndexState) -> Vec<(u8, u128)> {
    if index.scope_kind == SCOPE_LABEL {
        ns.nodes
            .iter()
            .filter(|(_, node)| node.labels.contains(&index.scope))
            .map(|(id, _)| (OWNER_NODE, *id))
            .collect()
    } else {
        ns.edges
            .iter()
            .filter(|(_, edge)| edge.edge_type == index.scope)
            .map(|(id, _)| (OWNER_EDGE, *id))
            .collect()
    }
}

fn in_scope(ns: &Ns, index: &IndexState, owner_kind: u8, owner: u128) -> bool {
    members(ns, index)
        .into_iter()
        .any(|(kind, id)| kind == owner_kind && id == owner)
}

fn value_taken(
    ns: &Ns,
    index: &IndexState,
    owner_kind: u8,
    owner: u128,
    value: &Value,
) -> Result<bool, StepError> {
    let needle = key_bytes(value)?;
    for (kind, id) in members(ns, index) {
        if kind == owner_kind && id == owner {
            continue;
        }
        if let Some(current) = property(ns, kind, id, index.key)
            && *current != Value::Null
            && key_bytes(current)? == needle
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn applicable_all(ns: &Ns, owner_kind: u8, owner: u128, key: u128) -> Vec<&Decl> {
    if owner_kind == OWNER_NODE {
        let Some(node) = ns.nodes.get(&owner) else {
            return Vec::new();
        };
        node.labels
            .iter()
            .filter_map(|label| ns.declarations.get(&(SCOPE_LABEL, *label, key)))
            .collect()
    } else {
        let Some(edge) = ns.edges.get(&owner) else {
            return Vec::new();
        };
        ns.declarations
            .get(&(SCOPE_TYPE, edge.edge_type, key))
            .into_iter()
            .collect()
    }
}

fn scope_violates(ns: &Ns, kind: u8, scope: u128, key: u128, decl: &Decl) -> bool {
    let owners: Vec<(u8, u128)> = if kind == SCOPE_LABEL {
        ns.nodes
            .iter()
            .filter(|(_, node)| node.labels.contains(&scope))
            .map(|(id, _)| (OWNER_NODE, *id))
            .collect()
    } else {
        ns.edges
            .iter()
            .filter(|(_, edge)| edge.edge_type == scope)
            .map(|(id, _)| (OWNER_EDGE, *id))
            .collect()
    };
    for (owner_kind, owner) in owners {
        match property(ns, owner_kind, owner, key) {
            None if decl.required => return true,
            Some(value) if !accepts(decl, value) => return true,
            _ => {}
        }
    }
    false
}

fn accepts(decl: &Decl, value: &Value) -> bool {
    match value.value_type() {
        None => decl.nullable,
        Some(value_type) => value_type.tag() == decl.type_tag,
    }
}

fn scope_parts(scope: ModelScope) -> (u8, u128) {
    match scope {
        ModelScope::Label(id) => (SCOPE_LABEL, id),
        ModelScope::Type(id) => (SCOPE_TYPE, id),
    }
}

fn catalog_kind(scope_kind: u8) -> u8 {
    if scope_kind == SCOPE_LABEL {
        NAME_LABEL
    } else {
        NAME_TYPE
    }
}

#[cfg(test)]
mod tests {
    use super::{DIR_IN, DIR_OUT, ModelScope, Operation, ReferenceGraph, StepError};
    use pizzlydb_core::{Value, ValueType};

    fn apply(graph: &mut ReferenceGraph, op: Operation) -> Result<(), StepError> {
        graph.apply(&op)
    }

    #[test]
    fn self_loop_and_parallel_edges_are_neighbors() {
        let mut graph = ReferenceGraph::new();
        apply(
            &mut graph,
            Operation::InternType {
                namespace: 1,
                name: "KNOWS".into(),
            },
        )
        .unwrap();
        apply(&mut graph, Operation::CreateNode { namespace: 1 }).unwrap();
        apply(&mut graph, Operation::CreateNode { namespace: 1 }).unwrap();
        apply(
            &mut graph,
            Operation::CreateEdge {
                namespace: 1,
                source: 1,
                target: 1,
                edge_type: 1,
            },
        )
        .unwrap();
        apply(
            &mut graph,
            Operation::CreateEdge {
                namespace: 1,
                source: 1,
                target: 2,
                edge_type: 1,
            },
        )
        .unwrap();
        apply(
            &mut graph,
            Operation::CreateEdge {
                namespace: 1,
                source: 1,
                target: 2,
                edge_type: 1,
            },
        )
        .unwrap();
        let snapshot = graph.snapshot().unwrap();
        let out = snapshot
            .neighbors
            .iter()
            .filter(|item| item.node == 1 && item.direction == DIR_OUT);
        assert_eq!(out.count(), 3);
        let incoming = snapshot
            .neighbors
            .iter()
            .find(|item| item.node == 1 && item.direction == DIR_IN && item.edge == 1)
            .unwrap();
        assert_eq!(incoming.neighbor, 1);
    }

    #[test]
    fn delete_edge_unlinks_both_directions_and_delete_node_tombstones() {
        let mut graph = ReferenceGraph::new();
        apply(
            &mut graph,
            Operation::InternType {
                namespace: 1,
                name: "KNOWS".into(),
            },
        )
        .unwrap();
        apply(
            &mut graph,
            Operation::InternLabel {
                namespace: 1,
                name: "Person".into(),
            },
        )
        .unwrap();
        apply(&mut graph, Operation::CreateNode { namespace: 1 }).unwrap();
        apply(
            &mut graph,
            Operation::AddLabel {
                namespace: 1,
                node: 1,
                label: 1,
            },
        )
        .unwrap();
        apply(
            &mut graph,
            Operation::CreateEdge {
                namespace: 1,
                source: 1,
                target: 1,
                edge_type: 1,
            },
        )
        .unwrap();
        assert_eq!(
            apply(
                &mut graph,
                Operation::DeleteNode {
                    namespace: 1,
                    node: 1
                }
            ),
            Err(StepError::EndpointInUse)
        );
        apply(
            &mut graph,
            Operation::DeleteEdge {
                namespace: 1,
                edge: 1,
            },
        )
        .unwrap();
        apply(
            &mut graph,
            Operation::DeleteNode {
                namespace: 1,
                node: 1,
            },
        )
        .unwrap();
        let snapshot = graph.snapshot().unwrap();
        assert!(snapshot.neighbors.is_empty());
        assert!(snapshot.labels.is_empty());
        assert!(snapshot.node_tombstones.contains(&(1, 1)));
        assert_eq!(
            apply(
                &mut graph,
                Operation::DeleteNode {
                    namespace: 1,
                    node: 1
                }
            ),
            Err(StepError::Deleted)
        );
        apply(&mut graph, Operation::CreateNode { namespace: 1 }).unwrap();
        assert!(graph.snapshot().unwrap().nodes.contains(&(1, 2)));
    }

    #[test]
    fn unique_index_rejects_a_duplicate_and_allows_nulls() {
        let mut graph = ReferenceGraph::new();
        apply(
            &mut graph,
            Operation::InternLabel {
                namespace: 1,
                name: "Person".into(),
            },
        )
        .unwrap();
        apply(
            &mut graph,
            Operation::InternKey {
                namespace: 1,
                name: "email".into(),
            },
        )
        .unwrap();
        apply(&mut graph, Operation::CreateNode { namespace: 1 }).unwrap();
        apply(&mut graph, Operation::CreateNode { namespace: 1 }).unwrap();
        apply(
            &mut graph,
            Operation::AddLabel {
                namespace: 1,
                node: 1,
                label: 1,
            },
        )
        .unwrap();
        apply(
            &mut graph,
            Operation::AddLabel {
                namespace: 1,
                node: 2,
                label: 1,
            },
        )
        .unwrap();
        apply(
            &mut graph,
            Operation::CreateIndex {
                namespace: 1,
                scope: ModelScope::Label(1),
                key: 1,
                unique: true,
            },
        )
        .unwrap();
        apply(
            &mut graph,
            Operation::SetNodeProperty {
                namespace: 1,
                node: 1,
                key: 1,
                value: Value::Null,
            },
        )
        .unwrap();
        apply(
            &mut graph,
            Operation::SetNodeProperty {
                namespace: 1,
                node: 2,
                key: 1,
                value: Value::Null,
            },
        )
        .unwrap();
        apply(
            &mut graph,
            Operation::SetNodeProperty {
                namespace: 1,
                node: 1,
                key: 1,
                value: Value::from("a@example.com"),
            },
        )
        .unwrap();
        assert_eq!(
            apply(
                &mut graph,
                Operation::SetNodeProperty {
                    namespace: 1,
                    node: 2,
                    key: 1,
                    value: Value::from("a@example.com"),
                },
            ),
            Err(StepError::UniqueConflict)
        );
        let before = graph.snapshot().unwrap();
        assert_eq!(
            apply(
                &mut graph,
                Operation::CreateIndex {
                    namespace: 1,
                    scope: ModelScope::Label(1),
                    key: 1,
                    unique: false,
                },
            ),
            Ok(())
        );
        assert!(graph.snapshot().unwrap().index_hits.len() > before.index_hits.len());
    }

    #[test]
    fn creating_a_unique_index_over_duplicates_allocates_nothing() {
        let mut graph = ReferenceGraph::new();
        apply(
            &mut graph,
            Operation::InternLabel {
                namespace: 1,
                name: "Person".into(),
            },
        )
        .unwrap();
        apply(
            &mut graph,
            Operation::InternKey {
                namespace: 1,
                name: "email".into(),
            },
        )
        .unwrap();
        apply(&mut graph, Operation::CreateNode { namespace: 1 }).unwrap();
        apply(&mut graph, Operation::CreateNode { namespace: 1 }).unwrap();
        apply(
            &mut graph,
            Operation::AddLabel {
                namespace: 1,
                node: 1,
                label: 1,
            },
        )
        .unwrap();
        apply(
            &mut graph,
            Operation::AddLabel {
                namespace: 1,
                node: 2,
                label: 1,
            },
        )
        .unwrap();
        let value = Value::from("a@example.com");
        apply(
            &mut graph,
            Operation::SetNodeProperty {
                namespace: 1,
                node: 1,
                key: 1,
                value: value.clone(),
            },
        )
        .unwrap();
        apply(
            &mut graph,
            Operation::SetNodeProperty {
                namespace: 1,
                node: 2,
                key: 1,
                value,
            },
        )
        .unwrap();
        assert_eq!(
            apply(
                &mut graph,
                Operation::CreateIndex {
                    namespace: 1,
                    scope: ModelScope::Label(1),
                    key: 1,
                    unique: true,
                },
            ),
            Err(StepError::UniqueConflict)
        );
        assert!(graph.snapshot().unwrap().indexes.is_empty());
        assert_eq!(graph.snapshot().unwrap().counters[&1].next_index, 1);
    }

    #[test]
    fn declaration_changes_follow_the_engine() {
        let mut graph = ReferenceGraph::new();
        apply(
            &mut graph,
            Operation::InternLabel {
                namespace: 1,
                name: "Person".into(),
            },
        )
        .unwrap();
        apply(
            &mut graph,
            Operation::InternKey {
                namespace: 1,
                name: "age".into(),
            },
        )
        .unwrap();
        let required = Operation::Declare {
            namespace: 1,
            scope: ModelScope::Label(1),
            key: 1,
            value_type: ValueType::Int,
            nullable: false,
            required: false,
        };
        apply(&mut graph, required.clone()).unwrap();
        assert_eq!(
            apply(
                &mut graph,
                Operation::Declare {
                    namespace: 1,
                    scope: ModelScope::Label(1),
                    key: 1,
                    value_type: ValueType::String,
                    nullable: false,
                    required: false,
                },
            ),
            Ok(())
        );
        apply(
            &mut graph,
            Operation::Declare {
                namespace: 1,
                scope: ModelScope::Label(1),
                key: 1,
                value_type: ValueType::Int,
                nullable: true,
                required: false,
            },
        )
        .unwrap();
        apply(&mut graph, Operation::CreateNode { namespace: 1 }).unwrap();
        apply(
            &mut graph,
            Operation::AddLabel {
                namespace: 1,
                node: 1,
                label: 1,
            },
        )
        .unwrap();
        assert_eq!(
            apply(
                &mut graph,
                Operation::SetNodeProperty {
                    namespace: 1,
                    node: 1,
                    key: 1,
                    value: Value::from("nope"),
                },
            ),
            Err(StepError::SchemaConflict)
        );
    }

    #[test]
    fn unissued_key_wins_over_nan_and_absent_label_removal_is_ok() {
        let mut graph = ReferenceGraph::new();
        apply(&mut graph, Operation::CreateNode { namespace: 1 }).unwrap();
        assert_eq!(
            apply(
                &mut graph,
                Operation::SetNodeProperty {
                    namespace: 1,
                    node: 1,
                    key: 4,
                    value: Value::Float(f64::NAN),
                },
            ),
            Err(StepError::InvalidIdentity)
        );
        apply(
            &mut graph,
            Operation::InternKey {
                namespace: 1,
                name: "n".into(),
            },
        )
        .unwrap();
        assert_eq!(
            apply(
                &mut graph,
                Operation::SetNodeProperty {
                    namespace: 1,
                    node: 1,
                    key: 1,
                    value: Value::Float(f64::NAN),
                },
            ),
            Err(StepError::InvalidValue)
        );
        apply(
            &mut graph,
            Operation::RemoveLabel {
                namespace: 1,
                node: 1,
                label: 9,
            },
        )
        .unwrap();
        assert_eq!(
            apply(
                &mut graph,
                Operation::RenameKey {
                    namespace: 1,
                    id: 1,
                    name: "\0".into()
                }
            ),
            Err(StepError::InvalidName)
        );
    }
}
