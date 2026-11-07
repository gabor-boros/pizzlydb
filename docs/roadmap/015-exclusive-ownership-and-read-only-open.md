# 015: Exclusive database ownership and a true read-only open mode

| Field | Value |
| --- | --- |
| Phase | 1A Storage engine hardening |
| Priority | Must |
| Size | M |
| Crates | `pizzlydb-storage` (`engine`, `backup`, `verify`, `inspect`), `pizzlydb-graph` (`salvage`, `verify`) |
| Type | Correctness, safety |

## Summary

1. Guarantee that at most one read-write owner exists for a database: across handles in one process and across processes, enforced with the lock primitive from [011](011-vfs-capabilities.md).
2. Add `Storage::open_read_only`. It never writes to the main file or the WAL, performs recovery in memory only, and takes a shared lock, which means it fails while a writer holds the exclusive lock unless the caller explicitly allows it.
3. Move offline verify, inspect, backup, and salvage onto read-only opens.

## Motivation

Two read-write openers corrupt the WAL. Today `backup`, `verify_quick`, and `salvage` call `Storage::open`, which is read-write and may write during recovery (for example, discarding an unsynced tail). Running them against a database that another process is using is unsafe, and nothing prevents it. Read-only open is also how forensic tools must access a damaged file without making it worse.

## Current State

- `crates/pizzlydb-storage/src/engine.rs`: `Storage::open(vfs, path, frame_count, durability)` always opens read-write.
- `crates/pizzlydb-storage/src/backup.rs` (around line 87) and `verify.rs` (around line 36) call `Storage::open`.
- `crates/pizzlydb-storage/src/inspect.rs` opens files read-only, but only for header inspection, not for snapshot reads.
- `crates/pizzlydb-graph/src/salvage.rs` (`open_readable`) decides between opening with the WAL or with the checkpoint only, but it still writes.
- No file lock exists anywhere.

## Scope

### In scope

- `Storage::create` and `Storage::open` acquire an exclusive lock that is held for the lifetime of `Storage`. A second opener receives a new `StorageError::Locked`.
- `Storage::open_read_only(vfs, path, frames, OpenMode { allow_concurrent_writer: bool })`:
  - It replays the WAL into the in-memory index without truncating, zeroing, or writing anything.
  - It exposes `snapshot()` only, and `begin_write` returns an error.
  - The `allow_concurrent_writer` mode is for diagnosis only and documents that results may be inconsistent. A live backup must use [105](105-online-consistent-backup.md), not this mode.
- Use read-only open in `verify_quick`, `backup`, `export_checkpoint`, `inspect`, `verify_full`, and `salvage`.
- Document the lock-file name and behavior: stale lock handling relies on kernel lock release, never on PID files.

### Out of scope

- Multi-process read sharing with a live writer (a shared-memory WAL index); this is not planned.

## Dependencies

- [011](011-vfs-capabilities.md): the lock primitive.

## Acceptance Criteria

- [ ] Opening the same path twice read-write (in the same process, and from a child process) fails with `Locked`.
- [ ] `open_read_only` on a database with an unsynced WAL tail leaves both files byte-identical (checked by a hash before and after).
- [ ] `verify_quick`, `backup`, and `salvage` succeed on a file with mode `0444`.
- [ ] The lock is released after `close()`, `drop`, and process exit (multi-process test).

## Testing and Validation

- Integration tests with `PosixVfs` in a temp directory and with `MemoryVfs` (whose lock is emulated).
- Crash-simulation run: a read-only open never changes the durable image.

## References

- `crates/pizzlydb-storage/src/{engine,backup,verify,inspect}.rs`, `crates/pizzlydb-graph/src/salvage.rs`
