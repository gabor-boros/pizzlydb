# 219: Benchmark suite, published results, and service-level objectives

| Field | Value |
| --- | --- |
| Phase | 3 Production capabilities |
| Priority | Must |
| Size | L |
| Crates | `pizzlydb-testkit` (`bench` workloads), CI |
| Type | Performance |

## Summary

Extend the harness from [005](005-benchmark-harness-and-baselines.md) into an end-to-end suite that runs through the SDK and server. It produces published, reproducible results and defines SLOs that release gating enforces. Workloads cover:

- point reads and writes
- traversals of 1, 2, and 3 hops, plus bounded paths
- commit throughput under contention
- subscription fan-out (thousands of subscriptions over hot keys)
- maintenance lag under write bursts
- bootstrap time for large result sets
- recovery time after a crash
- backup and restore throughput
- bulk-load rate

## Motivation

Guide §12.3 requires benchmarks for:

- storage and transaction cost: commit latency, WAL growth, write amplification, and recovery time
- query cost: point and traversal latency, plan quality, and memory
- incremental cost: maintenance lag, delta amplification, arrangement memory, and bootstrap time
- delivery cost: fan-out, ack latency, and resume cost

Guide §12.3 also requires a published workload, hardware, data set, and percentile reporting, with no unqualified throughput numbers. SLOs are what operators plan capacity against.

## Current State

- [005](005-benchmark-harness-and-baselines.md) introduces component-level benchmarks and baselines. There are no end-to-end workloads or SLOs.

## Scope

### In scope

- **Workload definitions** as code. Data generators are seeded and their scale is configurable, for example social graph and supply-chain shapes, and LDBC SNB Interactive-inspired queries where they fit.
- **Driver.** Open-loop load generation to avoid coordinated omission, HDR histograms, and reporting of p50, p99, p99.9, and max.
- **Reference hardware profiles.** Documented, with a dedicated CI runner or scheduled cloud runs.
- **SLO document:** for example commit p99 and maintenance-lag p99 at a stated load on the reference hardware, plus recovery-time and backup-throughput targets.
- **Regression gate.** The nightly run compares against the stored baseline, and a release is blocked on an SLO breach without a waiver.
- **Published results** in `docs/performance/`.

### Out of scope

- Competitive benchmarking claims.

## Dependencies

- [005](005-benchmark-harness-and-baselines.md): the harness.
- [212](212-rust-sdk.md): the SDK driver.
- [072](072-phase-1-exit-soak-and-invariant-audit.md): a stable engine.

## Acceptance Criteria

- [ ] Every guide §12.3 metric family is measured and reported, and the measurement method is documented.
- [ ] The driver is open-loop, with coordinated-omission correction verified by a test.
- [ ] The SLO document is approved, and the nightly gate is active.
- [ ] Results are reproducible within a stated variance across three runs.

## Testing and Validation

- The suite itself, with variance analysis.

## References

- [Guide §12.3](../architecture/implementation-guide.md#123-performance-measurement-not-performance-mythology)
- [LDBC SNB](https://ldbcouncil.org/benchmarks/snb/)
- [HdrHistogram](https://docs.rs/hdrhistogram)
