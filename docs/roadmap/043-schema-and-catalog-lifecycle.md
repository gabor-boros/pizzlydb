# 043: Schema and catalog lifecycle: drop, retire, schema versions, and change events

| Field | Value |
| --- | --- |
| Phase | 1C Graph model completion |
| Priority | Must (critical path) |
| Size | L |
| Crates | `pizzlydb-graph` (`catalog`, `property`, `index`), `pizzlydb-core` (`mutation`) |
| Type | Feature |

## Summary

Make the schema fully manageable and versioned. The issue adds:

- removing a property declaration (undeclare)
- dropping an index
- retiring labels, edge types, and property keys that are no longer referenced, or a documented reason why names are never retired
- a transactional per-namespace *schema version* that increases on every schema change
- schema-change records in the mutation log, which plan revision and subscription invalidation use ([065](065-plan-installation-and-lifecycle.md))
- a snapshot-consistent catalog cache

## Motivation

Guide §3.3 requires schema metadata to be versioned transactional state, and it requires changes that affect query typing to invalidate subscriptions through a plan-revision protocol. Today declarations and indexes can only be created, catalog names can only be interned and renamed, and there is no schema version. The query binder ([052](052-binder-type-checker-and-ir.md)) and the plan cache ([056](056-planner-cost-model-and-explain.md)) need a schema version to key on.

## Current State

- `crates/pizzlydb-graph/src/catalog.rs` has `intern`, `lookup`, `name`, `rename`, `add_label`, `remove_label`, `set_labels`, `has_label`, and `labels`. There is no delete or retire.
- `crates/pizzlydb-graph/src/property.rs` has `declare`, `declaration`, and the property operations, but no undeclare.
- `crates/pizzlydb-graph/src/index.rs` has `create_index`, `index`, `find_index`, the lookups, and the membership scans, but no `drop_index`.

## Scope

### In scope

- `undeclare(scope, key)`. Existing cells are migrated or remain valid as undeclared cells, depending on the cell layout decided in [041](041-schema-constraints-at-commit.md).
- `drop_index(index_id)`. It marks the index as dropping, stops maintaining it, and removes its entries in chunks with `delete_range` ([018](018-storage-range-operations.md)). It must be crash-safe, and the space is reclaimed later.
- A catalog retirement policy. Either names are never reused, so the "rename only" policy is documented, or unreferenced names can be retired and their ids are never reused.
- `schema_version(namespace) -> u64`, bumped in the same transaction as any declaration, index, constraint, or name change.
- `GraphMutation::SchemaChanged { version, change }` records.
- A catalog cache keyed by `(namespace, schema_version)` that is safe under concurrent snapshots.

### Out of scope

- Online index *build* ([044](044-online-index-build-and-drop.md)).

## Dependencies

- [035](035-graph-on-transactions.md): graph on transactions.
- [032](032-logical-mutation-log.md): schema-change records in the log.

## Acceptance Criteria

- [ ] Every schema-affecting operation increments `schema_version` exactly once per transaction (test).
- [ ] After `drop_index`, lookups through the index fail with `NotFound`, `verify_full` passes, and the entries are eventually gone. A crash during the chunked removal resumes correctly.
- [ ] A reader that holds an older snapshot keeps seeing the old schema consistently (snapshot test).
- [ ] The log contains a schema-change record for each change, with the version.

## Testing and Validation

- Unit tests, crash simulation of chunked drops, and a model-based extension.

## References

- [Guide §3.3](../architecture/implementation-guide.md#33-schema-model)
- `crates/pizzlydb-graph/src/{catalog,property,index}.rs`
