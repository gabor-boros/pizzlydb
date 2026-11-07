# 060: ADR-007: choosing the incremental execution engine, with prototype evidence

| Field | Value |
| --- | --- |
| Phase | 1E Reactive core |
| Priority | Must (decision gate) |
| Size | L |
| Crates | `docs/adr/`, prototype in `pizzlydb-incremental` (throwaway branch or `examples/`) |
| Type | Architecture decision, prototype |

## Summary

Decide, based on measured evidence, how incremental view maintenance is implemented. There are two options:

- **(A)** An in-house operator library with explicit signed-delta semantics and persisted arrangements.
- **(B)** An embedded dataflow engine such as Differential Dataflow or DBSP.

Prototype both options on two queries: a one-edge pattern with a property filter, and the two-hop "shared device" pattern from guide §5.5. Measure correctness, per-mutation cost, memory use, state persistence, and the complexity of crash recovery.

The ADR also decides two questions that shape the mutation log and the snapshot design:

1. Whether operators read base data from their own arrangements or through as-of graph reads ([061](061-as-of-snapshots-at-commit-sequence.md)).
2. Whether the mutation log's before-images ([032](032-logical-mutation-log.md)) are required.

## Motivation

Guide §11.3 requires this ADR, backed by benchmark evidence. The choice determines the shape of state persistence ([064](064-maintained-state-and-output-log.md)), the replay semantics, memory accounting, and long-term maintainability. Two constraints shape it. The guide requires the checkpointed state and the output deltas to advance atomically together with `processed_seq`, which is difficult to achieve with an external dataflow runtime that owns its own state. Operator probes must also observe the graph *as of* the commit being processed, not the latest state.

## Current State

- `crates/pizzlydb-incremental/src/{delta,arrangement,checkpoint}.rs` are one-line stubs.
- The crate depends on `core`, `query`, and `txn`.

## Scope

### In scope

- Prototype A: an in-house filter, an expand-join with arrangements kept in storage, and distinct.
- Prototype B: the same queries on an embedded engine, plus an assessment of state export and import for checkpoints, determinism, and dependency weight. It also checks license compatibility with the project's MIT license under `cargo-deny`.
- Measured results for each prototype:
  - correctness against the reference oracle ([053](053-reference-query-evaluator.md))
  - per-mutation latency and throughput
  - memory per maintained result row
  - time to build the initial state on 1 million edges
  - recovery time after a crash
  - lines of code and the number of `unsafe` lines in dependencies
- A decision on base-data access (arrangements versus as-of reads), with the consequences for retention.

### Out of scope

- Production implementation (062–067).

## Dependencies

- [052](052-binder-type-checker-and-ir.md): the IR.
- [032](032-logical-mutation-log.md): the mutation log.
- [053](053-reference-query-evaluator.md): the oracle.

## Acceptance Criteria

- [ ] ADR-007 is accepted. It includes the benchmark tables, the reproduction commands, a recommendation, and its rejected alternatives with reasons.
- [ ] Both prototypes pass the oracle on 10,000 random commit histories, or each failure is explained and attributed.
- [ ] The decision on arrangements versus as-of reads and on before-images is recorded, and [032](032-logical-mutation-log.md) and [061](061-as-of-snapshots-at-commit-sequence.md) are updated if needed.

## Testing and Validation

- Differential tests of the prototypes, and benchmark runs on named hardware.

## References

- [Guide §6](../architecture/implementation-guide.md#6-incremental-query-maintenance), [§11.3 external engine evaluation](../architecture/implementation-guide.md#113-external-engine-evaluation)
- McSherry et al., "Differential Dataflow" (CIDR 2013). Budiu et al., "DBSP: Automatic Incremental View Maintenance" (VLDB 2023).
