# 901: Replication and high availability (RFC-gated)

| Field | Value |
| --- | --- |
| Phase | Future (post-GA expansion track) |
| Priority | Could |
| Size | XL |
| Crates | new `pizzlydb-replication`, `pizzlydb-txn`, `pizzlydb-server` |
| Type | RFC, feature |

## Summary

Write an RFC and then implement a replicated deployment. It covers:

- **Topology:** a leader with synchronous or quorum followers.
- **Replication of the logical mutation log:** a Raft-style consensus log, or log shipping with fencing.
- **Leader election** and **fencing epochs**, which extend the incarnation epoch from [108](108-backup-verification-and-restore.md).
- **Failover-safe subscription cursors**, follower snapshot installation, follower reads at a bounded `CommitSeq` lag, and rolling upgrades.

## Motivation

Guide §10.5 states that single-node correctness is mandatory and that replication is an optional subsequent target. It requires an RFC that specifies leader election, log replication, fencing epochs, failover cursors, snapshot installation, and read consistency. Single-node v1 relies on backup and restore ([105](105-online-consistent-backup.md)–[108](108-backup-verification-and-restore.md)) and, optionally, point-in-time restore ([118](118-log-archiving-and-point-in-time-restore.md)) for recovery. Its recovery time is therefore limited by restore time, which may be unacceptable for some users.

## Current State

- Not designed. Phase 1 deliberately keeps `CommitSeq` and the mutation log ([031](031-commit-sequence-and-commit-metadata.md), [032](032-logical-mutation-log.md)) replication-friendly: dense, totally ordered, and self-describing.

## Scope

### In scope

- **The RFC** with every required section from guide §14, including a Jepsen-style test plan and a precise statement of read consistency for followers and subscriptions after failover.
- **Implementation phases** following the RFC.

### Out of scope

- Sharding ([907](907-sharding-and-distributed-execution.md)).

## Dependencies

- [223](223-external-review-and-ga-gate.md): single-node GA comes first, as the guide requires.

## Acceptance Criteria

- [ ] The RFC is accepted with quantified workload evidence.
- [ ] Chaos testing with partitions, leader kills, and clock skew shows no lost acknowledged commits and no R1/R2 violations across failover.
- [ ] Rolling upgrade is tested across adjacent versions.

## Testing and Validation

- Multi-node chaos with a linearizability checker.

## References

- [Guide §10.5](../architecture/implementation-guide.md#105-high-availability-and-distribution), [§14](../architecture/implementation-guide.md#14-mandatory-adrrfc-process)
