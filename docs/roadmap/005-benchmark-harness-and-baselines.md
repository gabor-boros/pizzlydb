# 005: Benchmark harness and first Rust baseline measurements

| Field | Value |
| --- | --- |
| Phase | 1.0 Foundations |
| Priority | Must |
| Size | M |
| Crates | new `benches/` targets, `pizzlydb-testkit` (datasets) |
| Type | Performance infrastructure |

## Summary

Create a reproducible benchmark harness and publish the first baseline for the Rust engine. The baseline covers storage point reads and writes, scans, commit latency by durability mode, checkpoint cost, WAL bytes per logical write, graph CRUD, adjacency expansion on hub nodes, and index lookups. Report results in physical units (pages read and written, bytes fsynced) as well as wall time.

## Motivation

The guide (§12.3) requires measurement, not performance mythology. The performance findings below come from reading the code and have not been measured:

- Page lookup is linear in frames and in WAL pages.
- Each B-tree node visit copies 8 KiB.
- B-tree splits rewrite the parent pointers of moved children.
- Every commit clones the whole free-space state.

Before [017](017-concurrent-page-table-and-zero-copy-reads.md), [020](020-group-commit-and-durability-modes.md), and [023](023-btree-write-amplification.md) change anything, a baseline must exist so that each change is justified by data and regressions are visible.

## Current State

- There is no `benches/` directory, no `criterion` or `divan` dependency, and the root `tests/benchmarks/` directory is empty.
- The `Pager` exposes `PagerStats` (hits, misses, evictions), which a harness can read.
- `rust-toolchain.toml` sets `channel = "stable"`, so the compiler changes whenever Rust releases. Guide WP-01 and §11 require a pinned toolchain, and baselines taken with different compilers are not comparable.

## Scope

### In scope

- Benchmark targets that use `criterion` (or `divan`) for micro benchmarks plus a custom macro harness for long runs.
- Seeded dataset generators in testkit: uniform, power-law degree (hubs), and a social-network shape.
- Reported metrics:
  - throughput and p50/p95/p99 latency
  - pages read and written per operation
  - WAL bytes per logical mutation
  - fsync count
  - peak RSS
  - buffer-pool hit ratio
- A machine-readable JSON results format, plus a script that compares two runs.
- A CI job that runs a small, fast suite on each PR and reports results without gating. Gating comes in [219](219-benchmark-suite-and-slos.md).
- A baseline report committed under `docs/benchmarks/` with hardware, OS, file system, compiler version, and the commit SHA.
- Pin `rust-toolchain.toml` to an exact stable release, with Renovate proposing upgrades, so that CI and baselines are reproducible.

### Out of scope

- Server end-to-end benchmarks and SLOs ([219](219-benchmark-suite-and-slos.md)).

## Dependencies

- [003](003-testkit-reference-model-and-property-testing.md): the dataset and workload generators.

## Acceptance Criteria

- [ ] `cargo bench -p pizzlydb-storage` and `cargo bench -p pizzlydb-graph` run the documented suites.
- [ ] The baseline report includes WAL bytes per inserted key, the cost of a B-tree split in pages written, and point-read latency as a function of WAL size (which shows the linear-scan effect).
- [ ] Two runs on the same machine with the same seed differ by less than a documented tolerance, for example 5% median.
- [ ] Benchmark code uses only public APIs or explicit `bench` features, never private internals.
- [ ] `rust-toolchain.toml` names an exact version (for example `1.xx.y`), and the baseline report records it.

## Testing and Validation

- Each benchmark uses `debug_assert` or result checks to confirm that it actually did the work.
- Peer review of the methodology: warm and cold cache are separated, and the fsync device is documented.

## References

- [Guide §12.3 performance measurement](../architecture/implementation-guide.md#123-performance-measurement-not-performance-mythology)
- `crates/pizzlydb-storage/src/pager.rs` (`PagerStats`)
