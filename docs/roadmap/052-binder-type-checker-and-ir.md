# 052: Binder, type checker, authorization hook, and the versioned typed IR

| Field | Value |
| --- | --- |
| Phase | 1D Query engine |
| Priority | Must (critical path) |
| Size | L |
| Crates | `pizzlydb-query` (`ir`, new `bind` module) |
| Type | Feature |

## Summary

Turn the AST into a language-independent, typed intermediate representation (IR). The binder resolves labels, types, and property keys against a catalog snapshot keyed by `schema_version`, infers the type of every expression and parameter, and applies normalization rewrites such as predicate pushdown candidates and constant folding. It then calls an authorization hook before any planning happens.

The IR is shared by snapshot execution and by incremental maintenance, and it has a versioned, stable serialization because maintained plans persist it.

## Motivation

The guide's pipeline is parse, bind, authorize, normalize to the typed IR, then estimate cost and plan (§5.1). Because the same IR feeds both snapshot plans and incremental plans, a subscription and a one-off query with the same text have identical semantics. Persisted plans ([064](064-maintained-state-and-output-log.md)) need an IR format that is versioned and decodes deterministically.

## Current State

- `crates/pizzlydb-query/src/ir.rs` is a one-line stub.
- The catalog lookup APIs exist (`crates/pizzlydb-graph/src/catalog.rs`). The schema version comes from [043](043-schema-and-catalog-lifecycle.md).

## Scope

### In scope

- **IR types.** Pattern graph (variables, label and type constraints, direction, path segments with a mode), filter expressions, projections, aggregates, ordering, limits, and parameters with their types.
- **Name resolution.** Unknown names are not always errors: a label that has never been interned matches nothing, which is a valid query with an empty result, whereas an unknown *function* is an error. The RFC decides each case.
- **Typing.** Type inference and checking follow the type-system RFC ([040](040-type-system-and-value-model.md)), and parameters must be bound with compatible types at execution time.
- **Authorization hook.** `Authorizer::authorize(&BoundQuery, &Principal) -> Result<(), AuthzError>`. It defaults to allow, and the real implementation arrives in [207](207-authorization-rbac-and-policy-epochs.md).
- **Normalization.** Constant folding, conjunctive normal form for filters, and identification of predicates that can be served by indexes.
- **Serialization.** A versioned format and a fingerprint that is stable across processes and depends on the canonical IR and the parameter types.

### Out of scope

- Cost-based planning ([056](056-planner-cost-model-and-explain.md)) and incremental compilation ([062](062-incremental-admissibility-and-compiler.md)).

## Dependencies

- [051](051-query-parser-and-ast.md): the parser.
- [043](043-schema-and-catalog-lifecycle.md): the schema version and the catalog.
- [040](040-type-system-and-value-model.md): the types.

## Acceptance Criteria

- [ ] Every semantic case in the conformance corpus binds, or fails with the expected code.
- [ ] Fingerprint stability: the same query and schema produce the same fingerprint across process restarts and across platforms (test with golden fingerprints).
- [ ] Round-trip property test of IR serialization, and a fuzz target for the IR decoder.
- [ ] The authorization hook is called exactly once per query, before planning (test with a mock authorizer).

## Testing and Validation

- Corpus tests, property tests, and fuzzing.

## References

- [Guide §5.1](../architecture/implementation-guide.md#51-language-and-compiler), [§16.1 interface boundaries](../architecture/implementation-guide.md#161-minimal-internal-interface-boundaries)
- `crates/pizzlydb-query/src/ir.rs`
