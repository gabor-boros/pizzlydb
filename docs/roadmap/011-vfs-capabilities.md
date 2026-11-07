# 011: VFS capabilities: advisory locking, truncation, preallocation, and data-only sync

| Field | Value |
| --- | --- |
| Phase | 1A Storage engine hardening |
| Priority | Must |
| Size | M |
| Crates | `pizzlydb-storage` (`vfs`, `vfs_posix`, `vfs_memory`, crash simulator) |
| Type | Feature |

## Summary

Extend the `Vfs` and `VfsFile` traits with the primitives a production engine needs:

- An exclusive or shared advisory lock on a database.
- `set_len`, for truncation and shrinking.
- Space preallocation, using `fallocate` or `posix_fallocate` where available.
- A data-only sync (`fdatasync`) that is distinct from a full metadata sync.

Implement them for POSIX, the in-memory VFS, the fault injector, and the crash simulator.

## Motivation

- **Locking:** nothing stops two handles, or two processes, from opening the same database read-write. Both would append to the WAL and corrupt it. [015](015-exclusive-ownership-and-read-only-open.md) needs a lock primitive.
- **Truncation:** files can only grow. There is no way to shrink a WAL after checkpoint or reclaim tail pages ([113](113-space-reclamation-and-compaction.md)).
- **Preallocation:** without reserved space, an `ENOSPC` during checkpoint can wedge the database ([022](022-disk-full-and-io-error-safety.md)).
- **fdatasync:** every sync calls `File::sync_all` (`fsync`), even for WAL appends where `fdatasync` is sufficient once the file size is stable. This doubles the metadata flushes on the commit path ([020](020-group-commit-and-durability-modes.md)).

## Current State

- `crates/pizzlydb-storage/src/vfs.rs`: the `Vfs` trait has `open`, `sync_dir`, `rename`, and `remove`. `VfsFile` has `read_at`, `write_at`, `sync`, `len`, `close`, and `role`. None of them provides lock, `set_len`, preallocation, or a data sync.
- `crates/pizzlydb-storage/src/vfs_posix.rs` (around lines 164–167) implements `sync` as `self.file.sync_all()`.
- The `Vfs` and `VfsFile` traits have no resize or preallocation operation.

## Scope

### In scope

- `Vfs::lock(path, mode) -> Result<LockGuard, VfsError>` with `Exclusive` and `Shared` modes and a non-blocking `WouldBlock` error mapped to a new `VfsError::Locked`. On POSIX, use `flock` or OFD `fcntl` locks on a dedicated lock file or on the main file. The ADR must state the choice and its NFS caveats.
- `VfsFile::set_len(len)`, `VfsFile::preallocate(offset, len)` (best effort with a documented fallback), and `VfsFile::sync_data()`.
- Rustdoc for each new method describing its crash semantics, which the crash simulator implements exactly ([004](004-crash-consistency-simulation-vfs.md)).
- Fault-injection hooks for every new operation.

### Out of scope

- Using these primitives in the engine; issues 015, 021, 022, and 113 do that.

## Dependencies

- [004](004-crash-consistency-simulation-vfs.md): the crash simulator must model `set_len` and `sync_data` semantics.

## Acceptance Criteria

- [ ] All VFS implementations support the new methods, and the MemoryVfs crash model reflects truncate and data-sync semantics.
- [ ] A test with two `PosixVfs` handles in separate threads, and one with separate processes, shows that the exclusive lock is enforced. The lock is released on drop and on process death.
- [ ] `sync_data` maps to `fdatasync` on Linux and to `F_FULLFSYNC` or the documented equivalent on macOS. The macOS caveat (plain `fsync` does not flush the drive cache) is documented and handled.
- [ ] No public API breakage without a semver note; traits are versioned or sealed as appropriate.

## Testing and Validation

- Unit tests for each method under `MemoryVfs`, `PosixVfs` (in a temp directory), and the fault injector.
- A multi-process lock test that spawns a child with `std::process::Command`.

## References

- `crates/pizzlydb-storage/src/vfs.rs`, `vfs_posix.rs`, `vfs_memory.rs`
- [SQLite: How to corrupt a database file (locking)](https://www.sqlite.org/howtocorrupt.html)
