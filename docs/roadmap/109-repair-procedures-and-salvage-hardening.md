# 109: Repair procedures and salvage hardening

| Field | Value |
| --- | --- |
| Phase | 2 Operational layer |
| Priority | Must |
| Size | L |
| Crates | `pizzlydb-graph` (`salvage`), `pizzlydb-incremental`, `pizzlydb-engine`, `pizzlydb-cli` |
| Type | Feature, operability |

## Summary

Provide documented, tested repair paths for each corruption class that verification reports ([107](107-full-verification-and-online-scrubbing.md)):

- **Derived data** (secondary indexes, label and type membership, the reverse adjacency direction) is rebuilt online from the authoritative records, using the online index-build mechanism.
- **Maintained incremental state** that is corrupt or invalid is rebuilt by reinstalling the plan.
- **Unrecoverable damage** to authoritative records is handled by salvage, which copies everything readable into a new database.

Salvage must be hardened. It must cover all namespaces and the system metadata, use bounded memory and chunked commits, report what it omitted, and give the result a new lineage, so that subscriptions are explicitly invalidated rather than silently resumed against altered data.

## Motivation

Guide §3.4 requires documented repair procedures for secondary indexes. Today's salvage copies a single namespace inside one batch and silently drops every other namespace. Operators facing corruption need a clear decision tree, and tools that are safe to run.

## Current State

- `crates/pizzlydb-graph/src/salvage.rs`: `salvage(vfs, source, destination, namespace)` runs a single `GraphBatch` and uses a constant file id, which [002](002-offline-tool-identity-and-limits.md) fixes.
- No index-rebuild or plan-rebuild command exists.

## Scope

### In scope

- `repair index <id>`: drop the index and rebuild it online ([044](044-online-index-build-and-drop.md)). Similar commands rebuild membership and the reverse direction of adjacency from the authoritative node and edge records, in chunks.
- `repair plan <id>`: reinstall the plan ([065](065-plan-installation-and-lifecycle.md)) and mark its subscribers `PLAN_INVALIDATED`, or keep them if the revision semantics allow it.
- **Salvage v2:**
  - all namespaces and the catalog
  - chunked commits, bounded memory, and progress reporting
  - a report of omissions per namespace
  - a new lineage and incarnation for the destination
  - an option to carry over the plan definitions, which are then re-bootstrapped
  - subscriptions dropped, with clients receiving `RESUME_FORBIDDEN` with the reason `SALVAGED`
- **Runbook:** `docs/operations/corruption-response.md`, a decision tree that maps each verification finding to its repair command, its expected data loss, and its follow-up.

### Out of scope

- Automatic self-healing without operator action, which may come post-1.0 for derived data only.

## Dependencies

- [107](107-full-verification-and-online-scrubbing.md): findings and classification.
- [044](044-online-index-build-and-drop.md): the online build mechanism.
- [065](065-plan-installation-and-lifecycle.md): plan reinstallation.

## Acceptance Criteria

- [ ] For each finding class, a fault test corrupts the database, runs the documented repair, and then `verify_full` passes. Where the class allows no data loss, the data equals the pre-corruption reference.
- [ ] Salvage of a multi-namespace database carries every namespace over, and memory stays within budget on a 10 million entity source.
- [ ] Clients holding tokens for a salvaged database receive a typed error on resume (end-to-end test).

## Testing and Validation

- Fault-injection repair tests and a benchmark of salvage memory use.

## References

- [Guide §3.4](../architecture/implementation-guide.md#34-initial-physical-index-set)
- `crates/pizzlydb-graph/src/salvage.rs`
