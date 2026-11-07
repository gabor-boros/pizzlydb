//! CrashSimVfs against the page engine.
//!
//! `engine_crash.rs` keeps the all-or-nothing model. This file crashes after
//! every mutating file operation and keeps a seeded subset of unsynced bytes.

use std::path::Path;

use pizzlydb_storage::{
    CrashSimConfig, CrashSimVfs, Durability, PersistMode, Storage, StorageError, for_each_crash,
};

struct Ack {
    op: u64,
    key: &'static [u8],
    value: &'static [u8],
}

fn commit(
    db: &Storage<CrashSimVfs>,
    vfs: &CrashSimVfs,
    key: &'static [u8],
    value: &'static [u8],
    acks: &mut Vec<Ack>,
) {
    let mut batch = db.begin_write().unwrap();
    batch.put(key, value).unwrap();
    batch.commit(Durability::Full).unwrap();
    acks.push(Ack {
        op: vfs.operation_count(),
        key,
        value,
    });
}

#[test]
fn wal_reset_does_not_lose_a_full_commit() {
    let config = CrashSimConfig {
        torn_probability: 0.25,
        persist: PersistMode::Random,
        ..CrashSimConfig::default()
    };
    let acks = std::cell::RefCell::new(Vec::new());
    let mut lost = false;
    let mut durable_loss = false;
    for_each_crash(
        config,
        crash_samples(),
        |vfs| {
            let db = Storage::create(vfs, Path::new("db"), 4, Durability::Full, |id| id.fill(9))
                .unwrap();
            commit(&db, vfs, b"a", b"1", &mut acks.borrow_mut());
            let held = db.snapshot().unwrap();
            commit(&db, vfs, b"b", b"2", &mut acks.borrow_mut());
            db.checkpoint().unwrap();
            drop(held);
            drop(db);
        },
        |op, _seed, vfs| {
            let expected: Vec<(&[u8], &[u8])> = acks
                .borrow()
                .iter()
                .filter(|ack| ack.op <= op)
                .map(|ack| (ack.key, ack.value))
                .collect();
            if expected.is_empty() {
                return;
            }
            let opened = Storage::open(vfs, Path::new("db"), 4, Durability::Full);
            match opened {
                Err(StorageError::Corrupt) => {
                    lost = true;
                    durable_loss = true;
                }
                Err(StorageError::Io(_)) => lost = true,
                Err(other) => panic!("unexpected open error: {other}"),
                Ok(db) => {
                    let view = db.snapshot().expect("snapshot after open");
                    let missing = expected.iter().any(|(key, value)| {
                        view.get(key).expect("point read").as_deref() != Some(*value)
                    });
                    if missing {
                        lost = true;
                        durable_loss = true;
                    }
                }
            }
        },
    );
    assert!(
        !lost,
        "a Durability::Full commit was lost or the store would not open"
    );
    let _ = durable_loss;
}

/// Persistence samples per crash point.
///
/// Pull requests use 4. The nightly workflow sets `PIZZLYDB_CRASH_SAMPLES`
/// to a larger budget. Issue 012 should invert the assertion above so that
/// no seed loses a `Durability::Full` commit.
fn crash_samples() -> u64 {
    std::env::var("PIZZLYDB_CRASH_SAMPLES")
        .ok()
        .and_then(|text| text.parse().ok())
        .filter(|count| *count > 0)
        .unwrap_or(4)
}
