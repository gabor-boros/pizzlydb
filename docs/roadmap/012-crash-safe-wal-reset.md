# 012: Crash-safe WAL reset: remove the in-place prune and add salted WAL generations

| Field | Value |
| --- | --- |
| Phase | 1A Storage engine hardening |
| Priority | Must (durability bug, critical path) |
| Size | M |
| Crates | `pizzlydb-storage` (`wal`, `pager`, `engine`, `superblock`) |
| Type | Bug fix, on-disk format change |

## Summary

`Wal::prune` rewrites the frames it keeps in place, starting at the WAL header, then zeroes the tail, and calls `sync` only once at the end. On real storage, a power loss before that sync completes can persist any subset of those writes. The result is either lost acknowledged commits or a WAL whose durable position is behind the checkpoint, which reopens as `Corrupt`.

Replace prune-by-rewrite with a WAL *generation* (salt) design, in which old frames are never overwritten while they may still be needed and stale frames can never be mistaken for current ones.

## Motivation

This is the most serious defect found in the assessment, because it violates guide invariant C3 (durability): no acknowledged commit may be lost. It is only reachable through `checkpoint()`, but checkpointing is the only way to bound WAL memory, so every long-running deployment would hit it.

Here is how the loss happens. Kept frames (LSN greater than the horizon) are copied toward the start of the file, and the region where they used to live is then overwritten by other frames or by zeros. If the kernel writes back the zeroing of the tail but not the relocated frames, both copies of those commits are gone. The current `MemoryVfs` cannot exhibit this failure; [004](004-crash-consistency-simulation-vfs.md) can.

## Current State

- `crates/pizzlydb-storage/src/wal.rs`, `Wal::prune` (around lines 749–799): it rewrites the header and the kept frames from `header_end`, zeroes `[end, allocated_end)`, and then makes a single `self.file.sync()`.
- `discard_unsynced` zeroes the unsynced tail without a sync.
- The WAL header holds a magic number, version 1, and the file identity, but no generation or salt. Frames carry a CRC but no salt, so a stale frame left over from an earlier generation is valid if its CRC matches.
- `Pager::recover_floor` (`crates/pizzlydb-storage/src/pager.rs`, around lines 925–941) treats `durable < checkpoint` as `Corrupt`, except when `durable == 0`.
- The checkpoint sequence in `engine.rs` (around lines 483–561) is: sync WAL, write main, sync main, write the other superblock, sync, then prune (and a failed prune closes the handle).

## Scope

### In scope

- Add a WAL header field `generation` (or salt pair) derived from the checkpoint. Each frame header includes the generation, and its CRC covers it. Recovery accepts only frames whose generation matches the header and stops at the first mismatch.
- Reset protocol: once the checkpoint superblock is durable, start a new generation by writing a new header (generation + 1) and syncing it. Frames appended after that overwrite stale frames from the start, and stale bytes are ignored because of their generation. Do not zero the tail, because zeroing is unnecessary with salts.
- Never relocate kept frames. While frames above the checkpoint horizon exist (a reader pins an older snapshot), either skip the reset (the WAL keeps growing until the readers release it, a policy that belongs to [021](021-automatic-checkpointing-and-wal-bounds.md)) or reset only when `visible == committed`.
- Superblock records the WAL generation that it checkpointed, so that a WAL from an older generation is recognized as fully checkpointed and is not reported as corrupt.
- Replace the `durable < checkpoint` corruption rule with a generation-aware rule. Keep a real corruption check for a WAL generation newer than the superblock that contains no valid frames.
- Bump the on-disk format version and make the format change explicit (pre-1.0, no upgrade path is required; see [024](024-storage-format-versioning-and-golden-fixtures.md)).

### Out of scope

- A disk-resident WAL index ([013](013-disk-resident-wal-index.md)); this issue keeps the in-memory index.
- Segmented WAL files. The single-file-with-generations design is sufficient. Segmenting can be reconsidered in [013](013-disk-resident-wal-index.md) if archiving ([118](118-log-archiving-and-point-in-time-restore.md)) needs it.

## Dependencies

- [004](004-crash-consistency-simulation-vfs.md): the crash simulator, which supplies the failing regression test.
- [010](010-storage-error-taxonomy-fidelity.md): error taxonomy.

## Acceptance Criteria

- [x] The [004](004-crash-consistency-simulation-vfs.md) regression test, which reproduces the prune hazard, passes and is no longer ignored.
- [ ] Exhaustive crash enumeration over "commit N, checkpoint, commit M, checkpoint" histories with at least 10,000 persistence subsets shows that every commit acknowledged under `Durability::Full` survives and that every reopen succeeds.
- [x] A stale frame (old generation, valid CRC) placed after the current frames is ignored during recovery (unit test).
- [x] No code path overwrites a WAL byte range that holds a frame above the durable checkpoint.
- [x] Format version bumped; opening an old-format file returns `UnsupportedVersion`.

## Testing and Validation

- Crash-simulation property tests (seeded, nightly large budget).
- A unit test for each recovery branch: empty WAL, WAL from an older generation, mixed generations, torn header, and torn first frame.
- Benchmark before and after ([005](005-benchmark-harness-and-baselines.md)): checkpoint latency must not regress by more than 10%.

## References

- [Guide §12.2 C3 durability](../architecture/implementation-guide.md#122-mandatory-correctness-properties)
- [SQLite WAL format (salt values)](https://www.sqlite.org/fileformat2.html#walformat)
- `crates/pizzlydb-storage/src/wal.rs`, `pager.rs`, `engine.rs`, `superblock.rs`
