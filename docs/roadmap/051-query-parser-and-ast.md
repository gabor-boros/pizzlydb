# 051: Query parser and AST

| Field | Value |
| --- | --- |
| Phase | 1D Query engine |
| Priority | Must |
| Size | M |
| Crates | `pizzlydb-query` (`parser`) |
| Type | Feature |

## Summary

Implement a parser from query text to a span-annotated AST for the grammar in [050](050-query-language-specification.md), with precise, user-friendly error messages. The parser enforces limits on input size and nesting depth, never panics, and is fuzzed.

## Motivation

The parser is the first untrusted-input boundary of the query system. It has to be total: every input produces either an AST or a typed error. Its error messages determine how usable the system is, and its depth limits prevent stack exhaustion.

## Current State

- `crates/pizzlydb-query/src/parser.rs` is a one-line stub. There is no lexer, no grammar, and no tests.

## Scope

### In scope

- A hand-written lexer and recursive-descent parser, using Pratt parsing for expressions. A hand-written parser is recommended over a generator because it gives better error recovery and messages and adds no dependencies. A combinator library such as `winnow` or `chumsky` is acceptable if the reasons are recorded.
- AST types with byte spans. The AST is independent of the IR.
- `QueryError::Syntax { span, expected, found, hint }`, including a caret rendering for the CLI ([213](213-cli.md)).
- Limits: maximum input bytes, maximum expression depth, and maximum pattern elements. Exceeding a limit returns a typed error.
- A fuzz target ([006](006-fuzzing-infrastructure.md)) that asserts no panic, plus a round trip of pretty-print followed by parse on generated ASTs.

### Out of scope

- Name resolution and typing ([052](052-binder-type-checker-and-ir.md)).

## Dependencies

- [050](050-query-language-specification.md): the grammar.
- [006](006-fuzzing-infrastructure.md): fuzzing infrastructure.

## Acceptance Criteria

- [ ] Every syntactic case in the conformance corpus parses, or fails with the expected code.
- [ ] Property test: `parse(pretty(ast)) == ast` for generated ASTs.
- [ ] A one-hour fuzz run finds no panic, stack overflow, or hang.
- [ ] Parsing a 64 KiB query takes less than 5 ms (benchmark).

## Testing and Validation

- Corpus tests, property tests, fuzzing, and snapshot tests of error messages (for example with `insta`).

## References

- [Guide §5.1](../architecture/implementation-guide.md#51-language-and-compiler)
- `crates/pizzlydb-query/src/parser.rs`
