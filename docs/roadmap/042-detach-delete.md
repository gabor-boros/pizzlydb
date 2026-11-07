# 042: Detach delete with bounded cascades

| Field | Value |
| --- | --- |
| Phase | 1C Graph model completion |
| Priority | Must |
| Size | M |
| Crates | `pizzlydb-graph` (`record_store`, `adjacency`, `constraint`) |
| Type | Feature |

## Summary

Add `detach_delete_node(id, limits)`. It atomically removes a node together with all its incident edges, in both directions and including self-loops. The edges' property cells, membership keys, adjacency keys, and index entries are removed as well, and so are the node's labels and properties. It is subject to a configured maximum number of edges per operation and returns a typed error when the limit would be exceeded.

## Motivation

Guide §3.2 says that deleting a node with incident edges fails by default, and that an explicit detach delete removes the affected edges atomically, subject to resource limits. Only the failing default exists today, so clients must enumerate and delete the edges themselves. Doing that across several requests races with concurrent edge creation unless it happens inside one serializable transaction.

## Current State

- `crates/pizzlydb-graph/src/record_store.rs`: `delete_node` (around lines 207–219) calls `ensure_no_incident_edges` and returns `GraphError::EndpointInUse` when edges exist.
- `crates/pizzlydb-graph/src/adjacency.rs`: `neighbors` and `neighbors_of_type` can enumerate incident edges.

## Scope

### In scope

- `GraphStore::detach_delete_node(&self, txn, id, DetachLimits { max_edges })`. It enumerates the outgoing and incoming adjacency of the node (self-loops are counted once), deletes each edge through the normal edge-delete path, and emits a `GraphMutation` for each edge plus one for the node, in a deterministic order.
- The adjacency range scans that the enumeration performs are in the read set, so an edge created concurrently to this node causes a conflict instead of a dangling edge.
- Error: `GraphError::TooManyIncidentEdges { limit, found_at_least }`, mapped to `RESOURCE_EXHAUSTED` or `QUERY_TOO_EXPENSIVE` in the protocol.
- Documentation of the limits and the recommended chunked pattern for hubs: delete edges in batches, then delete the node.

### Out of scope

- The GQL `DETACH DELETE` statement ([214](214-gql-data-modification.md)).

## Dependencies

- [035](035-graph-on-transactions.md): graph on transactions.

## Acceptance Criteria

- [ ] A node with outgoing, incoming, parallel, and self-loop edges is fully removed, and `verify_full` passes afterwards.
- [ ] A concurrent edge creation to the node being deleted results in a conflict or in a consistent order. Dangling edges are never possible (concurrency test plus the isolation workload in [036](036-isolation-verification-suite.md)).
- [ ] Exceeding `max_edges` changes nothing and returns the typed error.
- [ ] The mutation log contains exactly one delete record per removed edge, plus one for the node.

## Testing and Validation

- Unit tests, a model-based extension, and an isolation workload.

## References

- [Guide §3.2](../architecture/implementation-guide.md#32-integrity)
- `crates/pizzlydb-graph/src/{record_store,adjacency,constraint}.rs`
