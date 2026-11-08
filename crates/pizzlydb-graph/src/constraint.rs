//! Integrity checks run before a mutation stages any key.
//!
//! Every check reads through [`KeyRead`]. Inside a
//! [`GraphBatch`](crate::GraphBatch) that includes writes staged earlier in
//! the same batch.

use pizzlydb_core::{ID_LEN, LabelId, NamespaceId, NodeId, PropertyKeyId};
use pizzlydb_storage::Vfs;

use crate::access::{KeyRead, PrefixScan};
use crate::batch::GraphTxn;
use crate::directory;
use crate::error::GraphError;
use crate::keys;

/// Both endpoints must be live nodes.
pub(crate) fn ensure_endpoints_live<R: KeyRead>(
    read: &R,
    namespace: NamespaceId,
    source: NodeId,
    target: NodeId,
) -> Result<(), GraphError> {
    for node in [source, target] {
        match directory::load_node(read, namespace, node) {
            Ok(_) => {}
            Err(GraphError::NotFound | GraphError::Deleted) => {
                return Err(GraphError::EndpointNotLive);
            }
            Err(err) => return Err(err),
        }
    }

    Ok(())
}

/// `node` must have no adjacency key in either direction.
pub(crate) fn ensure_no_incident_edges<R: KeyRead>(
    read: &R,
    namespace: NamespaceId,
    node: NodeId,
) -> Result<(), GraphError> {
    let mut scan = PrefixScan::new(read, keys::adjacency_of(namespace, node))?;
    match scan.next().transpose()? {
        Some(_) => Err(GraphError::EndpointInUse),
        None => Ok(()),
    }
}

/// The unique entry at `key` must be free or held by `owner`.
pub(crate) fn ensure_unique<R: KeyRead>(
    read: &R,
    key: &[u8],
    owner: &[u8; ID_LEN],
) -> Result<(), GraphError> {
    match read.get(key)? {
        Some(holder) if holder.as_slice() != owner.as_slice() => Err(GraphError::UniqueConflict),
        _ => Ok(()),
    }
}

/// Check entities touched in this transaction against the schema they read.
pub(crate) fn validate_touched<V: Vfs>(batch: &mut GraphTxn<V>) -> Result<(), GraphError> {
    use std::collections::BTreeSet;

    let touched = batch.touched();
    let mut namespaces = BTreeSet::new();
    for (namespace, owner) in touched {
        namespaces.insert(namespace);
        if !owner_live(batch, namespace, owner)? {
            continue;
        }
        validate_owner(batch, namespace, owner)?;
    }
    for namespace in namespaces {
        let _ = crate::schema::read_version(batch, namespace)?;
    }
    Ok(())
}

fn owner_live<R: KeyRead>(
    read: &R,
    namespace: NamespaceId,
    owner: crate::property::Owner,
) -> Result<bool, GraphError> {
    match directory::ensure_live(read, namespace, owner) {
        Ok(()) => Ok(true),
        Err(GraphError::NotFound | GraphError::Deleted) => Ok(false),
        Err(err) => Err(err),
    }
}

fn validate_owner<R: KeyRead>(
    read: &R,
    namespace: NamespaceId,
    owner: crate::property::Owner,
) -> Result<(), GraphError> {
    let scopes = match owner {
        crate::property::Owner::Node(node) => {
            let labels =
                crate::access::IdScan::<_, LabelId>::new(read, keys::node_labels(namespace, node))?;
            labels
                .map(|label| label.map(crate::property::Scope::Label))
                .collect::<Result<Vec<_>, _>>()?
        }
        crate::property::Owner::Edge(edge) => {
            let record = directory::load_edge(read, namespace, edge)?;
            vec![crate::property::Scope::Type(record.edge_type)]
        }
    };
    for scope in scopes {
        for (key, declaration) in declarations_of(read, namespace, scope)? {
            match crate::property::read_cell(read, namespace, owner, key)? {
                None if declaration.required => {
                    return Err(crate::property::violation(
                        crate::error::ConstraintKind::RequiredMissing,
                        owner,
                        Some(key),
                        "required property is absent",
                    ));
                }
                None => {}
                Some(value) if !declaration.accepts(&value) => {
                    let kind = if value.value_type().is_none() {
                        crate::error::ConstraintKind::NullForbidden
                    } else {
                        crate::error::ConstraintKind::TypeMismatch
                    };
                    return Err(crate::property::violation(
                        kind,
                        owner,
                        Some(key),
                        "value does not match the declaration",
                    ));
                }
                Some(_) => {}
            }
        }
    }
    Ok(())
}

fn declarations_of<R: KeyRead>(
    read: &R,
    namespace: NamespaceId,
    scope: crate::property::Scope,
) -> Result<Vec<(PropertyKeyId, crate::property::PropertyDeclaration)>, GraphError> {
    let prefix = keys::declaration_keys(namespace);
    let mut found = Vec::new();
    for entry in crate::access::PrefixScan::new(read, prefix.clone())? {
        let (key, value) = entry?;
        if key.len() != prefix.len() + 1 + 32 {
            return Err(GraphError::Corrupt);
        }
        if key[prefix.len()] != scope.kind() {
            continue;
        }
        let stored = crate::property::Scope::decode(key[prefix.len()], &key, prefix.len() + 1)?;
        if stored != scope {
            continue;
        }
        let property = id_at(&key, prefix.len() + 1 + 16)?;
        found.push((
            property,
            crate::property::PropertyDeclaration::decode(&value)?,
        ));
    }
    Ok(found)
}

fn id_at<I: pizzlydb_core::Identifier>(bytes: &[u8], offset: usize) -> Result<I, GraphError> {
    crate::access::id_at(bytes, offset)
}
