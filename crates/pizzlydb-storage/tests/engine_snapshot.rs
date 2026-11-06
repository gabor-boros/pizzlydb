//! Snapshot sequence stability and scan order.

#[path = "common/mod.rs"]
mod common;

use std::path::Path;

#[cfg(unix)]
use pizzlydb_storage::PosixVfs;
use pizzlydb_storage::{Durability, MemoryVfs, Storage, Vfs};

use common::{apply, fill, scan_view};

#[test]
fn memory_snapshot_keeps_the_sequence_from_before_commit() {
    let vfs = MemoryVfs::new();
    let db = Storage::create(&vfs, Path::new("snap"), 8, Durability::Full, fill(2)).unwrap();
    commit_hides_from_older_view(&db);
}

#[test]
fn memory_snapshot_is_unchanged_by_checkpoint() {
    let vfs = MemoryVfs::new();
    let db = Storage::create(&vfs, Path::new("snap-check"), 8, Durability::Full, fill(14)).unwrap();
    checkpoint_does_not_move_the_view(&db);
}

#[test]
fn memory_two_views_stay_behind_a_third() {
    let vfs = MemoryVfs::new();
    let db = Storage::create(&vfs, Path::new("views"), 8, Durability::Full, fill(15)).unwrap();
    two_views_stay_behind(&db);
}

#[test]
fn memory_scan_is_unsigned_byte_order() {
    let vfs = MemoryVfs::new();
    let db = Storage::create(&vfs, Path::new("order"), 8, Durability::Full, fill(8)).unwrap();
    scan_order(&db);
}

#[cfg(unix)]
#[test]
fn posix_snapshot_keeps_the_sequence_from_before_commit() {
    let (_dir, path, _wal) = common::posix_paths("snap");
    let vfs = PosixVfs::new();
    let db = Storage::create(&vfs, &path, 8, Durability::Full, fill(2)).unwrap();
    commit_hides_from_older_view(&db);
}

#[cfg(unix)]
#[test]
fn posix_snapshot_is_unchanged_by_checkpoint() {
    let (_dir, path, _wal) = common::posix_paths("snap-check");
    let vfs = PosixVfs::new();
    let db = Storage::create(&vfs, &path, 8, Durability::Full, fill(14)).unwrap();
    checkpoint_does_not_move_the_view(&db);
}

#[cfg(unix)]
#[test]
fn posix_two_views_stay_behind_a_third() {
    let (_dir, path, _wal) = common::posix_paths("views");
    let vfs = PosixVfs::new();
    let db = Storage::create(&vfs, &path, 8, Durability::Full, fill(15)).unwrap();
    two_views_stay_behind(&db);
}

#[cfg(unix)]
#[test]
fn posix_scan_is_unsigned_byte_order() {
    let (_dir, path, _wal) = common::posix_paths("order");
    let vfs = PosixVfs::new();
    let db = Storage::create(&vfs, &path, 8, Durability::Full, fill(8)).unwrap();
    scan_order(&db);
}

fn commit_hides_from_older_view(db: &Storage<impl Vfs>) {
    apply(db, &[(b"a", Some(b"1"))], Durability::Full);
    let before = db.snapshot().unwrap();
    let mut batch = db.begin_write().unwrap();
    batch.delete(b"a").unwrap();
    batch.put(b"b", b"2").unwrap();
    batch.commit(Durability::Full).unwrap();

    assert_eq!(before.get(b"a").unwrap().as_deref(), Some(&b"1"[..]));
    assert!(before.get(b"b").unwrap().is_none());

    let after = db.snapshot().unwrap();
    assert!(after.get(b"a").unwrap().is_none());
    assert_eq!(after.get(b"b").unwrap().as_deref(), Some(&b"2"[..]));
}

fn checkpoint_does_not_move_the_view(db: &Storage<impl Vfs>) {
    apply(db, &[(b"a", Some(b"1"))], Durability::Full);
    let view = db.snapshot().unwrap();
    let sequence = view.sequence();
    db.checkpoint().unwrap();
    assert_eq!(view.sequence(), sequence);
    assert_eq!(view.get(b"a").unwrap().as_deref(), Some(&b"1"[..]));
    assert_eq!(
        scan_view(&view).get(b"a".as_slice()).map(Vec::as_slice),
        Some(&b"1"[..])
    );
}

fn two_views_stay_behind(db: &Storage<impl Vfs>) {
    apply(db, &[(b"a", Some(b"1"))], Durability::Full);
    let first = db.snapshot().unwrap();
    let second = db.snapshot().unwrap();
    let mut batch = db.begin_write().unwrap();
    batch.delete(b"a").unwrap();
    batch.put(b"b", b"2").unwrap();
    batch.commit(Durability::Full).unwrap();

    for view in [&first, &second] {
        assert_eq!(view.get(b"a").unwrap().as_deref(), Some(&b"1"[..]));
        assert!(view.get(b"b").unwrap().is_none());
    }

    let third = db.snapshot().unwrap();
    assert!(third.get(b"a").unwrap().is_none());
    assert_eq!(third.get(b"b").unwrap().as_deref(), Some(&b"2"[..]));
}

fn scan_order(db: &Storage<impl Vfs>) {
    apply(
        db,
        &[
            (b"b", Some(b"2")),
            (b"\xff", Some(b"3")),
            (b"", Some(b"0")),
            (b"a", Some(b"1")),
        ],
        Durability::Full,
    );
    let view = db.snapshot().unwrap();
    let keys: Vec<Vec<u8>> = view
        .scan(Some(b"a"))
        .unwrap()
        .map(|item| item.unwrap().0)
        .collect();
    assert_eq!(keys, vec![b"a".to_vec(), b"b".to_vec(), b"\xff".to_vec()]);

    let keys: Vec<Vec<u8>> = view
        .scan(None)
        .unwrap()
        .map(|item| item.unwrap().0)
        .collect();
    assert_eq!(
        keys,
        vec![b"".to_vec(), b"a".to_vec(), b"b".to_vec(), b"\xff".to_vec()]
    );
}
