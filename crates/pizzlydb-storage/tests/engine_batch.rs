//! Writer lock, rollback, close, and one-batch atomicity.

#[path = "common/mod.rs"]
mod common;

use std::path::Path;

#[cfg(unix)]
use pizzlydb_storage::PosixVfs;
use pizzlydb_storage::{Durability, MemoryVfs, Storage, StorageError};

use common::{apply, fill, records};

#[test]
fn memory_second_writer_and_checkpoint_are_busy() {
    let vfs = MemoryVfs::new();
    let db = Storage::create(&vfs, Path::new("busy"), 4, Durability::Full, fill(4)).unwrap();
    writer_and_checkpoint_are_busy(&db);
}

#[test]
fn memory_drop_and_rejected_put_publish_nothing() {
    let vfs = MemoryVfs::new();
    let path = Path::new("rollback");
    let db = Storage::create(&vfs, path, 8, Durability::Full, fill(3)).unwrap();
    drop_and_reject(&db);
    drop(db);
    vfs.crash();
    let db = Storage::open(&vfs, path, 8, Durability::Full).unwrap();
    assert!(records(&db).is_empty());
}

#[test]
fn memory_one_batch_is_atomic_across_crash() {
    let vfs = MemoryVfs::new();
    let path = Path::new("atomic-batch");
    let db = Storage::create(&vfs, path, 8, Durability::Full, fill(13)).unwrap();
    one_batch_rules(&db);
    apply(
        &db,
        &[(b"c", Some(b"3")), (b"d", Some(b"4"))],
        Durability::None,
    );
    drop(db);
    vfs.crash();
    let db = Storage::open(&vfs, path, 8, Durability::Full).unwrap();
    let map = records(&db);
    assert_eq!(map.get(b"a".as_slice()).map(Vec::as_slice), Some(&b"2"[..]));
    assert_eq!(map.get(b"b".as_slice()).map(Vec::as_slice), Some(&b"2"[..]));
    assert!(!map.contains_key(b"c".as_slice()));
    assert!(!map.contains_key(b"d".as_slice()));
}

#[test]
fn memory_atomic_write_sync_survives_crash() {
    let vfs = MemoryVfs::new();
    let path = Path::new("atomic");
    let db = Storage::create(&vfs, path, 8, Durability::Full, fill(10)).unwrap();
    atomic_sync(&db);
    drop(db);
    vfs.crash();
    let db = Storage::open(&vfs, path, 8, Durability::None).unwrap();
    assert_eq!(
        records(&db).get(b"a".as_slice()).map(Vec::as_slice),
        Some(&b"1"[..])
    );
}

#[test]
fn memory_close_rejects_later_calls() {
    let vfs = MemoryVfs::new();
    let db = Storage::create(&vfs, Path::new("shut"), 2, Durability::Full, fill(11)).unwrap();
    close_rejects(&db);
}

#[cfg(unix)]
#[test]
fn posix_second_writer_and_checkpoint_are_busy() {
    let (_dir, path, _wal) = common::posix_paths("busy");
    let vfs = PosixVfs::new();
    let db = Storage::create(&vfs, &path, 4, Durability::Full, fill(4)).unwrap();
    writer_and_checkpoint_are_busy(&db);
}

#[cfg(unix)]
#[test]
fn posix_drop_and_rejected_put_publish_nothing() {
    let (_dir, path, wal) = common::posix_paths("rollback");
    let vfs = PosixVfs::new();
    let db = Storage::create(&vfs, &path, 8, Durability::Full, fill(3)).unwrap();
    drop_and_reject(&db);
    let synced = common::file_len(&wal);
    drop(db);
    common::truncate_file(&wal, synced);
    let db = Storage::open(&vfs, &path, 8, Durability::Full).unwrap();
    assert!(records(&db).is_empty());
}

