# 045: Namespace registry and lifecycle

| Field | Value |
| --- | --- |
| Phase | 1C Graph model completion |
| Priority | Must |
| Size | M |
| Crates | `pizzlydb-graph`, `pizzlydb-txn`, `pizzlydb-core` |
| Type | Feature |

## Summary

Introduce a durable registry of graph namespaces (the guide's tenant-scoped logical graphs). Every namespace has metadata: name, creation `CommitSeq`, state, logical format version, and limits. Operations against an unknown or dropped namespace fail. A namespace drop runs as a chunked background deletion. Tools that work on "the database" (verify, salvage, backup, export) iterate over all registered namespaces.

## Motivation

`GraphStore::new(namespace)` accepts any non-zero id without checking that it exists. There is no list of namespaces and no way to drop one, so it is impossible to know which data exists, to verify or salvage all of it (salvage currently copies one namespace and silently drops the rest), or to attach per-tenant limits and authorization. Multi-tenancy (guide §9) and quotas ([208](208-multi-tenancy-and-quotas.md)) build on this registry.

## Current State

- `crates/pizzlydb-graph/src/record_store.rs`: `GraphStore::new(namespace)` performs no registry check.
- `crates/pizzlydb-core/src/id.rs`: `NamespaceId` is 128 bits, with zero invalid. The system key space ([031](031-commit-sequence-and-commit-metadata.md)) reuses the zero prefix.
- `salvage`, `bulk_load`, and `verify_full` all take a single `namespace` parameter.

## Scope

### In scope

- Registry keys in the system key space: `sys/ns/<id>`, holding the metadata, and `sys/ns_by_name/<name>`, enforcing unique names.
- `create_namespace(name, options)`, `list_namespaces`, `get_namespace`, and `rename_namespace`. Each change is a transactional mutation with a log record.
- **Drop.** `drop_namespace(id)` marks the namespace `Dropping` and rejects new operations immediately. A background task removes its key range in chunks (`delete_range` from [018](018-storage-range-operations.md)) and finally removes the registry entry. Ids are never reused.
- `GraphStore::open(txn, namespace)` validates the namespace's existence and state, and this check is part of the read set.
- **Metadata.** Each namespace records the graph logical format version from [024](024-storage-format-versioning-and-golden-fixtures.md). Limits are placeholders until quotas are implemented.
- **Tools.** Verification and the export paths enumerate the registry.

### Out of scope

- Authorization ([207](207-authorization-rbac-and-policy-epochs.md)) and quotas ([208](208-multi-tenancy-and-quotas.md)).

## Dependencies

- [035](035-graph-on-transactions.md): graph on transactions.
- [018](018-storage-range-operations.md): range delete.

## Acceptance Criteria

- [ ] An operation on a namespace that does not exist, or is being dropped, fails with a typed `NamespaceNotFound` or `NamespaceDropping` error (test).
- [ ] Dropping a namespace with 1 million entities completes in bounded chunks. A crash in the middle resumes the drop. Other namespaces are untouched, which a verification of them proves.
- [ ] A concurrent write into a namespace being dropped either conflicts or fails, and never resurrects data.
- [ ] `verify_full` over all namespaces is available ([107](107-full-verification-and-online-scrubbing.md) extends it further).

## Testing and Validation

- Unit tests, crash simulation of the drop, and concurrency tests.

## References

- [Guide §3.1](../architecture/implementation-guide.md#31-entities), [§9 security and tenancy](../architecture/implementation-guide.md#9-security-and-tenancy)
- `crates/pizzlydb-graph/src/record_store.rs`, `crates/pizzlydb-core/src/id.rs`
