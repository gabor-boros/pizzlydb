//! Commit order, validation, the logical log, and snapshot limits.

use std::path::Path;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use pizzlydb_core::{CommitSeq, GraphMutation, NamespaceId, NodeId};
use pizzlydb_storage::{Durability, MemoryVfs, SnapshotKind, Storage, StorageError};
use pizzlydb_txn::{ConflictReason, TxnError, TxnLimits, TxnManager, TxnOptions};

fn open(durability: Durability) -> (MemoryVfs, TxnManager<MemoryVfs>) {
    let vfs = MemoryVfs::new();
    let storage = Storage::create(&vfs, Path::new("db"), 8, durability, |id| id.fill(1)).unwrap();
    let manager = TxnManager::attach(&storage).unwrap();
    (vfs, manager)
}

fn limits() -> TxnLimits {
    TxnLimits {
        max_lifetime: Duration::from_secs(3600),
        max_idle: Duration::from_secs(3600),
        max_snapshot_age: Duration::from_secs(3600),
        ..TxnLimits::default()
    }
}

#[test]
fn lost_update_write_skew_and_phantom_abort() {
    let (_vfs, manager) = open(Durability::None);
    let options = TxnOptions {
        limits: limits(),
        ..TxnOptions::default()
    };

    let mut first = manager.begin(options.clone()).unwrap();
    let mut second = manager.begin(options.clone()).unwrap();
    assert!(first.get(b"k").unwrap().is_none());
    assert!(second.get(b"k").unwrap().is_none());
    first.put(b"k", b"1").unwrap();
    second.put(b"k", b"2").unwrap();
    first.commit(&[]).unwrap();
    let err = second.commit(&[]).unwrap_err();
    assert!(err.is_retryable());
    assert!(matches!(
        err,
        TxnError::Conflict {
            reason: ConflictReason::ReadWrite
        }
    ));

    let mut left = manager.begin(options.clone()).unwrap();
    left.put(b"x", b"0").unwrap();
    left.put(b"y", b"0").unwrap();
    left.commit(&[]).unwrap();
    let mut left = manager.begin(options.clone()).unwrap();
    let mut right = manager.begin(options.clone()).unwrap();
    assert_eq!(left.get(b"x").unwrap().as_deref(), Some(&b"0"[..]));
    assert_eq!(right.get(b"y").unwrap().as_deref(), Some(&b"0"[..]));
    left.put(b"y", b"1").unwrap();
    right.put(b"x", b"1").unwrap();
    left.commit(&[]).unwrap();
    assert!(right.commit(&[]).unwrap_err().is_retryable());

    let mut scan = manager.begin(options.clone()).unwrap();
    let mut writer = manager.begin(options).unwrap();
    assert!(scan.scan_range(b"p", Some(b"q")).unwrap().is_empty());
    scan.put(b"out", b"1").unwrap();
    writer.put(b"p0", b"1").unwrap();
    writer.commit(&[]).unwrap();
    assert!(scan.commit(&[]).unwrap_err().is_retryable());
}

#[test]
fn read_only_transaction_does_not_conflict() {
    let (_vfs, manager) = open(Durability::None);
    let reader = manager.begin(TxnOptions::read_only()).unwrap();
    assert!(reader.get(b"k").unwrap().is_none());
    let mut writer = manager.begin(TxnOptions::default()).unwrap();
    writer.put(b"k", b"1").unwrap();
    writer.commit(&[]).unwrap();
    assert!(reader.commit(&[]).is_ok());
    let mut reader = manager.begin(TxnOptions::read_only()).unwrap();
    let err = reader.put(b"k", b"2").unwrap_err();
    assert!(matches!(err, TxnError::ReadOnlyViolation));
    assert!(!err.is_retryable());
}

#[test]
fn read_skew_stays_on_the_snapshot() {
    let (_vfs, manager) = open(Durability::None);
    let mut setup = manager.begin(TxnOptions::default()).unwrap();
    setup.put(b"x", b"1").unwrap();
    setup.put(b"y", b"1").unwrap();
    setup.commit(&[]).unwrap();

    let reader = manager.begin(TxnOptions::default()).unwrap();
    assert_eq!(reader.get(b"x").unwrap().as_deref(), Some(&b"1"[..]));
    let mut writer = manager.begin(TxnOptions::default()).unwrap();
    writer.put(b"x", b"2").unwrap();
    writer.put(b"y", b"2").unwrap();
    writer.commit(&[]).unwrap();
    assert_eq!(reader.get(b"y").unwrap().as_deref(), Some(&b"1"[..]));
}

