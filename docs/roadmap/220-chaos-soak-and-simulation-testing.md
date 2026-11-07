# 220: Chaos, soak, and long-running simulation testing

| Field | Value |
| --- | --- |
| Phase | 3 Production capabilities |
| Priority | Must |
| Size | L |
| Crates | `pizzlydb-testkit`, CI |
| Type | Testing |

## Summary

Run the full system, meaning the server plus SDK clients, under continuous fault injection:

- process kills (SIGKILL) at random points
- disk-full and I/O errors (through the fault VFS in test builds, or dm-flakey and dm-error on Linux in a VM)
- network partitions and latency (toxiproxy-style)
- clock jumps
- memory pressure
- backup and restore during load
- configuration reloads

A Jepsen-style checker validates the resulting history: commit linearizability under strict serializability, subscription R1 and R2 per client, and no lost acknowledged commits. A soak of at least 72 hours with mixed workloads is part of the release gate.

## Motivation

Guide §12.1 requires fault-injection testing (disk full, slow fsync, process kill, network disconnects, slow consumers) and long-running tests. The phase-level suites ([072](072-phase-1-exit-soak-and-invariant-audit.md), [119](119-operational-fault-injection-suite.md)) exercise the engine. This suite exercises the *deployed* system, including the network, the SDK resume logic, and the operational procedures together.

## Current State

- Engine-level crash simulation exists after [004](004-crash-consistency-simulation-vfs.md) and [025](025-storage-model-based-and-crash-suite.md). Nothing exists at system level.

## Scope

### In scope

- **Chaos harness.** It orchestrates a server in a container or VM, N SDK clients that record operation histories, and fault schedulers.
- **History checkers.** Elle-style transactional anomaly detection, adapted, or a custom checker over the recorded read and write sets, together with the R1/R2 checker from [067](067-incremental-correctness-suite.md) applied to client logs.
- **Scenarios:** kill during commit, during checkpoint, during backup, and during compaction; disk full during a write burst; a partition during a subscription bootstrap; restore while clients hold tokens (they must receive the typed error described in [108](108-backup-verification-and-restore.md)).
- **Soak:** 72 hours with memory, file-descriptor, and disk-growth tracking. Leaks fail the run.
- **Scheduled CI** runs (nightly short chaos, weekly long soak), with archived artifacts.

### Out of scope

- Multi-node chaos ([901](901-replication-and-high-availability.md)).

## Dependencies

- [212](212-rust-sdk.md): the SDK clients.
- [119](119-operational-fault-injection-suite.md): engine-level operational faults.

## Acceptance Criteria

- [ ] The checker detects injected anomalies in a deliberately broken build (mutation check).
- [ ] 1,000 consecutive chaos iterations pass with zero checker violations and zero lost acknowledged commits.
- [ ] A 72-hour soak passes with memory, file descriptors, and disk usage bounded within the configured budgets.
- [ ] Results are archived for each release candidate.

## Testing and Validation

- The suite itself.

## References

- [Guide §12.1](../architecture/implementation-guide.md#121-testing-layers)
- [Jepsen Elle](https://github.com/jepsen-io/elle)
