# 013: Disk-resident WAL with a bounded in-memory version index

| Field | Value |
| --- | --- |
| Phase | 1A Storage engine hardening |
| Priority | Must (critical path) |
| Size | L |
| Crates | `pizzlydb-storage` (`wal`, `pager`) |
| Type | Architecture, scalability |

## Summary

Stop holding every committed WAL page image in RAM. Keep frames on disk, and maintain a compact in-memory index from each page id to its versions, each version recorded as an LSN and a frame offset. Read WAL page versions through the buffer pool on demand. Recovery must scan the WAL as a stream, without reading the whole file into memory.

## Motivation

The WAL holds a full copy of every page written since the last checkpoint. Because `Wal` keeps all of them in a `Vec<CommittedPage>`, memory use equals WAL size. A write-heavy workload, or a single long reader that blocks checkpointing ([021](021-automatic-checkpointing-and-wal-bounds.md)), grows memory without bound until the process is killed. `Wal::open` reads the whole file, so a 50 GiB WAL needs 50 GiB of RAM just to open the database.

## Current State

- `crates/pizzlydb-storage/src/wal.rs`: `Wal { pages: Vec<CommittedPage>, ... }` (around lines 386–395) keeps an 8 KiB image per frame.
- `Wal::open` (around lines 499–519) calls `read_file(&mut file)`, which reads the entire file.
- `crates/pizzlydb-storage/src/pager.rs`: `State::newest_wal_lsn` and `State::wal_image` (around lines 1499–1524) scan `wal.pages()` linearly on every page load, and `find_cached` calls `newest_wal_lsn` even on cache hits. Page access cost is therefore O(WAL frames).

## Scope

### In scope

- A WAL index that maps `PageId -> small sorted Vec<(Lsn, FrameOffset)>` (or a sharded hash map). Memory is O(distinct pages × versions) at roughly 16–24 bytes per version, documented and exposed in stats.
- On-demand loading of WAL images into buffer-pool frames by offset, verified by frame CRC and generation ([012](012-crash-safe-wal-reset.md)).
- Streaming recovery: read the header, then read frames sequentially with a bounded buffer while building the index; truncate logically at the first invalid frame.
- Pruning the index on checkpoint and reset.
- Version lookup in O(log versions) per page.
- An explicit memory bound: if the index would exceed a configured limit, writers get backpressure, and the case is reported as `ResourceExhausted` or `WalFull`, so the process never aborts on OOM. The policy is coordinated with [021](021-automatic-checkpointing-and-wal-bounds.md).

### Out of scope

- Concurrency (addressed in [016](016-thread-safe-storage-engine.md)). The index design must allow concurrent readers later; document the intended locking.
- Changing WAL content from page images to logical or physiological records. That would require an RFC and is not planned before 1.0.

## Dependencies

- [012](012-crash-safe-wal-reset.md): frame generation and the reset protocol.

## Acceptance Criteria

- [ ] Peak RSS while writing a 4 GiB WAL, with no checkpoint and a 64 MiB buffer pool, stays below 64 MiB plus the index size plus a constant (measured in the benchmark harness from [005](005-benchmark-harness-and-baselines.md)).
- [ ] Opening a database with a 4 GiB WAL does not allocate O(WAL) memory.
- [ ] Point-read latency does not grow linearly with WAL length (benchmark curve published).
- [ ] All existing crash, snapshot, and checkpoint suites pass, plus the crash-simulation enumeration from [012](012-crash-safe-wal-reset.md).
- [x] The documentation states the memory formula for the index.

## Testing and Validation

- Property test: for random commit and snapshot histories, the page version returned for `(page, snapshot)` equals that of the old in-memory implementation, which serves as the reference.
- A crash enumeration during streaming recovery that combines a torn final frame with a stale generation.

## References

- [SQLite WAL index (wal-index / shm)](https://www.sqlite.org/walformat.html)
- `crates/pizzlydb-storage/src/wal.rs`, `pager.rs`
