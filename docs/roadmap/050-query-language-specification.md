# 050: Query-language specification RFC (ADR-006)

| Field | Value |
| --- | --- |
| Phase | 1D Query engine |
| Priority | Must (decision gate) |
| Size | L |
| Crates | `docs/rfcs/`, `tests/` (executable grammar and conformance tests) |
| Type | RFC |

## Summary

Specify pizzlydb's documented, GQL-aligned query subset precisely enough to implement and test. The RFC covers:

- the grammar
- the name, type, and value semantics, building on the type-system RFC in [040](040-type-system-and-value-model.md)
- three-valued logic
- the multiset result semantics
- path semantics (`walk`, `trail`, `simple`) and how they are written in a query
- the `SUBSCRIBE` extension and the subset admissible for incremental maintenance
- parameters
- ordering and pagination semantics
- the limits
- the machine-readable error catalog, including `QUERY_UNSUPPORTED` reasons

The RFC also defines the format of a conformance test corpus that is executed in CI.

## Motivation

Guide §5.1 requires the grammar, path rules, and return types to be frozen by a language RFC and by executable grammar tests, and it forbids claiming full ISO GQL conformance. The parser ([051](051-query-parser-and-ast.md)), binder ([052](052-binder-type-checker-and-ir.md)), reference evaluator ([053](053-reference-query-evaluator.md)), and incremental admissibility checker ([062](062-incremental-admissibility-and-compiler.md)) all need one normative source.

## Current State

- `crates/pizzlydb-query/src/{parser,ir,planner,executor}.rs` contain one-line stubs.
- The guide's §5.5 examples are labelled illustrative. No query-language RFC exists.

## Scope

### In scope

- **Statements.** `MATCH` with one or more comma-separated patterns, `WHERE`, `RETURN` with expressions, aliases, and `DISTINCT`, `ORDER BY` (ascending and descending, with null ordering), `SKIP` and `LIMIT`, and `COUNT` and `SUM` with implicit grouping. The RFC also decides whether `OPTIONAL MATCH` is supported in v1. The recommendation is not to support it, and to state that publicly.
- **Patterns.** Node and edge patterns with label and type predicates, inline property maps, direction (out, in, and either), and bounded variable-length segments `*m..n` with an explicit path mode.
- **Expressions.** Literals for every type in [040](040-type-system-and-value-model.md), parameters `$name`, comparison, arithmetic, boolean operators with three-valued logic, `IS NULL`, property access, `id()`, `labels()`, and `type()`.
- **`SUBSCRIBE`.** The syntax, which constructs it admits, and how it relates to snapshot semantics (initial snapshot results followed by deltas).
- **Result model.** Multisets, column names and types, and how values and nulls are encoded in results.
- **Limits.** Query text size, nesting depth, pattern size, maximum path expansion, and output rows.
- **Corpus format.** Each test lists the input graph, the query, the parameters, and the expected result or error code. The tests live under `tests/query-conformance/`.
- **Deferred list.** What is out of v1: `OPTIONAL MATCH`, `UNION`, subqueries, unbounded recursion, and data-modification statements ([214](214-gql-data-modification.md)). Each item has a rationale.

### Out of scope

- Implementation (051 onward).

## Dependencies

- [040](040-type-system-and-value-model.md): the type system.
- [001](001-design-record-and-baseline-adrs.md): the RFC home.

## Acceptance Criteria

- [ ] The RFC is accepted, with an EBNF grammar, a semantics section for every construct, and an error catalog listing a stable code and reason for each unsupported construct.
- [ ] The conformance corpus has at least 150 cases covering every grammar production and every error code. Parallel edges, self-loops, cycles, nulls, and mixed numerics are all represented.
- [ ] The path-mode semantics are stated with worked examples that show different results for `walk`, `trail`, and `simple` on the same graph.

## Testing and Validation

- A review by someone familiar with GQL or Cypher. The corpus is validated later by [053](053-reference-query-evaluator.md).

## References

- [Guide §5.1–§5.5](../architecture/implementation-guide.md#5-query-system)
- ISO/IEC 39075:2024 (GQL), [openCypher TCK](https://github.com/opencypher/openCypher/tree/main/tck)
