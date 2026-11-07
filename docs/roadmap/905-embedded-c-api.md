# 905: Stable embedded API and C ABI

| Field | Value |
| --- | --- |
| Phase | Future (post-GA expansion track) |
| Priority | Could |
| Size | L |
| Crates | `pizzlydb-engine`, new `pizzlydb-capi` |
| Type | Feature |

## Summary

Commit to a stable, semver-governed public Rust embedding API built on `pizzlydb-engine` ([071](071-embedded-engine-composition.md)). Optionally, add a C ABI for embedding pizzlydb in other runtimes. The C ABI would use opaque handles, explicit ownership, and error codes that mirror the protocol.

## Motivation

The embedded composition root exists from Phase 1 for testing and for the server, but it is an *internal* API. Embedding demand, for example in edge or desktop applications, would justify stabilizing it. A C ABI requires `unsafe` code at the FFI boundary. The workspace forbids `unsafe_code`, so the C ABI must live in a separately reviewed crate with a documented exception policy (ADR-001).

## Current State

- [071](071-embedded-engine-composition.md) provides an internal API.

## Scope

### In scope

- **API stabilization review** and `cargo-semver-checks` coverage.
- **C ABI.** An ADR covers the unsafe-code exception, and the ABI ships with cbindgen headers and a fuzzed FFI surface.

### Out of scope

- Language bindings beyond C.

## Dependencies

- [071](071-embedded-engine-composition.md): the engine API.
- [222](222-release-engineering-and-compatibility.md): the compatibility policy.

## Acceptance Criteria

- [ ] The public embedding API is documented and semver-checked.
- [ ] If the C ABI is built, it passes the Miri or sanitizer test suites, and an ADR approves the unsafe-code exception.

## Testing and Validation

- API tests, plus FFI fuzzing and sanitizers.

## References

- [Guide §11](../architecture/implementation-guide.md#11-rust-implementation-structure), [§14 ADR-001](../architecture/implementation-guide.md#14-mandatory-adrrfc-process)