#[test]
fn commit_sequence_survives_crash_and_reopen() {
    let count = std::env::var("PIZZLYDB_COMMIT_SEQS")
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(1_000);
    let vfs = MemoryVfs::new();
    let storage =
        Storage::create(&vfs, Path::new("seq"), 8, Durability::Full, |id| id.fill(3)).unwrap();
    let manager = TxnManager::attach(&storage).unwrap();
    for index in 0..count {
        let mut txn = manager.begin(TxnOptions::default()).unwrap();
        let key = index.to_string();
        txn.put(key.as_bytes(), b"v").unwrap();
        let seq = txn.commit(&[]).unwrap().unwrap();
        assert_eq!(seq.get(), index + 1);
    }
    assert_eq!(manager.frontier().unwrap().get(), count);
    drop(manager);
    drop(storage);
    vfs.crash();
    let storage = Storage::open(&vfs, Path::new("seq"), 8, Durability::Full).unwrap();
    let manager = TxnManager::attach(&storage).unwrap();
    assert_eq!(manager.frontier(), CommitSeq::from_u64(count).ok());
    let log = manager.read_log(None, usize::MAX, usize::MAX).unwrap();
    assert_eq!(log.len() as u64, count);
    for (index, commit) in log.iter().enumerate() {
        assert_eq!(commit.seq.get(), index as u64 + 1);
    }
}

#[test]
fn commit_record_is_atomic_with_the_write() {
    let vfs = MemoryVfs::new();
    let storage = Storage::create(&vfs, Path::new("atom"), 8, Durability::Full, |id| {
        id.fill(4)
    })
    .unwrap();
    let manager = TxnManager::attach(&storage).unwrap();
    let mut txn = manager.begin(TxnOptions::default()).unwrap();
    txn.put(b"a", b"1").unwrap();
    txn.commit(&[]).unwrap();
    drop(manager);
    drop(storage);
    vfs.crash();
    let storage = Storage::open(&vfs, Path::new("atom"), 8, Durability::Full).unwrap();
    let view = storage.snapshot().unwrap();
    assert_eq!(view.get(b"a").unwrap().as_deref(), Some(&b"1"[..]));
    let manager = TxnManager::attach(&storage).unwrap();
    assert_eq!(manager.frontier().unwrap().get(), 1);

    let volatile =
        Storage::create(&vfs, Path::new("vol"), 8, Durability::None, |id| id.fill(5)).unwrap();
    let manager = TxnManager::attach(&volatile).unwrap();
    let mut txn = manager.begin(TxnOptions::default()).unwrap();
    txn.put(b"b", b"2").unwrap();
    txn.commit(&[]).unwrap();
    drop(manager);
    drop(volatile);
    vfs.crash();
    let storage = Storage::open(&vfs, Path::new("vol"), 8, Durability::None).unwrap();
    assert!(storage.snapshot().unwrap().get(b"b").unwrap().is_none());
    let manager = TxnManager::attach(&storage).unwrap();
    assert_eq!(manager.frontier(), None);
}

#[test]
fn log_tail_is_exact_and_truncation_respects_floors() {
    let (_vfs, manager) = open(Durability::None);
    let namespace = NamespaceId::from_u128(1).unwrap();
    let node = NodeId::from_u128(1).unwrap();
    for index in 1..=3 {
        let mut txn = manager.begin(TxnOptions::default()).unwrap();
        txn.put(&[index], &[index]).unwrap();
        txn.commit(&[GraphMutation::NodeCreate { namespace, node }])
            .unwrap();
    }
    let first = manager.read_log(None, 10, usize::MAX).unwrap();
    assert_eq!(first.len(), 3);
    let again = manager
        .read_log(Some(first[0].seq), 10, usize::MAX)
        .unwrap();
    assert_eq!(again.len(), 2);
    assert_eq!(again[0].seq.get(), 2);

    let floor = manager
        .register_floor(true, CommitSeq::from_u64(2).unwrap())
        .unwrap();
    let removed = manager.truncate_log().unwrap();
    assert!(removed >= 1);
    let left = manager.read_log(None, 10, usize::MAX).unwrap();
    assert!(left.iter().all(|commit| commit.seq.get() >= 2));
    manager.release_floor(floor).unwrap();
}

#[test]
fn read_and_write_sets_are_bounded() {
    let (_vfs, manager) = open(Durability::None);
    let options = TxnOptions {
        limits: TxnLimits {
            max_read_set: 1,
            max_write_set: 1,
            ..limits()
        },
        ..TxnOptions::default()
    };
    let txn = manager.begin(options.clone()).unwrap();
    txn.get(b"a").unwrap();
    let err = txn.get(b"b").unwrap_err();
    assert!(matches!(err, TxnError::TransactionTooLarge { .. }));
    assert!(!err.is_retryable());

    let mut txn = manager.begin(options).unwrap();
    txn.put(b"a", b"1").unwrap();
    let err = txn.put(b"b", b"2").unwrap_err();
    assert!(matches!(err, TxnError::TransactionTooLarge { .. }));
}

