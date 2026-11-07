# 041: Schema constraints at commit: required properties, type checks, and declaration semantics

| Field | Value |
| --- | --- |
| Phase | 1C Graph model completion |
| Priority | Must |
| Size | L |
| Crates | `pizzlydb-graph` (`property`, `constraint`, `catalog`), `pizzlydb-txn` |
| Type | Feature, semantics |

## Summary

Complete the constraint model of guide §3.2 and §3.3. Required (mandatory) properties are enforced at commit. The rule for which declaration applies when a node has several labels is replaced with an unambiguous one. Label changes re-validate the node against the constraints of its new labels. Every constraint is checked against the schema version that is authoritative at commit time, and failures return a typed `ConstraintViolation` with machine-readable details.

## Motivation

- **Missing required properties.** A declaration currently carries only `value_type` and `nullable`, so required properties cannot be expressed, although the guide lists them.
- **Order-dependent declaration rule.** The applicable declaration for a node is the one on its *lowest-id label*. Adding an older label can therefore change which declaration governs existing values, without any validation.
- **Mid-transaction checks.** Constraints are checked when each operation is staged. Under optimistic transactions ([033](033-optimistic-transaction-manager.md)) the authoritative check has to hold at commit. The intermediate states inside one transaction may legitimately violate constraints, for example by creating a node and then setting its required property.

## Current State

- `crates/pizzlydb-graph/src/property.rs`:
  - the module docs (around lines 7–16) describe the lowest-id-label rule and the separate declared and undeclared cells
  - `PropertyDeclaration { value_type, nullable }` (around lines 179–184)
  - "adding or loosening a declaration does not rewrite existing cells"
- `crates/pizzlydb-graph/src/constraint.rs`: `ensure_no_incident_edges` and related checks, all applied immediately.
- `GraphError` has no structured constraint-violation variant that carries details.

## Scope

### In scope

- **Declaration model.** A declaration carries `required: bool`, the value type, and nullability.
- **Multi-label rule.** A node must satisfy the declarations of *all* its labels, and conflicting declarations for the same key across labels are rejected when they are declared. Alternatively the ADR may justify a different deterministic rule, and the change must be documented.
- **Commit-time validation.** The transaction tracks touched entities, and before commit validates each one against the schema version that the transaction read. That read is part of the read set, so a concurrent schema change causes a conflict.
- **Tightening.** Tightening a declaration (adding `required`, making it non-nullable, changing its type) validates existing data. It runs either online or chunked through the same mechanism as an index build ([044](044-online-index-build-and-drop.md)), or it fails if violations exist. Choose one and document it.
- **Errors.** `GraphError::ConstraintViolation { kind, entity, key, detail }` maps to the guide's `CONSTRAINT_VIOLATION` code.
- **Cell layout.** Revisit whether the dual declared and undeclared cells are still needed; a simplification would be a graph format change.

### Out of scope

- Uniqueness across multiple properties (composite constraints, [904](904-composite-and-specialized-indexes.md)).

## Dependencies

- [035](035-graph-on-transactions.md): graph on transactions.
- [040](040-type-system-and-value-model.md): the type system that declarations use.

## Acceptance Criteria

- [ ] A transaction that creates a node with a required property missing fails at commit with `ConstraintViolation(RequiredMissing)`. Creating the node and then setting the property in the same transaction succeeds.
- [ ] Adding a label whose declarations the node violates fails at commit.
- [ ] A concurrent schema tightening and a violating write: exactly one of them succeeds (concurrency test).
- [ ] The documented declaration-resolution rule is order-independent (property test over random label insertion orders).

## Testing and Validation

- Table-driven constraint tests and model-based tests that extend the testkit reference model ([003](003-testkit-reference-model-and-property-testing.md)).

## References

- [Guide §3.2](../architecture/implementation-guide.md#32-integrity), [§3.3](../architecture/implementation-guide.md#33-schema-model), [§8.3](../architecture/implementation-guide.md#83-error-taxonomy)
- `crates/pizzlydb-graph/src/{property,constraint,catalog}.rs`
