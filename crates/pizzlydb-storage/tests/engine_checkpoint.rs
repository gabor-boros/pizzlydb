//! Checkpoint versus open snapshots, and a log that no longer holds copied pages.

#[path = "common/mod.rs"]
mod common;

use std::path::Path;

#[cfg(unix)]
use pizzlydb_storage::PosixVfs;
use pizzlydb_storage::{Durability, FileRole, MemoryVfs, Storage, StorageError, Vfs};

use common::{apply, fill, records, scan_view, wal_path};

#[test]
fn memory_header_only_log_still_reads() {
    let vfs = MemoryVfs::new();
    let path = Path::new("check");
    let db = Storage::create(&vfs, path, 8, Durability::Full, fill(5)).unwrap();
    apply(
        &db,
        &[(b"a", Some(b"1")), (b"b", Some(b"2"))],
        Durability::Full,
    );
    let expected = records(&db);
    let held = db.snapshot().unwrap();
    db.checkpoint().unwrap();
    assert_eq!(scan_view(&held), expected);
    drop(held);
    drop(db);

    let wal = wal_path(path);
    let bytes = common::read_memory(&vfs, &wal, FileRole::Wal);
    assert!(bytes.len() >= 64);
    vfs.replace(&wal, FileRole::Wal, &bytes[..64]).unwrap();
    let db = Storage::open(&vfs, path, 8, Durability::Full).unwrap();
    assert_eq!(records(&db), expected);
}

#[test]
fn memory_long_reader_keeps_checkpoint_behind_a_later_commit() {
    let vfs = MemoryVfs::new();
    let path = Path::new("long-reader");
    let db = Storage::create(&vfs, path, 8, Durability::Full, fill(16)).unwrap();
    long_reader(&db);
    drop(db);
    let db = Storage::open(&vfs, path, 8, Durability::Full).unwrap();
    assert_eq!(
        records(&db).get(b"b".as_slice()).map(Vec::as_slice),
        Some(&b"2"[..])
    );
    db.checkpoint().unwrap();
    drop(db);

    let wal = wal_path(path);
    let bytes = common::read_memory(&vfs, &wal, FileRole::Wal);
    assert!(bytes.len() >= 64);
    vfs.replace(&wal, FileRole::Wal, &bytes[..64]).unwrap();
    let db = Storage::open(&vfs, path, 8, Durability::Full).unwrap();
    let map = records(&db);
    assert_eq!(map.get(b"a".as_slice()).map(Vec::as_slice), Some(&b"1"[..]));
    assert_eq!(map.get(b"b".as_slice()).map(Vec::as_slice), Some(&b"2"[..]));
}

#[test]
fn memory_checkpoint_while_a_batch_is_open_is_busy() {
    let vfs = MemoryVfs::new();
    let db = Storage::create(&vfs, Path::new("check-busy"), 4, Durability::Full, fill(17)).unwrap();
    checkpoint_is_busy(&db);
}

#[cfg(unix)]
#[test]
fn posix_header_only_log_still_reads() {
    let (_dir, path, wal) = common::posix_paths("check");
    let vfs = PosixVfs::new();
    let db = Storage::create(&vfs, &path, 8, Durability::Full, fill(5)).unwrap();
    apply(
        &db,
        &[(b"a", Some(b"1")), (b"b", Some(b"2"))],
        Durability::Full,
    );
    let expected = records(&db);
    let held = db.snapshot().unwrap();
    db.checkpoint().unwrap();
    assert_eq!(scan_view(&held), expected);
    drop(held);
    drop(db);

    assert!(common::file_len(&wal) >= 64);
    common::truncate_file(&wal, 64);
    let db = Storage::open(&vfs, &path, 8, Durability::Full).unwrap();
    assert_eq!(records(&db), expected);
}

#[cfg(unix)]
#[test]
fn posix_long_reader_keeps_checkpoint_behind_a_later_commit() {
    let (_dir, path, wal) = common::posix_paths("long-reader");
    let vfs = PosixVfs::new();
    let db = Storage::create(&vfs, &path, 8, Durability::Full, fill(16)).unwrap();
    long_reader(&db);
    drop(db);
    let db = Storage::open(&vfs, &path, 8, Durability::Full).unwrap();
    assert_eq!(
        records(&db).get(b"b".as_slice()).map(Vec::as_slice),
        Some(&b"2"[..])
    );
    db.checkpoint().unwrap();
    drop(db);

    assert!(common::file_len(&wal) >= 64);
    common::truncate_file(&wal, 64);
    let db = Storage::open(&vfs, &path, 8, Durability::Full).unwrap();
    let map = records(&db);
    assert_eq!(map.get(b"a".as_slice()).map(Vec::as_slice), Some(&b"1"[..]));
    assert_eq!(map.get(b"b".as_slice()).map(Vec::as_slice), Some(&b"2"[..]));
}

#[cfg(unix)]
#[test]
fn posix_checkpoint_while_a_batch_is_open_is_busy() {
    let (_dir, path, _wal) = common::posix_paths("check-busy");
    let vfs = PosixVfs::new();
    let db = Storage::create(&vfs, &path, 4, Durability::Full, fill(17)).unwrap();
    checkpoint_is_busy(&db);
}

fn long_reader(db: &Storage<impl Vfs>) {
    apply(db, &[(b"a", Some(b"1"))], Durability::Full);
    let held = db.snapshot().unwrap();
    apply(db, &[(b"b", Some(b"2"))], Durability::Full);
    db.checkpoint().unwrap();

    assert_eq!(held.get(b"a").unwrap().as_deref(), Some(&b"1"[..]));
    assert!(held.get(b"b").unwrap().is_none());
    let fresh = db.snapshot().unwrap();
    assert_eq!(fresh.get(b"b").unwrap().as_deref(), Some(&b"2"[..]));
    drop(held);
    drop(fresh);
}

fn checkpoint_is_busy(db: &Storage<impl Vfs>) {
    let batch = db.begin_write().unwrap();
    assert!(matches!(db.checkpoint(), Err(StorageError::Busy)));
    drop(batch);
    db.checkpoint().unwrap();
}
