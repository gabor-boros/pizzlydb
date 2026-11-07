# 021: Automatic checkpointing and WAL growth control

| Field | Value |
| --- | --- |
| Phase | 1A Storage engine hardening |
| Priority | Must |
| Size | M |
| Crates | `pizzlydb-storage` (`engine`, `pager`, `wal`) |
| Type | Feature, robustness |

## Summary

Run checkpoints automatically on a background thread when thresholds are reached: WAL bytes, frames since the last checkpoint, or elapsed time. Each checkpoint is budgeted, so it copies a bounded number of pages per step, yields between steps, and limits its I/O rate. Define the policy for the case where long-lived snapshots prevent the WAL from being reset:

- Warn at a soft limit.
- Apply writer backpressure at a hard limit.
- Optionally expire the snapshots that block progress, in coordination with [037](037-snapshot-retention-and-transaction-limits.md).

## Motivation

Today a checkpoint happens only when the caller invokes `Storage::checkpoint()`. Without automatic checkpoints, the WAL and its index grow forever, recovery time grows with the WAL, and disk use is unbounded. Production engines must keep recovery time and WAL size within configured bounds without operator action (guide invariant C9, guide §10).

## Current State

- `crates/pizzlydb-storage/src/engine.rs`: `checkpoint()` (around lines 483–561) is manual and synchronous. It copies the WAL page versions visible at the oldest snapshot into the main file, syncs, writes the superblock, and prunes the WAL.
- Checkpoint progress is limited by the oldest registered snapshot (`visible`), but nothing reports this or acts on it.
- There is no background thread, because the engine is single-threaded until [016](016-thread-safe-storage-engine.md).

## Scope

### In scope

- `CheckpointPolicy { wal_soft_bytes, wal_hard_bytes, max_interval, io_budget }` with documented defaults.
- A background checkpointer that runs incremental checkpoint steps (copy N pages per step), so that commits are never blocked for the duration of a full checkpoint. The superblock is written only when a step range completes.
- Behavior when blocked:
  - Report the oldest snapshot through stats and a typed event.
  - At the hard limit, either apply backpressure to writers (commit returns `ResourceExhausted` or waits with a timeout), or expire the oldest snapshots if policy allows it. Expired readers get `SnapshotTooOld`, defined in [037](037-snapshot-retention-and-transaction-limits.md).
- Checkpoint on clean `close()`, so restart replays a near-empty WAL.
- Recovery-time estimate exposed in stats (WAL bytes since the checkpoint).

### Out of scope

- A unified maintenance scheduler ([115](115-background-maintenance-scheduler.md)); this issue ships a dedicated checkpointer that the scheduler later absorbs.

## Dependencies

- [016](016-thread-safe-storage-engine.md): background threads.
- [013](013-disk-resident-wal-index.md): WAL index pruning.
- [012](012-crash-safe-wal-reset.md): the safe reset protocol.

## Acceptance Criteria

- [ ] Under a sustained write workload with no manual checkpoint, WAL size stays below `wal_soft_bytes` plus one checkpoint step in steady state (24-hour soak test in the nightly job; 10 minutes on each PR).
- [ ] p99 commit latency during a checkpoint is within 2x of p99 without one (benchmark).
- [ ] With a reader that pins a snapshot for 10 minutes, the WAL grows to the hard limit and then follows the configured policy (backpressure or expiry), and the database is still correct.
- [ ] A crash during any checkpoint step loses no committed data (crash simulation).

## Testing and Validation

- Crash simulation over incremental checkpoint steps.
- Soak test with RSS and disk usage tracked over time.

## References

- [Guide §4.5 retention and recovery](../architecture/implementation-guide.md#45-retention-and-recovery)
- `crates/pizzlydb-storage/src/engine.rs` (`checkpoint`)