#[cfg(unix)]
#[test]
fn posix_one_batch_is_atomic_across_crash() {
    let (_dir, path, wal) = common::posix_paths("atomic-batch");
    let vfs = PosixVfs::new();
    let db = Storage::create(&vfs, &path, 8, Durability::Full, fill(13)).unwrap();
    one_batch_rules(&db);
    let synced = common::file_len(&wal);
    apply(
        &db,
        &[(b"c", Some(b"3")), (b"d", Some(b"4"))],
        Durability::None,
    );
    drop(db);
    common::truncate_file(&wal, synced);
    let db = Storage::open(&vfs, &path, 8, Durability::Full).unwrap();
    let map = records(&db);
    assert_eq!(map.get(b"a".as_slice()).map(Vec::as_slice), Some(&b"2"[..]));
    assert_eq!(map.get(b"b".as_slice()).map(Vec::as_slice), Some(&b"2"[..]));
    assert!(!map.contains_key(b"c".as_slice()));
    assert!(!map.contains_key(b"d".as_slice()));
}

#[cfg(unix)]
#[test]
fn posix_atomic_write_sync_survives_crash() {
    let (_dir, path, _wal) = common::posix_paths("atomic");
    let vfs = PosixVfs::new();
    let db = Storage::create(&vfs, &path, 8, Durability::Full, fill(10)).unwrap();
    atomic_sync(&db);
    drop(db);
    let db = Storage::open(&vfs, &path, 8, Durability::None).unwrap();
    assert_eq!(
        records(&db).get(b"a".as_slice()).map(Vec::as_slice),
        Some(&b"1"[..])
    );
}

#[cfg(unix)]
#[test]
fn posix_close_rejects_later_calls() {
    let (_dir, path, _wal) = common::posix_paths("shut");
    let vfs = PosixVfs::new();
    let db = Storage::create(&vfs, &path, 2, Durability::Full, fill(11)).unwrap();
    close_rejects(&db);
}

fn writer_and_checkpoint_are_busy(db: &Storage<impl pizzlydb_storage::Vfs>) {
    let batch = db.begin_write().unwrap();
    assert!(matches!(db.begin_write(), Err(StorageError::Busy)));
    assert!(matches!(db.checkpoint(), Err(StorageError::Busy)));
    drop(batch);
    db.checkpoint().unwrap();
}

fn drop_and_reject(db: &Storage<impl pizzlydb_storage::Vfs>) {
    let mut batch = db.begin_write().unwrap();
    batch.put(b"c", b"3").unwrap();
    drop(batch);
    assert!(records(db).is_empty());

    let mut batch = db.begin_write().unwrap();
    let too_long = vec![0u8; 257];
    assert!(matches!(
        batch.put(&too_long, b"x"),
        Err(StorageError::Full)
    ));
    drop(batch);
}

fn one_batch_rules(db: &Storage<impl pizzlydb_storage::Vfs>) {
    let mut batch = db.begin_write().unwrap();
    batch.put(b"a", b"1").unwrap();
    batch.put(b"a", b"2").unwrap();
    batch.put(b"b", b"2").unwrap();
    batch.put(b"gone", b"x").unwrap();
    batch.delete(b"gone").unwrap();
    batch.commit(Durability::Full).unwrap();

    let map = records(db);
    assert_eq!(map.get(b"a".as_slice()).map(Vec::as_slice), Some(&b"2"[..]));
    assert_eq!(map.get(b"b".as_slice()).map(Vec::as_slice), Some(&b"2"[..]));
    assert!(!map.contains_key(b"gone".as_slice()));
}

fn atomic_sync(db: &Storage<impl pizzlydb_storage::Vfs>) {
    let mut batch = db.begin_write().unwrap();
    batch.put(b"a", b"1").unwrap();
    db.atomic_write_sync(batch).unwrap();
}

fn close_rejects(db: &Storage<impl pizzlydb_storage::Vfs>) {
    apply(db, &[(b"a", Some(b"1"))], Durability::Full);
    let view = db.snapshot().unwrap();
    let mut batch = db.begin_write().unwrap();
    batch.put(b"b", b"2").unwrap();
    db.close().unwrap();
    assert!(matches!(view.get(b"a"), Err(StorageError::Closed)));
    assert!(matches!(
        batch.commit(Durability::Full),
        Err(StorageError::Closed)
    ));
    assert!(matches!(db.snapshot(), Err(StorageError::Closed)));
    assert!(matches!(db.begin_write(), Err(StorageError::Closed)));
    assert!(matches!(db.checkpoint(), Err(StorageError::Closed)));
    assert!(matches!(db.close(), Err(StorageError::Closed)));
}
