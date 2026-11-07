# 010: Storage error taxonomy fidelity: stop reporting I/O failures as corruption

| Field | Value |
| --- | --- |
| Phase | 1A Storage engine hardening |
| Priority | Must |
| Size | S |
| Crates | `pizzlydb-storage`, `pizzlydb-graph` |
| Type | Correctness, operability |

## Summary

Make `StorageError` report what actually happened. Today a disk read failure during a B-tree traversal surfaces as `Corrupt`, and buffer-pool exhaustion surfaces as `Io`. Introduce an explicit resource-exhaustion variant, preserve the error class across the pager and tree boundaries, and attach diagnostic context (page id, file role, operation) without leaking file contents.

## Motivation

Operators react differently to each error class. `Corrupt` triggers restore-from-backup procedures. `Io` suggests retrying or checking the device. Resource exhaustion means the configuration is wrong. Mislabeling causes wrong incident responses and breaks the retryability information that the error model of guide §8.3 needs (`RESOURCE_EXHAUSTED` and `UNAVAILABLE` versus a data-integrity failure). Later layers (transactions, server) map these variants to canonical codes, so they must be correct at the source.

## Current State

- In `crates/pizzlydb-storage/src/engine.rs`, `SnapshotPages::pin` (around lines 1221–1234) maps every `pager.pin` error to `TreeError::Corrupt`, including `PagerError::Io` and `PoolExhausted`.
- `map_pager` (around lines 1554–1565) maps `PagerError::PoolExhausted` to `StorageError::Io`.
- `StorageError` has the variants `Io`, `Corrupt`, `Busy`, `Full`, `UnsupportedVersion`, and `Closed`. None of them carries a page id or an operation.
- `GraphError` wraps `StorageError` (`crates/pizzlydb-graph/src/error.rs`), so the mislabeling propagates upward.

## Scope

### In scope

- Add `StorageError::ResourceExhausted { resource }`, where `resource` names the buffer pool, the transaction size, and so on. The enum is `#[non_exhaustive]`, so this is an API addition.
- Carry the error class through `TreeError`, either by adding `TreeError::Io` and `TreeError::Exhausted` or by having the tree accept an error type from `PageRead`.
- Add optional diagnostic context (page id, role, operation) through a `source()` chain or structured fields. The `Display` output must not include user data.
- Audit every `map_*` helper and every `.map_err(|_| ...)` in storage and graph for class-losing conversions.
- Document the error classes and their retryability in the rustdoc of `StorageError`.

### Out of scope

- Mapping to protocol codes ([201](201-native-protocol-definition.md)) and the unified engine error ([071](071-embedded-engine-composition.md)).

## Dependencies

- None. Every later storage issue relies on this taxonomy.

## Acceptance Criteria

- [ ] A test with `FaultInjecting` making a read fail during `ReadView::get` gets `StorageError::Io`, not `Corrupt`.
- [ ] A test with a one-frame pool and a two-level tree gets `ResourceExhausted`, not `Io`.
- [ ] A true checksum mismatch still yields `Corrupt` and includes the page id in its context.
- [ ] `rg 'map_err\(\|_\|'` over storage and graph finds no class-losing conversions, or each remaining one has a written justification.

## Testing and Validation

- Parameterized unit tests over a fault matrix (read error, short read, checksum mismatch, pool exhaustion) for each read and write entry point.
- The existing crash and backup suites keep passing.

## References

- [Guide §8.3 error taxonomy](../architecture/implementation-guide.md#83-error-taxonomy)
- `crates/pizzlydb-storage/src/engine.rs` (`SnapshotPages::pin`, `map_pager`, `map_tree`)
- `crates/pizzlydb-storage/src/pager.rs` (`PagerError`)
