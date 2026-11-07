# Property harness

`pizzlydb-testkit` is the shared oracle. It depends only on `pizzlydb-core`. An engine adapter lives in the crate under test and calls that crate's public API.

## Types

- `ReferenceGraph` stores namespaces, nodes, edges, labels, types, properties, declarations, and indexes in `BTreeMap`s.
- `Operation` is one graph command. `History` is an ordered list plus the seed that produced it.
- `generate_history(seed, len, bias)` is a deterministic `ChaCha8Rng` stream. `history_strategy` is the proptest strategy.
- `Subject` is the adapter trait: `reset`, `apply`, and `snapshot`.
- `run_history` applies each step to the subject and to a fresh model, then compares snapshots and error classes.
- `minimize` drops operations until the failure no longer reproduces.

`WorkloadBias` raises the rate of parallel edges, self-loops, cycles, hubs, delete/recreate, label churn, unique-index collisions, and nulls.

## Replay

A divergence prints `PIZZLYDB_SEED=<seed>`. The same seed, `PROPERTY_STEPS`, and `WorkloadBias::default()` rebuild that history:

```shell
PIZZLYDB_SEED=11 cargo test -p pizzlydb-graph --test model -- replay_seed_from_the_environment
```

`PIZZLYDB_PROPTEST_CASES` sets the history count. CI uses the default, 1000.

```shell
cargo test -p pizzlydb-graph --test model -- --test-threads=1
```

The graph adapter opens `MemoryVfs` with `Durability::None` and commits one `GraphBatch` per operation.

## Crash simulator

`CrashSimVfs` is exported by `pizzlydb-storage` (the virtual workspace cannot pass a per-crate feature to `cargo test --workspace`). `for_each_crash` replays a workload, then crashes after every mutating VFS operation for `PIZZLYDB_CRASH_SAMPLES` persistence draws (default 4). The nightly workflow raises that budget.

`tests/engine_crash.rs` stays on `MemoryVfs`. That model is all-or-nothing. `CrashSimVfs` is a superset, and the known `Wal::prune` bug makes a full enumeration fail until [012](../roadmap/012-crash-safe-wal-reset.md).