#[test]
fn evicted_history_aborts_the_old_reader() {
    let (_vfs, manager) = open(Durability::None);
    let options = TxnOptions {
        limits: TxnLimits {
            max_history_bytes: 0,
            ..limits()
        },
        ..TxnOptions::default()
    };
    let mut old = manager.begin(options.clone()).unwrap();
    old.get(b"a").unwrap();
    old.put(b"z", b"1").unwrap();
    for index in 0..4u8 {
        let mut txn = manager.begin(options.clone()).unwrap();
        txn.put(&[index], b"0123456789").unwrap();
        txn.commit(&[]).unwrap();
    }
    let err = old.commit(&[]).unwrap_err();
    assert!(matches!(
        err,
        TxnError::Conflict {
            reason: ConflictReason::HistoryEvicted
        }
    ));
}

#[test]
fn snapshot_lease_and_expiry() {
    let vfs = MemoryVfs::new();
    let storage =
        Storage::create(&vfs, Path::new("age"), 8, Durability::None, |id| id.fill(9)).unwrap();
    storage.set_time_ms(0);
    let mut batch = storage.begin_write().unwrap();
    batch.put(b"a", b"1").unwrap();
    batch.commit(Durability::None).unwrap();
    let before = storage.appended_wal_bytes().unwrap();

    let lease = storage
        .lease(SnapshotKind::Query, "page", Duration::from_millis(10))
        .unwrap();
    storage.set_time_ms(9);
    lease.renew(Duration::from_millis(10)).unwrap();
    storage.reap_snapshots();
    assert_eq!(lease.view().get(b"a").unwrap().as_deref(), Some(&b"1"[..]));

    let infos = storage.inspect_snapshots();
    assert!(infos.iter().any(|info| info.tag == "page"));

    storage.set_time_ms(1_000);
    assert!(storage.reap_snapshots() >= 1);
    assert!(matches!(
        lease.view().get(b"a"),
        Err(StorageError::SnapshotTooOld)
    ));
    storage.checkpoint().unwrap();
    let after = storage.appended_wal_bytes().unwrap();
    assert!(after <= before);
}

#[test]
fn bank_transfers_keep_the_total() {
    let transfers = std::env::var("PIZZLYDB_STRESS_TXNS")
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(256);
    let (_vfs, manager) = open(Durability::None);
    let manager = Arc::new(manager);
    let mut setup = manager.begin(TxnOptions::default()).unwrap();
    for account in 0..8u8 {
        setup.put(&[account], &1000i64.to_be_bytes()).unwrap();
    }
    setup.commit(&[]).unwrap();

    let barrier = Arc::new(Barrier::new(8));
    let mut threads = Vec::new();
    for worker in 0..8u8 {
        let manager = Arc::clone(&manager);
        let barrier = Arc::clone(&barrier);
        threads.push(thread::spawn(move || {
            barrier.wait();
            for step in 0..transfers {
                loop {
                    let mut txn = manager.begin(TxnOptions::default()).unwrap();
                    let from = (worker as usize + step) % 8;
                    let to = (from + 1) % 8;
                    let source = i64::from_be_bytes(
                        txn.get(&[from as u8])
                            .unwrap()
                            .unwrap()
                            .as_slice()
                            .try_into()
                            .unwrap(),
                    );
                    let dest = i64::from_be_bytes(
                        txn.get(&[to as u8])
                            .unwrap()
                            .unwrap()
                            .as_slice()
                            .try_into()
                            .unwrap(),
                    );
                    if source < 1 {
                        txn.rollback();
                        break;
                    }
                    txn.put(&[from as u8], &(source - 1).to_be_bytes()).unwrap();
                    txn.put(&[to as u8], &(dest + 1).to_be_bytes()).unwrap();
                    match txn.commit(&[]) {
                        Ok(_) => break,
                        Err(TxnError::Conflict { .. }) => continue,
                        Err(err) => panic!("{err}"),
                    }
                }
            }
        }));
    }
    for thread in threads {
        thread.join().unwrap();
    }
    let txn = manager.begin(TxnOptions::read_only()).unwrap();
    let mut total = 0i64;
    for account in 0..8u8 {
        let value = i64::from_be_bytes(
            txn.get(&[account])
                .unwrap()
                .unwrap()
                .as_slice()
                .try_into()
                .unwrap(),
        );
        total += value;
    }
    assert_eq!(total, 8_000);
}

