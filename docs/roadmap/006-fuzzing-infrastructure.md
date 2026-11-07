# 006: Fuzzing infrastructure for every on-disk and wire decoder

| Field | Value |
| --- | --- |
| Phase | 1.0 Foundations |
| Priority | Must |
| Size | M |
| Crates | new `fuzz/` (cargo-fuzz), `pizzlydb-storage`, `pizzlydb-core`, `pizzlydb-graph` |
| Type | Test infrastructure, security |

## Summary

Set up coverage-guided fuzzing for every decoder that reads untrusted or possibly corrupted bytes:

- page header, superblock, WAL header and frame
- B-tree node and overflow chain
- allocator, bitmap, and retire-spill pages
- `Value` and key encodings
- graph record, declaration, and index-definition decoders

Later targets will cover the query parser ([051](051-query-parser-and-ast.md)), the protocol messages ([201](201-native-protocol-definition.md)), and resume tokens ([070](070-resume-tokens.md)). A decoder must return a typed error for any input, never panic or hang.

## Motivation

The engine verifies checksums, but a valid checksum does not prove the structure is valid: a bug, a hostile file, or a bad restore can produce a checksummed page with nonsense fields. Release builds use `panic = "abort"`, so any panic reachable from a decoder takes down the whole process. Guide §12.1 lists fuzzing as a required test layer, and no fuzz targets exist today.

## Current State

- There is no `fuzz/` directory and no `arbitrary` dependency.
- Decoders exist in these places, each with unit tests but without fuzzing:
  - `crates/pizzlydb-storage/src/page.rs`, `superblock.rs`, `wal.rs` (`decode_frame_header`), `btree.rs` (node decode), and `space.rs` (`decode_header`, `decode_spill`, `decode_bitmap`)
  - `crates/pizzlydb-core/src/value.rs`
  - `crates/pizzlydb-graph/src/{record,property,index}.rs`

## Scope

### In scope

- A `cargo-fuzz` workspace with one target per decoder. Each target asserts "no panic" and, where an encoder exists, the round trip `decode(encode(x)) == x`.
- Structure-aware targets: an engine-level target that opens a database built from fuzzed page bytes and runs `verify_quick` and the snapshot reads, asserting a clean `Corrupt` error or success.
- Corpus seeding from the existing test fixtures.
- A CI smoke run of 60 seconds per target on each PR (nightly toolchain, isolated job), and a nightly long run that uploads crash artifacts.
- A documented triage process: each crash becomes a regression unit test.

### Out of scope

- OSS-Fuzz onboarding, which is optional and can follow later.

## Dependencies

- None. This is foundation work.

## Acceptance Criteria

- [x] Every decoder listed above has a fuzz target in CI.
- [ ] Each target runs for one hour locally without a panic, or every finding is fixed and has a regression test.
- [x] Engine-level fuzzing proves that `Storage::open` on arbitrary bytes returns `Ok` or a typed `StorageError` and never panics.
- [x] `CONTRIBUTING` documents how to add a target and how to reproduce a crash.

## Testing and Validation

- An injected `unwrap()` in a decoder is found by the smoke run (a meta-test of the setup).
- The nightly job's coverage report is kept as an artifact.

## References

- [Guide §12.1](../architecture/implementation-guide.md#121-testing-layers)
- [`cargo-fuzz`](https://rust-fuzz.github.io/book/cargo-fuzz.html)
