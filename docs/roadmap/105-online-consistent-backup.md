# 105: Online consistent backup with a manifest

| Field | Value |
| --- | --- |
| Phase | 2 Operational layer |
| Priority | Must |
| Size | L |
| Crates | `pizzlydb-storage` (`backup`), `pizzlydb-engine` |
| Type | Feature |

## Summary

Take a consistent backup of a *live* database without blocking writers. The backup copies every reachable page as of a registered snapshot at `CommitSeq` S, read through the pager at that snapshot. It includes the graph, the system key space (commit log, plans, maintained state, subscriptions), and all metadata, and it skips free pages. A backup manifest is written with:

- the manifest version and the format versions
- the database lineage id and a backup id
- S, together with its LSN and timestamp
- the page count and the namespace list
- per-chunk and whole-backup checksums
- the tool version and the creation time

The backup is published atomically by writing to a temporary directory, syncing, renaming, and syncing the parent directory. I/O is throttled.

## Motivation

Guide §10.2 requires a consistent online backup that captures the authoritative graph state, index metadata, schema, and the committed frontier, plus enough log and checkpoint metadata to restore a coherent database. It also requires a manifest, checksums, and encryption metadata. The current `backup()` has these gaps:

- It is offline only, and it opens the source read-write.
- It copies free pages.
- It writes the destination directly rather than through a temporary location and rename.
- It produces no manifest.

All durable state lives in one storage engine ([064](064-maintained-state-and-output-log.md), [068](068-durable-subscriptions-core.md)), so one snapshot captures everything consistently. No coordination across subsystems is needed.

## Current State

- `crates/pizzlydb-storage/src/backup.rs`: `backup(vfs, source, destination)` calls `Storage::open`, copies pages `0..high_water`, writes superblocks with generations 2 and 1 and an empty WAL, and calls `sync_dir`. [002](002-offline-tool-identity-and-limits.md) fixes its file identity.
- `export_checkpoint` exists.

## Scope

### In scope

- `Database::backup(destination, BackupOptions { io_rate_limit, chunk_bytes })` runs against a live engine.
  - It registers a backup-class snapshot ([037](037-snapshot-retention-and-transaction-limits.md)) and streams pages from that snapshot in chunks.
  - Free pages are written as holes (sparse) or omitted with a map.
  - The destination is a fresh database with a new file identity and the source's lineage id ([002](002-offline-tool-identity-and-limits.md)).
- **Manifest format v1** (documented and versioned). Checksums use BLAKE3 or SHA-256, chosen in the ADR, per chunk and for the whole backup.
- **Atomic publication** of the backup directory.
- **Progress reporting and cancellation.**
- **Offline mode.** The old offline backup becomes `backup --offline`, using a read-only open ([015](015-exclusive-ownership-and-read-only-open.md)).

### Out of scope

- Restore and verification ([108](108-backup-verification-and-restore.md)), encryption and compression ([106](106-backup-encryption-compression-destinations.md)), and incremental backup with PITR ([118](118-log-archiving-and-point-in-time-restore.md)).

## Dependencies

- [015](015-exclusive-ownership-and-read-only-open.md): read-only open and locking.
- [016](016-thread-safe-storage-engine.md): concurrent snapshot reads during writes.
- [024](024-storage-format-versioning-and-golden-fixtures.md): versions recorded in the manifest.
- [031](031-commit-sequence-and-commit-metadata.md): `CommitSeq` S.

## Acceptance Criteria

- [ ] A backup taken during a sustained write workload restores to exactly the state at S. The check compares a reference dump at S with a dump of the restored database.
- [ ] Writer p99 latency during a backup is within 2x of the baseline when the default I/O rate limit applies (benchmark).
- [ ] A crash during the backup leaves no published backup, and the source is unaffected (crash simulation).
- [ ] Backup size scales with the live pages, not with the high-water mark (test with many freed pages).
- [ ] The manifest format is documented, and a fuzz target exists for the manifest decoder.

## Testing and Validation

- Integration tests, crash simulation, a benchmark, and fuzzing.

## References

- [Guide §10.2 backups and restore](../architecture/implementation-guide.md#102-backups-and-restore)
- `crates/pizzlydb-storage/src/backup.rs`
