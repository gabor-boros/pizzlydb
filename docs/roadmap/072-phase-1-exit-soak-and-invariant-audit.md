# 072: Phase 1 exit gate: engine soak test and invariant audit

| Field | Value |
| --- | --- |
| Phase | 1E Reactive core |
| Priority | Must (milestone gate M4) |
| Size | M |
| Crates | `tests/`, every engine crate |
| Type | Verification |

## Summary

Before any Phase 2 or Phase 3 work depends on the engine as a whole, run a long, mixed, in-process soak. The workload combines:

- concurrent transactions and queries
- subscriptions, with slow and fast consumers
- plan installation and removal
- schema changes and online index builds
- namespace drops
- checkpoints, and crashes with recovery

Then audit every invariant in guide §12.2 that applies at engine level against its evidence. This gate defines Milestone M4, "Reactive core".

## Motivation

The user-mandated ordering requires the core engine to be sufficiently complete and validated before work on higher layers proceeds. Individual suites prove individual properties, while the soak test catches resource leaks and rare interactions that only appear over hours. The audit makes sure that no invariant is merely assumed.

## Current State

- Not started. The prerequisites are the whole of Phase 1.

## Scope

### In scope

- A 24-hour nightly soak, plus a 72-hour run before the milestone is signed off. Invariant checkers run online: incremental results match the oracle at sampled frontiers, and verification runs at intervals. Resources are tracked throughout: RSS, file sizes, WAL size, retained bytes, and threads.
- An invariant audit table in `docs/milestones/M4.md`. For each of C1–C9, S1–S3, R1, R2, and A1, it links the test that demonstrates the invariant, or explains why the invariant belongs to a later phase (for example C8 and A1 need authorization, in [218](218-security-test-suite.md)).
- Performance baselines for the full engine published with the [005](005-benchmark-harness-and-baselines.md) harness, covering commit, query, and maintenance lag.
- A triage of known issues: each one is fixed, or filed with a severity and an explicit waiver.

### Out of scope

- Network, authorization, and operational tooling (Phases 2 and 3).

## Dependencies

- [071](071-embedded-engine-composition.md): the composed engine.
- [047](047-graph-integrity-suite.md) and [025](025-storage-model-based-and-crash-suite.md): the earlier gates.

## Acceptance Criteria

- [ ] The 72-hour soak completes with zero invariant violations. Resource usage reaches a steady state, defined as the slope of RSS and disk usage staying within noise after warm-up.
- [ ] The audit table is complete, every engine-level invariant has linked evidence, and it has been reviewed by a second engineer.
- [ ] No open Must-severity bugs remain in Phase 1 crates.

## Testing and Validation

- This issue is the test and the audit.

## References

- [Guide §12.2](../architecture/implementation-guide.md#122-mandatory-correctness-properties), [§13 M4/M5](../architecture/implementation-guide.md#13-implementation-roadmap-dependency-ordered), [§15](../architecture/implementation-guide.md#15-definition-of-done-for-pizzlydb-core)
