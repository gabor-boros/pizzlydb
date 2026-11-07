# 108: Backup verification, restore, incarnation epochs, and restore drills

| Field | Value |
| --- | --- |
| Phase | 2 Operational layer |
| Priority | Must |
| Size | L |
| Crates | `pizzlydb-storage` (`backup`), `pizzlydb-engine`, `pizzlydb-subscriptions` |
| Type | Feature |

## Summary

This issue builds the restore half of the backup cycle:

- **Verify without restoring:** check a backup's manifest and checksums before any data is written.
- **Restore to a new data directory.** The command refuses to write into a directory that is locked or not empty. It decrypts and decompresses the backup, writes a fresh file identity, keeps the lineage, and runs `verify_full` before publishing.
- **Incarnation epoch.** Every restore bumps a database *incarnation epoch*. Resume tokens and cursors issued after the backup point then fail with a typed error instead of silently skipping data.
- **Rebuild path.** Restore can rebuild maintained incremental state from scratch.
- **Restore drills.** An automated drill periodically restores the latest backup into scratch space and records the result as a metric.

## Motivation

Guide §10.2 makes restores that fail a release blocker, even when the backup appeared to succeed. It requires restores to be tested regularly, including a restore to different hardware and a restore followed by reconstruction of incremental state.

Restores have a further, subtle hazard. Subscribers may have acknowledged positions *after* the backup's S. Without an epoch check, a resume against the restored database would skip the changes between S and the client's position, which violates invariant R2.

## Current State

- `crates/pizzlydb-storage/src/backup.rs`: `restore` runs `verify_quick`, copies the files to `.partial`, and then calls `publish`, which renames into place with park and undo, then calls `sync_dir`.
- Nothing tracks epochs, and nothing runs drills.

## Scope

### In scope

- `pizzlydb backup verify <path>`, which checks the manifest, the checksums, decryption, and `verify_quick` on a temporary materialization.
- `restore(backup, target_dir, RestoreOptions { rebuild_incremental, epoch_bump: always })` with publication through a temporary directory and rename.
- **Incarnation epoch.** It is stored in the system key space and included in resume tokens ([070](070-resume-tokens.md)) and in heartbeats. Validation rejects a token from an earlier epoch whose position is above the restored frontier, returning `CURSOR_EXPIRED` or `RESUME_FORBIDDEN` with the reason `RESTORED`.
- **Rebuild mode.** It drops the maintained state and reinstalls plans from snapshot S ([065](065-plan-installation-and-lifecycle.md)).
- **Drill job.** It runs on a schedule and records `last_successful_restore_drill_timestamp` and the drill duration ([102](102-metrics-foundation.md)).
- **Cross-architecture test.** A backup taken on x86-64 restores on ARM64 in CI.
- **Runbook:** `docs/operations/backup-restore.md`.

### Out of scope

- Point-in-time restore ([118](118-log-archiving-and-point-in-time-restore.md)).

## Dependencies

- [105](105-online-consistent-backup.md): the backup format.
- [107](107-full-verification-and-online-scrubbing.md): full verification.
- [071](071-embedded-engine-composition.md): engine recovery and plan reinstallation.

## Acceptance Criteria

- [ ] Each corrupted-backup class makes `backup verify` fail with a typed error that names the failing chunk.
- [ ] Restoring into a non-empty or locked directory is refused (test).
- [ ] After a restore, a subscriber holding a token positioned after S receives a typed error. A subscriber positioned before S resumes correctly. Neither case produces a silent gap (end-to-end test).
- [ ] In rebuild mode, maintained results after the restore equal the oracle at S.
- [ ] The cross-architecture restore job passes in CI.

## Testing and Validation

- End-to-end backup, write, restore, and resume tests, plus fault tests and the cross-architecture CI job.

## References

- [Guide §10.2](../architecture/implementation-guide.md#102-backups-and-restore), [§7.2](../architecture/implementation-guide.md#72-delivery-guarantee), [§7.3](../architecture/implementation-guide.md#73-resume-tokens)
- `crates/pizzlydb-storage/src/backup.rs`
