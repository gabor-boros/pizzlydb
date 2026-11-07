//! Durability across a crash and torn log prefixes.
//!
//! These tests use [`MemoryVfs`], whose crash drops every unsynced byte and
//! keeps every synced byte. [`CrashSimVfs`](pizzlydb_storage::CrashSimVfs) is
//! the hostile model: partial writeback, reordering, torn sectors, and
//! directory operations lost before `sync_dir`. The all-or-nothing prefixes
//! in this file stay on `MemoryVfs` because they assert an exact committed
//! prefix. Partial persistence is covered by `crash_sim.rs`. A crash during
//! `Wal::prune` can lose a `Durability::Full` commit under that model; issue
//! 012 is the fix.

#[path = "common/mod.rs"]
mod common;

use std::path::Path;

#[cfg(unix)]
use pizzlydb_storage::PosixVfs;
use pizzlydb_storage::{
    Durability, Fault, FaultInjecting, FaultKind, FileRole, MemoryVfs, Storage, StorageError,
};

use common::{apply, assert_committed_prefix, fill, read_memory, records, three_batches, wal_path};

#[test]
fn memory_full_survives_crash_and_none_does_not() {
    let vfs = MemoryVfs::new();
    let path = Path::new("durable");
    let db = Storage::create(&vfs, path, 8, Durability::Full, fill(1)).unwrap();
    apply(&db, &[(b"a", Some(b"1"))], Durability::Full);
    drop(db);
    vfs.crash();
    let db = Storage::open(&vfs, path, 8, Durability::Full).unwrap();
    assert_eq!(
        records(&db).get(b"a".as_slice()).map(Vec::as_slice),
        Some(&b"1"[..])
    );
    drop(db);

    let db = Storage::open(&vfs, path, 8, Durability::None).unwrap();
    apply(&db, &[(b"b", Some(b"2"))], Durability::None);
    drop(db);
    vfs.crash();
    let db = Storage::open(&vfs, path, 8, Durability::Full).unwrap();
    let map = records(&db);
    assert!(!map.contains_key(b"b".as_slice()));
    assert_eq!(map.get(b"a".as_slice()).map(Vec::as_slice), Some(&b"1"[..]));
}

#[test]
fn memory_overflow_value_survives_crash() {
    let vfs = MemoryVfs::new();
    let path = Path::new("wide");
    let db = Storage::create(&vfs, path, 8, Durability::Full, fill(9)).unwrap();
    let value = vec![0x5Au8; 20_000];
    apply(&db, &[(b"wide", Some(value.as_slice()))], Durability::Full);
    drop(db);
    vfs.crash();
    let db = Storage::open(&vfs, path, 8, Durability::Full).unwrap();
    assert_eq!(
        db.snapshot().unwrap().get(b"wide").unwrap().as_deref(),
        Some(value.as_slice())
    );
}

#[test]
fn memory_every_log_prefix_is_a_committed_batch_prefix() {
    let vfs = MemoryVfs::new();
    let path = Path::new("prefix");
    let db = Storage::create(&vfs, path, 8, Durability::Full, fill(6)).unwrap();
    let history = three_batches(&db);
    drop(db);

    let wal = wal_path(path);
    let bytes = read_memory(&vfs, &wal, FileRole::Wal);
    for len in 0..=bytes.len() {
        vfs.replace(&wal, FileRole::Wal, &bytes[..len]).unwrap();
        match Storage::open(&vfs, path, 8, Durability::Full) {
            Ok(db) => {
                let got = records(&db);
                assert_committed_prefix(&history, &got, len);
                drop(db);
            }
            Err(err) => {
                assert!(len < 64, "prefix {len} failed after a full header: {err:?}");
            }
        }
    }
}

#[test]
fn memory_fault_on_full_sync_closes_and_omits_the_batch() {
    let mut found = false;
    for at in 1..500 {
        let vfs = FaultInjecting::new(
            MemoryVfs::new(),
            [Fault {
                at,
                kind: FaultKind::Error,
            }],
        );
        let db = match Storage::create(&vfs, Path::new("fault"), 8, Durability::Full, fill(7)) {
            Ok(db) => db,
            Err(_) => continue,
        };
        if common::apply_result(&db, &[(b"a", Some(b"1"))], Durability::Full).is_err() {
            drop(db);
            continue;
        }
        match common::apply_result(&db, &[(b"b", Some(b"2"))], Durability::Full) {
            Err(StorageError::Io(_)) => {
                assert!(matches!(db.snapshot(), Err(StorageError::Closed)));
                drop(db);
                vfs.inner().crash();
                let db = Storage::open(vfs.inner(), Path::new("fault"), 8, Durability::Full)
                    .expect("reopen");
                let map = records(&db);
                assert_eq!(map.get(b"a".as_slice()).map(Vec::as_slice), Some(&b"1"[..]));
                assert!(!map.contains_key(b"b".as_slice()));
                found = true;
                break;
            }
            Ok(_) => continue,
            Err(err) => panic!("unexpected fault result at {at}: {err:?}"),
        }
    }
    assert!(found, "no scripted fault landed on the second commit");
}

