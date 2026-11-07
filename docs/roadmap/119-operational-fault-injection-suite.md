# 119: Operational fault-injection and recovery suite (exit gate for Phase 2)

| Field | Value |
| --- | --- |
| Phase | 2 Operational layer |
| Priority | Must (milestone gate M5) |
| Size | L |
| Crates | `tests/crash-recovery/`, CI infrastructure |
| Type | Verification |

## Summary

Validate the operational layer on real Linux file systems (ext4 and xfs) under real faults:

- `kill -9` loops during mixed workloads
- simulated power loss, using LazyFS (or dm-log-writes) to drop unsynced data, with dm-flakey for injected I/O faults
- disk-full on size-limited mounts
- fsync failures, using dm-error or FUSE

The suite also runs backup and restore drills (including cross-architecture), upgrades from every fixture, compaction and shrinking under load, and retention caps with adversarial consumers. A 72-hour soak must reach steady-state resource use. This gate defines Milestone M5, "Operable engine".

## Motivation

The crash simulator ([004](004-crash-consistency-simulation-vfs.md)) proves the protocols correct against a model of the file system. Real file systems, kernels, and devices differ from any model, and guide §12.1 requires forced process kills, WAL and sync errors, corruption, disk-full, and restart and checkpoint recovery to be tested. Operational procedures also have to work end to end, not only as unit-tested functions.

## Current State

- Every test runs on `MemoryVfs` or `PosixVfs` in temporary directories on the CI runner's file system. There are no device-level fault tests.

## Scope

### In scope

- **CI infrastructure.** A nightly job on Linux VMs with privileges for device-mapper or FUSE, plus scripts with documented env vars and example invocations.
- **Scenarios.** Each one asserts the invariants after recovery: no acknowledged commit lost, `verify_full` passes, and subscriptions resume without gaps.
  - kill -9 at random intervals, run 1,000 times
  - power loss through dropped unsynced data, run 500 times
  - EIO on read and on write
  - fsync EIO, after which the engine must enter `Failed` and must not reopen automatically
  - ENOSPC
- **Operations drills.** Backup then restore, a cross-architecture restore, PITR if [118](118-log-archiving-and-point-in-time-restore.md) is done, fixture upgrades, compaction during load, and retention caps.
- **72-hour soak** with resource-slope analysis.
- **Milestone report** `docs/milestones/M5.md`.

### Out of scope

- Network chaos and client-side tests ([220](220-chaos-soak-and-simulation-testing.md)).

## Dependencies

- Phase 1 exit gate: [072](072-phase-1-exit-soak-and-invariant-audit.md). The operational suite assumes the engine invariants that 072 audits, so M5 cannot close before M4.
- Lifecycle and governance: [104](104-engine-lifecycle-and-health.md), [117](117-resource-governance-and-memory-budgets.md).
- Backup and restore: [106](106-backup-encryption-compression-destinations.md), [108](108-backup-verification-and-restore.md).
- Repair, load, export, and upgrade: [109](109-repair-procedures-and-salvage-hardening.md), [110](110-bounded-memory-bulk-loader.md), [111](111-logical-export-import.md), [112](112-format-upgrade-and-migration.md).
- Maintenance and inspection: [113](113-space-reclamation-and-compaction.md), [114](114-retention-management-and-inspection.md), [115](115-background-maintenance-scheduler.md), [116](116-diagnostics-and-introspection.md).

## Acceptance Criteria

- [ ] All scenarios pass for 14 consecutive nights.
- [ ] The 72-hour soak shows a steady state and every invariant holding.
- [ ] Every runbook (backup and restore, corruption response, disk-full, upgrade) is executed verbatim by a drill script and works.
- [ ] The M5 report is published.

## Testing and Validation

- This issue is the test.

## References

- [Guide §12.1](../architecture/implementation-guide.md#121-testing-layers), [§10.2](../architecture/implementation-guide.md#102-backups-and-restore), [§13 M7](../architecture/implementation-guide.md#m7--operational-hardening)
- [LazyFS](https://github.com/dsrhaslab/lazyfs), [dm-flakey](https://docs.kernel.org/admin-guide/device-mapper/dm-flakey.html)
