//! Batch size limits.

use std::path::Path;

use pizzlydb_storage::{
    BatchLimits, Direction, Durability, LimitKind, MAX_VALUE_LEN, MemoryVfs, Storage, StorageError,
};

#[test]
fn too_many_dirty_pages_leaves_the_database_usable() {
    let vfs = MemoryVfs::new();
    let db = Storage::create(&vfs, Path::new("limits"), 8, Durability::Full, |id| {
        id.fill(1)
    })
    .unwrap();
    db.set_batch_limits(BatchLimits {
        max_dirty_pages: 64,
        ..BatchLimits::default()
    });
    let mut batch = db.begin_write().unwrap();
    let value = vec![7u8; MAX_VALUE_LEN];
    batch.put(b"a", &value).unwrap();
    let err = batch.commit(Durability::Full).unwrap_err();
    assert!(matches!(
        err,
        StorageError::TransactionTooLarge {
            limit: LimitKind::DirtyPages,
            observed,
        } if observed > 64
    ));
    db.set_batch_limits(BatchLimits::default());
    let mut batch = db.begin_write().unwrap();
    batch.put(b"a", b"1").unwrap();
    batch.commit(Durability::None).unwrap();
    assert_eq!(
        db.snapshot().unwrap().get(b"a").unwrap().as_deref(),
        Some(b"1".as_slice())
    );
}

#[test]
fn scan_range_is_bounded_in_both_directions() {
    let vfs = MemoryVfs::new();
    let db = Storage::create(&vfs, Path::new("range"), 8, Durability::None, |id| {
        id.fill(2)
    })
    .unwrap();
    let mut batch = db.begin_write().unwrap();
    for key in [b"a".as_slice(), b"b", b"c", b"d"] {
        batch.put(key, key).unwrap();
    }
    batch.delete_range(b"b", b"d").unwrap();
    batch.commit(Durability::None).unwrap();
    let view = db.snapshot().unwrap();
    let forward: Vec<_> = view
        .scan_range(Some(b"a"), Some(b"c"), Direction::Forward)
        .unwrap()
        .map(|item| item.unwrap().0)
        .collect();
    assert_eq!(forward, vec![b"a".to_vec()]);
    let reverse: Vec<_> = view
        .scan_range(None, None, Direction::Reverse)
        .unwrap()
        .map(|item| item.unwrap().0)
        .collect();
    assert_eq!(reverse, vec![b"d".to_vec(), b"a".to_vec()]);
    let estimate = view.estimate_range(None, None).unwrap();
    assert_eq!(estimate.keys_lower, estimate.keys_upper);
    assert_eq!(estimate.keys_lower, 2);
}

#[test]
fn storage_and_views_are_send_and_sync() {
    fn assert_sync<T: Send + Sync>() {}
    fn assert_ref<T: Send + Sync>(_: &T) {}
    assert_sync::<Storage<MemoryVfs>>();
    assert_sync::<MemoryVfs>();
    let vfs = MemoryVfs::new();
    let db = Storage::create(&vfs, Path::new("bounds"), 4, Durability::None, |id| {
        id.fill(1)
    })
    .unwrap();
    let view = db.snapshot().unwrap();
    assert_ref(&view);
    #[cfg(unix)]
    assert_sync::<pizzlydb_storage::PosixVfs>();
    #[cfg(unix)]
    assert_sync::<Storage<pizzlydb_storage::PosixVfs>>();
}