#[cfg(unix)]
#[test]
fn posix_full_survives_reopen_and_none_dies_when_the_tail_is_cut() {
    let (_dir, path, wal) = common::posix_paths("durable");
    let vfs = PosixVfs::new();
    let db = Storage::create(&vfs, &path, 8, Durability::Full, fill(1)).unwrap();
    apply(&db, &[(b"a", Some(b"1"))], Durability::Full);
    drop(db);
    let db = Storage::open(&vfs, &path, 8, Durability::Full).unwrap();
    assert_eq!(
        records(&db).get(b"a".as_slice()).map(Vec::as_slice),
        Some(&b"1"[..])
    );

    let synced = common::file_len(&wal);
    apply(&db, &[(b"b", Some(b"2"))], Durability::None);
    drop(db);
    common::truncate_file(&wal, synced);
    let db = Storage::open(&vfs, &path, 8, Durability::Full).unwrap();
    let map = records(&db);
    assert!(!map.contains_key(b"b".as_slice()));
    assert_eq!(map.get(b"a".as_slice()).map(Vec::as_slice), Some(&b"1"[..]));
}

#[cfg(unix)]
#[test]
fn posix_overflow_value_survives_reopen() {
    let (_dir, path, _wal) = common::posix_paths("wide");
    let vfs = PosixVfs::new();
    let db = Storage::create(&vfs, &path, 8, Durability::Full, fill(9)).unwrap();
    let value = vec![0x5Au8; 20_000];
    apply(&db, &[(b"wide", Some(value.as_slice()))], Durability::Full);
    drop(db);
    let db = Storage::open(&vfs, &path, 8, Durability::Full).unwrap();
    assert_eq!(
        db.snapshot().unwrap().get(b"wide").unwrap().as_deref(),
        Some(value.as_slice())
    );
}

#[cfg(unix)]
#[test]
fn posix_every_log_prefix_is_a_committed_batch_prefix() {
    let (_dir, path, wal) = common::posix_paths("prefix");
    let vfs = PosixVfs::new();
    let db = Storage::create(&vfs, &path, 8, Durability::Full, fill(6)).unwrap();
    let history = three_batches(&db);
    drop(db);

    let bytes = common::read_bytes(&wal);
    for len in 0..=bytes.len() {
        std::fs::write(&wal, &bytes[..len]).expect("rewrite log prefix");
        match Storage::open(&vfs, &path, 8, Durability::Full) {
            Ok(db) => {
                let got = records(&db);
                assert_committed_prefix(&history, &got, len);
                drop(db);
            }
            Err(err) => {
                assert!(len < 64, "prefix {len} failed after a full header: {err:?}");
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn posix_fault_on_full_sync_closes_and_omits_the_batch() {
    let mut found = false;
    for at in 1..800 {
        let (dir, path, _wal) = common::posix_paths("fault");
        let vfs = FaultInjecting::new(
            PosixVfs::new(),
            [Fault {
                at,
                kind: FaultKind::Error,
            }],
        );
        let db = match Storage::create(&vfs, &path, 8, Durability::Full, fill(7)) {
            Ok(db) => db,
            Err(_) => {
                drop(dir);
                continue;
            }
        };
        if common::apply_result(&db, &[(b"a", Some(b"1"))], Durability::Full).is_err() {
            drop(db);
            drop(dir);
            continue;
        }
        match common::apply_result(&db, &[(b"b", Some(b"2"))], Durability::Full) {
            Err(StorageError::Io(_)) => {
                assert!(matches!(db.snapshot(), Err(StorageError::Closed)));
                drop(db);
                let db =
                    Storage::open(&PosixVfs::new(), &path, 8, Durability::Full).expect("reopen");
                let map = records(&db);
                assert_eq!(map.get(b"a".as_slice()).map(Vec::as_slice), Some(&b"1"[..]));
                assert!(!map.contains_key(b"b".as_slice()));
                drop(db);
                drop(dir);
                found = true;
                break;
            }
            Ok(_) => {
                drop(db);
                drop(dir);
            }
            Err(err) => panic!("unexpected fault result at {at}: {err:?}"),
        }
    }
    assert!(found, "no scripted fault landed on the second commit");
}
