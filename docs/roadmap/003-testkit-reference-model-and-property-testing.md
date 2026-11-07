# 003: Testkit reference graph model, workload generator, and property-testing harness

| Field | Value |
| --- | --- |
| Phase | 1.0 Foundations |
| Priority | Must |
| Size | L |
| Crates | `pizzlydb-testkit` (new public test API), dev-dependencies across the workspace |
| Type | Test infrastructure |

## Summary

Build `pizzlydb-testkit` into the shared oracle for every correctness suite. It needs three parts:

- An obviously correct in-memory reference graph model.
- A deterministic, seeded workload and history generator.
- Property-testing utilities with failure minimization and replayable seeds.

## Motivation

Guide §12.1 calls for model-based, property-based, differential, and history-checking tests. All later correctness gates compare the real engine against a reference: storage ([025](025-storage-model-based-and-crash-suite.md)), graph ([047](047-graph-integrity-suite.md)), query ([053](053-reference-query-evaluator.md)), and incremental ([067](067-incremental-correctness-suite.md)). Right now each crate tests itself with hand-picked examples, there is no shared generator, and the workspace has no property-testing dependency.

## Current State

- `crates/pizzlydb-testkit/src/reference.rs` contains only a doc comment, and the crate has no tests and no public API.
- The workspace has no `proptest`, `quickcheck`, or `arbitrary` dependency.
- The root `tests/{model-based,compatibility,crash-recovery,benchmarks}` directories are empty.
- The graph integration tests (`crates/pizzlydb-graph/tests/*.rs`) use fixed examples.

## Scope

### In scope

- A `ReferenceGraph` built on `BTreeMap`s. It models namespaces, nodes, edges, labels, edge types, properties, declarations, and index definitions with the semantics that the guide §3 and the graph crate document today. It must contain no performance tricks, so that it is easy to audit.
- `Operation` and `History` types with seeded generation (`proptest` strategies plus a deterministic `rand_chacha` stream). Generation can be biased toward: parallel edges, self-loops, cycles, hub nodes, delete and recreate, label churn, unique-index collisions, and null values.
- A history runner that applies the same operation stream to the engine (through a trait adapter) and to the model, compares observable state after each step, and reports the first divergence with a minimal reproduction.
- Seed capture: a failing run prints a `PIZZLYDB_SEED=...` line that reproduces the failure exactly.
- A testing guide in `docs/` that explains how crates plug into the harness.

### Out of scope

- Query semantics ([053](053-reference-query-evaluator.md)) and the isolation checker ([036](036-isolation-verification-suite.md)). Both build on this crate.

## Dependencies

- None. This is foundation work that can run in parallel with [001](001-design-record-and-baseline-adrs.md).

## Acceptance Criteria

- [ ] `pizzlydb-testkit` exposes `ReferenceGraph`, `Operation`, `History`, generators, and a `run_history` driver, each with rustdoc and a doctest.
- [ ] The current graph crate passes a property test of at least 1,000 generated histories against the model, run in CI with a fixed seed budget.
- [ ] A deliberately injected bug, such as skipping one adjacency key on edge delete, is caught and minimized to a history of at most 10 operations.
- [ ] A failing seed reproduces the identical failure.
- [ ] The testkit never depends on storage internals; it uses only public crate APIs.

## Testing and Validation

- The testkit has self-tests: the generators respect the configured distributions, and the model handles every operation it generates.
- A mutation test (the injected bug above) checks that the harness actually detects divergence.

## References

- [Guide §12.1 testing layers](../architecture/implementation-guide.md#121-testing-layers), [§12.2 mandatory correctness properties](../architecture/implementation-guide.md#122-mandatory-correctness-properties)
- `crates/pizzlydb-testkit/src/reference.rs`
- [`proptest`](https://docs.rs/proptest)
