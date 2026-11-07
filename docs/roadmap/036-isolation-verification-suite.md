# 036: Isolation verification suite with a history checker

| Field | Value |
| --- | --- |
| Phase | 1B Transactions and logical commit log |
| Priority | Must |
| Size | L |
| Crates | `pizzlydb-testkit` (history checker), `tests/model-based/` |
| Type | Verification |

## Summary

Build an Elle-style history checker. It records concurrent transaction histories, infers write-write, write-read, and read-write dependencies, and searches for cycles that indicate isolation anomalies. Run it against the key-value transaction layer and against the graph layer, including graph-specific anomalies.

## Motivation

Serializability is the hardest claim the system makes and the easiest one to break with an optimization. Guide §12.1 asks for an isolation-anomaly test layer, and Milestone M2 requires that layer as evidence. Unit tests of known anomalies are necessary, but they only find bugs someone already thought of; randomized history checking also finds bugs nobody anticipated.

## Current State

- No history checker exists, and no concurrency tests exist above storage.

## Scope

### In scope

- **History recording.** Each transaction records its operations with the values it observed, plus its commit or abort outcome and its `CommitSeq`.
- **Workloads.**
  - List-append and register workloads at the key level, which allow precise dependency inference.
  - Graph workloads:
    - a unique-constraint race
    - a label-scan phantom (count members, then insert a member)
    - an edge to a concurrently deleted node
    - a detach-delete versus a concurrent edge create, once [042](042-detach-delete.md) lands
    - read skew across both adjacency directions
- **Cycle detection.** The checker detects G0, G1a, G1b, G1c, G-single, G2-item, and G2 including predicate (phantom) cycles, and checks real-time order for strict serializability.
- **Meta-tests.** A debug flag disables validation, and the checker must then find anomalies.
- **Scale.** Configurable thread counts and fault injection: crashes mid-run, after which surviving commits are checked.

### Out of scope

- Network-level Jepsen testing ([220](220-chaos-soak-and-simulation-testing.md)).

## Dependencies

- [035](035-graph-on-transactions.md): graph on transactions.
- [003](003-testkit-reference-model-and-property-testing.md): the testkit harness.

## Acceptance Criteria

- [ ] On each PR, 100,000 transactions across 16 threads find zero anomalies. The nightly job runs 10,000,000.
- [ ] With validation disabled, the checker reports at least G2-item and a phantom anomaly within the PR budget (meta-test).
- [ ] Every anomaly class named in ADR-004 has a dedicated workload.
- [ ] Each failure output includes a minimal cycle explanation and a reproduction seed.
- [ ] The suite is the named evidence for invariant C2 (every history meets the advertised isolation level) and for the isolation half of S3 (each successful transaction has one linearized commit position).

## Testing and Validation

- The checker has its own unit tests on hand-built histories, with known answers for each anomaly class.

## References

- Kingsbury and Alvaro, "Elle: Inferring Isolation Anomalies from Experimental Observations" (VLDB 2020)
- Adya, "Weak Consistency" (1999)
- [Guide §12.1](../architecture/implementation-guide.md#121-testing-layers)