#[test]
fn disabled_validation_is_visible_to_the_checker() {
    use pizzlydb_testkit::{AnomalyClass, HistOp, HistTxn, find_anomaly};

    let (_vfs, manager) = open(Durability::None);
    let mut seed = manager.begin(TxnOptions::default()).unwrap();
    seed.put(b"x", b"0").unwrap();
    seed.put(b"y", b"0").unwrap();
    let seed_seq = seed.commit(&[]).unwrap().unwrap().get();
    manager.set_validation_enabled(false);
    let mut first = manager.begin(TxnOptions::default()).unwrap();
    let mut second = manager.begin(TxnOptions::default()).unwrap();
    let x = first.get(b"x").unwrap();
    let y = second.get(b"y").unwrap();
    first.put(b"y", b"1").unwrap();
    second.put(b"x", b"1").unwrap();
    let seq1 = first.commit(&[]).unwrap().unwrap().get();
    let seq2 = second.commit(&[]).unwrap().unwrap().get();
    let history = vec![
        HistTxn {
            id: 0,
            ops: vec![
                HistOp::Write {
                    key: b"x".to_vec(),
                    value: b"0".to_vec(),
                },
                HistOp::Write {
                    key: b"y".to_vec(),
                    value: b"0".to_vec(),
                },
            ],
            commit: Some(seed_seq),
            started: 0,
            finished: 0,
        },
        HistTxn {
            id: 1,
            ops: vec![
                HistOp::Read {
                    key: b"x".to_vec(),
                    value: x,
                },
                HistOp::Write {
                    key: b"y".to_vec(),
                    value: b"1".to_vec(),
                },
            ],
            commit: Some(seq1),
            started: 1,
            finished: 1,
        },
        HistTxn {
            id: 2,
            ops: vec![
                HistOp::Read {
                    key: b"y".to_vec(),
                    value: y,
                },
                HistOp::Write {
                    key: b"x".to_vec(),
                    value: b"1".to_vec(),
                },
            ],
            commit: Some(seq2),
            started: 2,
            finished: 2,
        },
    ];
    let cycle = find_anomaly(&history, 11).expect("anomaly");
    assert_eq!(cycle.class, AnomalyClass::G2Item);
    assert_eq!(cycle.seed, 11);
}

#[test]
fn optimistic_commits_overlap_reader_work() {
    use std::sync::Mutex;
    use std::time::Instant;

    let (_vfs, manager) = open(Durability::None);
    let manager = Arc::new(manager);
    let samples = 2u32;
    let workers = 8u32;
    let spin = Duration::from_millis(10);

    let optimistic = Arc::clone(&manager);
    let started = Instant::now();
    let barrier = Arc::new(Barrier::new(workers as usize));
    let mut threads = Vec::new();
    for worker in 0..workers {
        let manager = Arc::clone(&optimistic);
        let barrier = Arc::clone(&barrier);
        threads.push(thread::spawn(move || {
            barrier.wait();
            for step in 0..samples {
                let mut txn = manager.begin(TxnOptions::default()).unwrap();
                let start = Instant::now();
                while start.elapsed() < spin {}
                let key = format!("{worker}-{step}");
                txn.put(key.as_bytes(), b"1").unwrap();
                txn.commit(&[]).unwrap();
            }
        }));
    }
    for thread in threads {
        thread.join().unwrap();
    }
    let optimistic_elapsed = started.elapsed();

    let vfs = MemoryVfs::new();
    let storage = Storage::create(&vfs, Path::new("base"), 8, Durability::None, |id| {
        id.fill(2)
    })
    .unwrap();
    let gate = Arc::new(Mutex::new(()));
    let started = Instant::now();
    let barrier = Arc::new(Barrier::new(workers as usize));
    let mut threads = Vec::new();
    for worker in 0..workers {
        let storage = storage.clone();
        let gate = Arc::clone(&gate);
        let barrier = Arc::clone(&barrier);
        threads.push(thread::spawn(move || {
            barrier.wait();
            for step in 0..samples {
                let _lock = gate.lock().unwrap();
                let start = Instant::now();
                while start.elapsed() < spin {}
                let mut batch = storage.begin_write().unwrap();
                let key = format!("{worker}-{step}");
                batch.put(key.as_bytes(), b"1").unwrap();
                batch.commit(Durability::None).unwrap();
            }
        }));
    }
    for thread in threads {
        thread.join().unwrap();
    }
    let baseline = started.elapsed();
    let ratio = baseline.as_secs_f64() / optimistic_elapsed.as_secs_f64();
    assert!(
        ratio >= 4.0,
        "optimistic {optimistic_elapsed:?} baseline {baseline:?} ratio {ratio}"
    );
}
