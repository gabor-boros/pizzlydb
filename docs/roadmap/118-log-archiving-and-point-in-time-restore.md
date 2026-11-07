# 118: Continuous log archiving and point-in-time restore

| Field | Value |
| --- | --- |
| Phase | 2 Operational layer |
| Priority | Should (post-1.0 acceptable; it reduces RPO from the backup interval to seconds) |
| Size | L |
| Crates | `pizzlydb-txn` (`log`), `pizzlydb-engine`, `pizzlydb-storage` (`backup`) |
| Type | Feature |

## Summary

Continuously archive the logical mutation log, in segments of commit records and mutation records, to a backup destination ([106](106-backup-encryption-compression-destinations.md)), together with a manifest chain. Restore to any `CommitSeq` or timestamp T after a base backup S in two steps:

1. Restore the base backup ([108](108-backup-verification-and-restore.md)).
2. Deterministically replay the archived mutations from S+1 through T through the transaction layer, preserving the original ids and `CommitSeq` values.

Incremental state is then rebuilt by normal maintenance replay.

## Motivation

Without archiving, the recovery point objective equals the backup interval: everything committed since the last backup is lost when the disk fails. Point-in-time restore is also the standard remedy for logical mistakes, such as dropping the wrong namespace. The guide does not require it, but production users routinely expect it.

## Current State

- The logical log comes from [032](032-logical-mutation-log.md), with truncation and floors. No archiver exists.

## Scope

### In scope

- **Archiver.** A durable retention floor ([114](114-retention-management-and-inspection.md)) keeps the log until each segment is archived. Segments are sealed at a time or size threshold, then encrypted and compressed, and each segment's manifest links to the previous one with a hash chain. Archive lag is exposed as a metric.
- **Deterministic replay.** Mutations are applied with their recorded ids and `CommitSeq` values, through a privileged replay path that bypasses id allocation and validation but still checks invariants. Schema, namespace, and index records are included.
- **`restore --to-seq N` and `--to-time T`.**
- **Gap detection.** A missing or corrupt segment produces a typed error that names the last good sequence.

### Out of scope

- Physical WAL shipping and replication ([901](901-replication-and-high-availability.md)).

## Dependencies

- [105](105-online-consistent-backup.md): base backups.
- [032](032-logical-mutation-log.md): the log.
- [061](061-as-of-snapshots-at-commit-sequence.md): floors and the `CommitSeq` mapping.

## Acceptance Criteria

- [ ] PITR to a random T yields a state equal to the reference state at T (property test across a base backup and archived segments).
- [ ] Archive lag stays within its configured bound under sustained writes (soak test).
- [ ] A missing segment is detected, never silently skipped (test).

## Testing and Validation

- Property tests, fault tests, and a soak test.

## References

- [Guide §10.2](../architecture/implementation-guide.md#102-backups-and-restore)
- [PostgreSQL continuous archiving and PITR](https://www.postgresql.org/docs/current/continuous-archiving.html) (prior art)
