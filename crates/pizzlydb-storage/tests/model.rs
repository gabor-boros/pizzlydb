//! Key/value state machine. Case count is `PIZZLYDB_PROPTEST_CASES`, or 64.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use pizzlydb_storage::{CrashSimConfig, CrashSimVfs, Durability, MemoryVfs, PersistMode, Storage};

fn cases() -> u32 {
    std::env::var("PIZZLYDB_PROPTEST_CASES")
        .ok()
        .and_then(|text| text.parse().ok())
        .filter(|count| *count > 0)
        .unwrap_or(64)
}

fn step(seed: u64, index: u32) -> (u8, u8, bool) {
    let mixed = seed
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(u64::from(index));
    (
        (mixed & 0xff) as u8,
        ((mixed >> 8) & 0xff) as u8,
        (mixed >> 16) & 1 == 0,
    )
}

#[test]
fn seeded_history_matches_a_btreemap() {
    let seed = std::env::var("PIZZLYDB_SEED")
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(0x51A1);
    let vfs = MemoryVfs::new();
    let db = Storage::create(&vfs, Path::new("model"), 8, Durability::None, |id| {
        id.fill(7);
    })
    .unwrap();
    let mut map = BTreeMap::new();
    for index in 0..cases() {
        let (key, value, write) = step(seed, index);
        let mut batch = db.begin_write().unwrap();
        if write {
            batch.put(&[key], &[value]).unwrap();
            map.insert(key, value);
        } else {
            batch.delete(&[key]).unwrap();
            map.remove(&key);
        }
        batch.commit(Durability::None).unwrap();
    }
    let view = db.snapshot().unwrap();
    for key in 0..=255_u8 {
        let stored = view.get(&[key]).unwrap().map(|bytes| bytes[0]);
        assert_eq!(stored, map.get(&key).copied());
    }
}

#[test]
fn a_full_commit_survives_one_crash_point() {
    let config = CrashSimConfig {
        persist: PersistMode::DropUnsynced,
        ..CrashSimConfig::default()
    };
    let vfs = CrashSimVfs::new(config);
    let db = Storage::create(&vfs, Path::new("crash-model"), 8, Durability::Full, |id| {
        id.fill(8);
    })
    .unwrap();
    let mut batch = db.begin_write().unwrap();
    batch.put(b"a", b"1").unwrap();
    batch.commit(Durability::Full).unwrap();
    drop(db);
    vfs.crash(1);
    let db = Storage::open(&vfs, Path::new("crash-model"), 8, Durability::Full).unwrap();
    assert_eq!(
        db.snapshot().unwrap().get(b"a").unwrap().as_deref(),
        Some(&b"1"[..])
    );
}

#[test]
fn a_deferred_commit_is_absent_until_sync() {
    let vfs = MemoryVfs::new();
    let path = Path::new("defer");
    let durability = Durability::Deferred {
        flush_interval: Duration::from_secs(60),
    };
    let db = Storage::create(&vfs, path, 4, durability, |id| id.fill(9)).unwrap();
    let mut batch = db.begin_write().unwrap();
    batch.put(b"k", b"v").unwrap();
    batch.commit(durability).unwrap();
    drop(db);
    vfs.crash();
    let db = Storage::open(&vfs, path, 4, Durability::Full).unwrap();
    assert!(db.snapshot().unwrap().get(b"k").unwrap().is_none());
    drop(db);

    let db = Storage::create(&vfs, Path::new("defer-sync"), 4, durability, |id| {
        id.fill(9)
    })
    .unwrap();
    let mut batch = db.begin_write().unwrap();
    batch.put(b"k", b"v").unwrap();
    batch.commit(durability).unwrap();
    db.sync().unwrap();
    drop(db);
    vfs.crash();
    let db = Storage::open(&vfs, Path::new("defer-sync"), 4, Durability::Full).unwrap();
    assert_eq!(
        db.snapshot().unwrap().get(b"k").unwrap().as_deref(),
        Some(&b"v"[..])
    );
}
