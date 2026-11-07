# 037: Snapshot retention, long-reader limits, snapshot leases, and transaction expiry

| Field | Value |
| --- | --- |
| Phase | 1B Transactions and logical commit log |
| Priority | Must |
| Size | M |
| Crates | `pizzlydb-txn`, `pizzlydb-storage` |
| Type | Robustness, resource control |

## Summary

Bound how long snapshots and transactions can pin resources: WAL versions, unreclaimed pages, and the recent-write-set history. The issue defines:

- a maximum transaction lifetime, a maximum idle time, and a maximum snapshot age
- a forced-expiry mechanism that fails the affected reads with `SnapshotTooOld` or `TxExpired`
- explicit, renewable *snapshot leases* for multi-request work (pagination in [058](058-ordering-and-pagination.md), subscription bootstrap in [069](069-subscription-bootstrap-and-event-stream.md))
- introspection of the oldest snapshot holders

## Motivation

One forgotten reader can block checkpoint progress ([021](021-automatic-checkpointing-and-wal-bounds.md)), page reclamation, and transaction validation history ([033](033-optimistic-transaction-manager.md)) indefinitely. Guide §4.4 requires configured lifetimes, and guide §4.5 requires that snapshot retention be inspectable.

## Current State

- `crates/pizzlydb-storage/src/pager.rs`: `State::snapshots: Vec<(u64, Lsn)>`. Registration lasts as long as the `ReadView` exists, and there is no age tracking.
- No limits or leases exist, and no API reports the oldest snapshot.

## Scope

### In scope

- Registration timestamps and owner tags for snapshots, recording what kind of work holds each one: query, transaction, bootstrap, backup, or maintenance.
- `TxnLimits { max_lifetime, max_idle, max_snapshot_age }` with documented defaults.
- Enforcement: the next operation on an expired transaction or snapshot returns `TxExpired` or `SnapshotTooOld`. A background reaper unregisters snapshots that exceed their limits; their reads then fail and never return wrong data.
- `SnapshotLease`: an id, a TTL, and `renew()`. It is the unit that server-side pagination cursors hold.
- Exempt or longer-limit classes for backup ([105](105-online-consistent-backup.md)) and maintenance, configured explicitly.
- `inspect_snapshots()`, which lists holders with their age, kind, and the resources they pin (WAL bytes, retired pages).

### Out of scope

- Commit-log retention and output-log retention ([114](114-retention-management-and-inspection.md)).

## Dependencies

- [033](033-optimistic-transaction-manager.md): transaction lifecycle.
- [021](021-automatic-checkpointing-and-wal-bounds.md): the checkpointer is the main beneficiary.

## Acceptance Criteria

- [ ] A reader held past `max_snapshot_age` gets `SnapshotTooOld` on its next read, and the WAL then resets at the next checkpoint (integration test).
- [ ] Lease renewal keeps a snapshot alive, and a lapsed lease expires it.
- [ ] `inspect_snapshots()` reports the correct holder, age, and pinned WAL bytes (test).
- [ ] No expired snapshot ever returns data from a reclaimed page; property test with aggressive expiry and reclaim.

## Testing and Validation

- Time-controlled tests with an injectable clock, plus a concurrency stress test with expiry.

## References

- [Guide §4.4](../architecture/implementation-guide.md#44-transactions-and-isolation), [§4.5](../architecture/implementation-guide.md#45-retention-and-recovery)
- `crates/pizzlydb-storage/src/{pager,engine}.rs`
