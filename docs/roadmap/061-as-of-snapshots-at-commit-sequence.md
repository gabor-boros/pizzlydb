# 061: As-of snapshots at a retained `CommitSeq`

| Field | Value |
| --- | --- |
| Phase | 1E Reactive core |
| Priority | Must |
| Size | L |
| Crates | `pizzlydb-txn` (`snapshot`), `pizzlydb-storage` (pager, WAL, checkpoint) |
| Type | Feature |

## Summary

Open a consistent read snapshot at any committed `CommitSeq` S that is at or above a *protected floor*, rather than only at the latest commit. The engine maps S to its storage LSN through the commit record ([031](031-commit-sequence-and-commit-metadata.md)). The checkpointer and the page reclaimer never discard the page versions needed for any protected floor. Floors are registered by consumers: plan installation, subscription bootstrap, backup, and as-of-read probes if ADR-007 chooses them.

## Motivation

The guide requires plan installation to be atomic with registering a protected log frontier, and an initial graph scan followed by tailing must not lose an intervening commit (§6.4). That means "take a snapshot at S, then tail the log from S+1" must be expressible. The incremental engine may also need to read the graph at the commit currently being processed, depending on [060](060-adr-incremental-execution-engine.md). Today a snapshot can only be registered at the latest committed LSN, and the WAL versions that a past snapshot would need are discarded at checkpoint unless that snapshot was already registered.

## Current State

- `crates/pizzlydb-storage/src/pager.rs`: `snapshot()` registers `committed_lsn`, and `pin` requires a registered LSN.
- The checkpointer copies the versions visible at the oldest registered snapshot and then prunes them ([012](012-crash-safe-wal-reset.md), [021](021-automatic-checkpointing-and-wal-bounds.md)).

## Scope

### In scope

- `TxnManager::snapshot_at(seq)`, which returns `SnapshotTooOld` if `seq` is below the oldest protected floor and otherwise a consistent view at that sequence.
- **Floors.** `ProtectedFloor` registrations, durable or in-memory, carrying an owner and an expiry. The storage horizon is the minimum of the active snapshots and the protected floors. The checkpointer and reclaimer honor that horizon, and its cost (retained WAL and pages) is reported ([114](114-retention-management-and-inspection.md)).
- **Restart.** Durable floors survive restart: recovery keeps the WAL frames above the minimum durable floor. The cost model is documented. If floors would keep the WAL too large, the alternative is to retain only the logical log and rebuild state, as decided in [060](060-adr-incremental-execution-engine.md).
- **Mapping lookup.** The mapping from `CommitSeq` to LSN is an O(log n) lookup through the commit-record keys.

### Out of scope

- Time-travel queries for users beyond protected floors ([906](906-time-travel-queries.md)).

## Dependencies

- Commit order and log: [031](031-commit-sequence-and-commit-metadata.md), [032](032-logical-mutation-log.md).
- WAL and retention: [013](013-disk-resident-wal-index.md), [021](021-automatic-checkpointing-and-wal-bounds.md), [037](037-snapshot-retention-and-transaction-limits.md).

## Acceptance Criteria

- [ ] Property test: for random histories with protected floors, a snapshot at any S at or above the floor equals the reference state after commit S, including across checkpoints and crash/reopen.
- [ ] A snapshot below the floor fails with `SnapshotTooOld`. It never returns a mixture of versions.
- [ ] Releasing a floor allows the WAL to reset at the next checkpoint (integration test).

## Testing and Validation

- Property tests, crash simulation, and a benchmark of the retention cost.

## References

- [Guide §6.4](../architecture/implementation-guide.md#64-checkpoints-and-replay), [§4.5](../architecture/implementation-guide.md#45-retention-and-recovery), [§16 WP-05](../architecture/implementation-guide.md#16-first-implementation-slice--concrete-work-packages)
- `crates/pizzlydb-storage/src/{pager,engine}.rs`
