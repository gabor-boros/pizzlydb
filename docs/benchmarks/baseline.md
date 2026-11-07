# Storage baseline

Measured on the working tree based on `77892c6aee0d3e5b6485b9ecd850357ff82f6dc5`, before that tree was committed. Profile: `cargo test -p pizzlydb-storage --test metrics --release`.

| Item | Value |
| --- | --- |
| Machine | Apple M1 Pro, 16 GiB |
| OS | macOS 27.2, Darwin arm64 |
| Filesystem | APFS |
| Compiler | rustc 1.98.1 (48a229cea 2026-09-01) |
| Toolchain file | `channel = "1.98.1"` |
| Backend | `MemoryVfs` |
| Seed | 1 |
| Workload | 32 keys, `Durability::Full`, then one checkpoint, then 128 more full commits |

`MemoryVfs` sync copies bytes in the process. It is not a device flush. macOS RSS is the current resident size from `ps -o rss=`, so `rss_is_peak` is false. Linux reports `VmHWM` and sets `rss_is_peak`.

## Physical counters

These matched on two calls inside one process and on a second process.

| Metric | Value |
| --- | --- |
| WAL bytes after 32 inserts | 1,054,400 |
| WAL bytes per inserted key | 32,950 |
| fsync calls for that prefix | 166 |
| Pages written by the following checkpoint | 5 |
| WAL file length after that checkpoint | 1,054,400 |
| WAL file length after 128 further commits | 4,217,408 |
| Pager `read_io` during the inserts | 0 |
| Buffer-pool hit ratio during the inserts | 0.791 |
| RSS | 21,643,264 bytes |

Checkpoint does not shrink the WAL file. Prune leaves the previous length in place. The larger file is the 128 commits after that checkpoint.

Pager `read_io` stayed 0 because the point reads were served from memory.

## Point read and WAL length

Warm medians from 32 reads in the first release process:

| WAL file length | Median point read |
| --- | --- |
| 1,054,400 bytes (after checkpoint) | 15,333 ns |
| 4,217,408 bytes (after 128 more commits) | 21,709 ns |

The same process reported insert latency p50 91,375 ns, p95 117,083 ns, p99 205,417 ns (32 commits).

A second release process on the same machine kept every physical counter and moved the insert median to 54,666 ns (about 40% below the first process). `compare-bench` with tolerance 0.05 exits 1 on that pair. The 5% median tolerance is the check for a quiet machine and a longer sample. It is not met by these short runs. [219](../roadmap/219-benchmark-suite-and-slos.md) is the gating suite.

## Commands

```shell
PIZZLYDB_RUSTC="$(rustc --version)" PIZZLYDB_BASELINE_OUT=target/baseline.json \
  cargo test -p pizzlydb-storage --test metrics --release -- --exact physical_counters_match_across_two_runs

cargo run -p pizzlydb-testkit --bin compare-bench -- left.json right.json 0.05
cargo bench -p pizzlydb-storage --bench storage
cargo bench -p pizzlydb-graph --bench graph
```

`PIZZLYDB_BENCH_QUICK=1` shortens Criterion. `PIZZLYDB_BENCH_OUT` writes a `BenchReport` from the storage bench. Criterion timings are warm. The metrics test separates the post-checkpoint reads from the reads after the WAL file has grown.
