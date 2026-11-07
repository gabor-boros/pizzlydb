# Contributing

Day-to-day checks are in the [root README](README.md). This file covers decoder fuzzing.

The `fuzz/` directory is its own Cargo workspace so libFuzzer can unwind. The root workspace sets `panic = "abort"` in release, which would hide crashes. Root `cargo test --workspace` does not build `fuzz/`.

## Run a target

Install a nightly toolchain and Clang, then run one target for 60 seconds. `fuzz/.cargo` does not pass sanitizer-coverage flags: those flags are also applied to build scripts and fail to link. libFuzzer still executes the decoder. A run that prints `Is the code instrumented for coverage?` is still a panic check; it is not coverage-guided.

```shell
cd fuzz
RUSTUP_TOOLCHAIN=nightly cargo run --release --bin page_header -- corpus/page_header -max_total_time=60
```

Targets: `page_header`, `superblock`, `wal_header`, `wal_frame`, `btree_node`, `overflow`, `allocator`, `bitmap`, `retire_spill`, `value`, `key`, `graph_record`, `declaration`, `index_definition`, `engine`.

`panic_canary` is not a decoder. It panics on input byte `0xA5`. The CI smoke job expects that process to exit non-zero. Do not add it to the pass list.

`engine` writes the input into a `MemoryVfs` database and calls `Storage::open`, `verify_quick`, and a short snapshot read. Those calls return `Ok` or a typed `StorageError`.

## Add a target

1. Add a function under `pizzlydb-storage` or `pizzlydb-graph` feature `fuzzing`, or call a public `pizzlydb-core` decoder directly. The function must return a typed error for rejected input and must not panic.
2. Where an encoder exists, check `decode(encode(x)) == x` on values the decoder accepted.
3. Add `fuzz/fuzz_targets/<name>.rs` with `fuzz_target!` and a `[[bin]]` entry in `fuzz/Cargo.toml`.
4. Seed `fuzz/corpus/<name>/` with a small valid header (`PZLY` for pages, `PZLW` for the WAL).
5. Add `<name>` to the smoke list in `.github/workflows/ci.yml` and to the long run in `.github/workflows/nightly.yml`.

## Reproduce a crash

libFuzzer writes the input under `fuzz/artifacts/<target>/` when `-artifact_prefix` is set. Re-run that file as the only corpus entry:

```shell
cd fuzz
RUSTUP_TOOLCHAIN=nightly cargo run --release --bin page_header -- artifacts/page_header -runs=1
```

A confirmed crash becomes a unit test next to the decoder, with the input bytes in the test. Fix the decoder, keep the test, and delete the artifact. Do not commit files under `fuzz/artifacts/` other than `.gitkeep`.
