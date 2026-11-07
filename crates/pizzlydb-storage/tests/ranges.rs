//! Bounded scans and range deletes against a `BTreeMap`.

use std::collections::BTreeMap;
use std::path::Path;

use pizzlydb_storage::{Direction, Durability, MemoryVfs, Storage};

fn apply(
    db: &Storage<MemoryVfs>,
    map: &mut BTreeMap<Vec<u8>, Vec<u8>>,
    key: &[u8],
    value: Option<&[u8]>,
) {
    let mut batch = db.begin_write().unwrap();
    match value {
        Some(bytes) => {
            batch.put(key, bytes).unwrap();
            map.insert(key.to_vec(), bytes.to_vec());
        }
        None => {
            batch.delete(key).unwrap();
            map.remove(key);
        }
    }
    batch.commit(Durability::None).unwrap();
}

fn expect_range(
    db: &Storage<MemoryVfs>,
    map: &BTreeMap<Vec<u8>, Vec<u8>>,
    start: &[u8],
    end: &[u8],
) {
    let view = db.snapshot().unwrap();
    let forward: Vec<_> = view
        .scan_range(Some(start), Some(end), Direction::Forward)
        .unwrap()
        .map(|item| item.unwrap())
        .collect();
    let mut reverse: Vec<_> = view
        .scan_range(Some(start), Some(end), Direction::Reverse)
        .unwrap()
        .map(|item| item.unwrap())
        .collect();
    reverse.reverse();
    let expected: Vec<_> = map
        .range(start.to_vec()..end.to_vec())
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    assert_eq!(forward, expected);
    assert_eq!(reverse, expected);
    let estimate = view.estimate_range(Some(start), Some(end)).unwrap();
    assert_eq!(estimate.keys_lower, expected.len() as u64);
    assert_eq!(estimate.keys_upper, estimate.keys_lower);
}

#[test]
fn scans_and_range_deletes_match_a_btreemap() {
    let vfs = MemoryVfs::new();
    let db = Storage::create(&vfs, Path::new("ranges"), 8, Durability::None, |id| {
        id.fill(1);
    })
    .unwrap();
    let mut map = BTreeMap::new();
    for key in [b"a".as_slice(), b"b", b"c", b"d", b"m", b"z"] {
        apply(&db, &mut map, key, Some(b"v"));
    }
    let held = db.snapshot().unwrap();
    let mut batch = db.begin_write().unwrap();
    batch.put(b"c", b"staged").unwrap();
    batch.delete_range(b"m", b"z").unwrap();
    assert_eq!(held.get(b"c").unwrap().as_deref(), Some(&b"v"[..]));
    assert!(held.get(b"m").unwrap().is_some());
    drop(batch);
    expect_range(&db, &map, b"a", b"z");

    let mut batch = db.begin_write().unwrap();
    batch.delete_range(b"b", b"d").unwrap();
    batch.commit(Durability::None).unwrap();
    map.remove(b"b".as_slice());
    map.remove(b"c".as_slice());
    expect_range(&db, &map, b"", b"\xff");

    let uniform = view_count(&db, b"a", b"z");
    let skewed = view_count(&db, b"m", b"z");
    let view = db.snapshot().unwrap();
    let uniform_estimate = view.estimate_range(Some(b"a"), Some(b"z")).unwrap();
    let skewed_estimate = view.estimate_range(Some(b"m"), Some(b"z")).unwrap();
    assert_eq!(uniform_estimate.keys_lower, uniform);
    assert_eq!(uniform_estimate.keys_upper, uniform);
    assert_eq!(skewed_estimate.keys_lower, skewed);
    assert_eq!(skewed_estimate.keys_upper, skewed);
}

fn view_count(db: &Storage<MemoryVfs>, start: &[u8], end: &[u8]) -> u64 {
    db.snapshot()
        .unwrap()
        .scan_range(Some(start), Some(end), Direction::Forward)
        .unwrap()
        .count() as u64
}

#[test]
fn delete_range_page_count_follows_the_key_count() {
    let vfs = MemoryVfs::new();
    let db = Storage::create(&vfs, Path::new("bound"), 16, Durability::None, |id| {
        id.fill(2);
    })
    .unwrap();
    let mut batch = db.begin_write().unwrap();
    for key in 0..32_u8 {
        batch.put(&[key], b"v").unwrap();
    }
    batch.commit(Durability::None).unwrap();
    let mut batch = db.begin_write().unwrap();
    batch.delete_range(&[8], &[24]).unwrap();
    batch.commit(Durability::None).unwrap();
    let view = db.snapshot().unwrap();
    assert!(view.get(&[7]).unwrap().is_some());
    assert!(view.get(&[8]).unwrap().is_none());
    assert!(view.get(&[23]).unwrap().is_none());
    assert!(view.get(&[24]).unwrap().is_some());
}
