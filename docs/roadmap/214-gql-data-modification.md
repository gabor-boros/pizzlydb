# 214: Query-language data modification (INSERT, SET, REMOVE, DELETE, DETACH DELETE)

| Field | Value |
| --- | --- |
| Phase | 3 Production capabilities |
| Priority | Should |
| Size | L |
| Crates | `pizzlydb-query`, `pizzlydb-graph` |
| Type | Feature |

## Summary

Add data-modification statements to the query language. They run inside the caller's transaction, using the same optimistic validation, schema constraints, and mutation log as the programmatic API. Statement semantics are explicit: reads see the transaction's own writes, the order in which pattern matches are applied is defined, and there are limits on how many entities a statement may affect.

## Motivation

Guide §5.2 scopes the core as snapshot reads, and the programmatic graph API ([035](035-graph-on-transactions.md)) already covers mutations. Users of GQL or Cypher expect DML, and it reduces round-trips for pattern-based updates. This is a usability improvement rather than a correctness prerequisite, so its priority is Should.

## Current State

- The language specification ([050](050-query-language-specification.md)) defines reads only, and [042](042-detach-delete.md) provides the detach-delete primitive.

## Scope

### In scope

- **Specification extension.** Statement grammar, semantics (match then apply, with conflicts within a statement defined), and the limits.
- **Implementation:** binder, IR, and executor operators that call the transaction write API.
- **Differential tests** against the reference evaluator ([053](053-reference-query-evaluator.md)) extended with mutations.
- **Mutation-log integration.** Effects appear in the log exactly as they would through the programmatic API.

### Out of scope

- `MERGE` or upsert semantics, which need a separate RFC.

## Dependencies

- [059](059-query-correctness-suite.md): the query correctness baseline.
- [042](042-detach-delete.md): detach delete.
- [203](203-transaction-and-query-service.md): the service surface.

## Acceptance Criteria

- [ ] The specification is merged, with edge-case examples.
- [ ] Differential tests show that DML results and post-states match the reference evaluator.
- [ ] Statement limits are enforced with typed errors.
- [ ] DML through queries produces the same mutation log and the same subscription deltas as the equivalent API calls (test).

## Testing and Validation

- Differential, property, and integration tests.

## References

- [Guide §5.2](../architecture/implementation-guide.md#52-supported-core-snapshot-operations)
