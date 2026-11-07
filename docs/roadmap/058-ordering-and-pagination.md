# 058: Snapshot-consistent ordering and pagination

| Field | Value |
| --- | --- |
| Phase | 1D Query engine |
| Priority | Must |
| Size | M |
| Crates | `pizzlydb-query`, `pizzlydb-txn` (snapshot leases) |
| Type | Feature |

## Summary

Define and implement pagination with clear snapshot semantics. Two modes are needed:

1. **Snapshot pagination.** A continuation token binds the query fingerprint, the parameters, the snapshot lease ([037](037-snapshot-retention-and-transaction-limits.md)), and the executor's resume position. Every page reads the *same* snapshot, so the pages together are exactly the result of a single execution.
2. **Keyset pagination.** For long-lived browsing, the query is re-executed at the latest snapshot with the bounds `ORDER BY key > last_key`. Results are consistent within each page, and the semantics across pages are documented.

## Motivation

Guide §5.2 requires "ordered results and pagination with clearly defined snapshot semantics". Pagination with `OFFSET` over a moving dataset produces duplicates and gaps. Clients and SDKs ([212](212-rust-sdk.md)) need a cursor contract that is exactly specified.

## Current State

- Not implemented. Snapshot leases come from [037](037-snapshot-retention-and-transaction-limits.md).

## Scope

### In scope

- Continuation token format: versioned, opaque, and integrity-protected, using the same keyring mechanism as resume tokens ([070](070-resume-tokens.md)), or server-side cursor state referenced by id. The ADR chooses one.
- Resumable executor positions for scan-based plans. Plans that cannot resume, such as a hash aggregate with a final sort, are either materialized within the memory budget or are not paginated.
- Deterministic total order: an `ORDER BY` that is not unique gets implicit tie-breaking by entity id, and this is documented.
- Lease expiry yields `CURSOR_EXPIRED` or `SNAPSHOT_TOO_OLD`, and the client must restart.
- Page size limits.

### Out of scope

- Protocol transport for pages ([203](203-transaction-and-query-service.md)).

## Dependencies

- [055](055-physical-operators-and-executor.md): resumable operators.
- [037](037-snapshot-retention-and-transaction-limits.md): leases.

## Acceptance Criteria

- [ ] In snapshot mode, paging through a result while concurrent writes happen yields exactly the single-execution result at that snapshot (property test).
- [ ] Keyset mode produces no duplicates and skips nothing that existed for the whole paging session (property test with a precise definition).
- [ ] A tampered or expired token is rejected with a typed error.

## Testing and Validation

- Property tests with concurrent writers, and token fuzzing.

## References

- [Guide §5.2](../architecture/implementation-guide.md#52-supported-core-snapshot-operations)
