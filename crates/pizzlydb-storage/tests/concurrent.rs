//! Readers share a database with one writer.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

use pizzlydb_storage::{Durability, MemoryVfs, Storage};

fn commits() -> u64 {
    std::env::var("PIZZLYDB_STRESS")
        .ok()
        .and_then(|text| text.parse().ok())
        .filter(|count| *count > 0)
        .unwrap_or(256)
}

#[test]
fn readers_see_committed_batches() {
    let vfs = MemoryVfs::new();
    let db = Arc::new(
        Storage::create(&vfs, Path::new("shared"), 32, Durability::Full, |id| {
            id.fill(3)
        })
        .unwrap(),
    );
    let readers_n = 16;
    let barrier = Arc::new(Barrier::new(readers_n + 2));
    let mut readers = Vec::new();
    for _ in 0..readers_n {
        let db = Arc::clone(&db);
        let barrier = Arc::clone(&barrier);
        readers.push(thread::spawn(move || {
            barrier.wait();
            for _ in 0..100 {
                let view = db.snapshot().expect("snapshot");
                let mut last = None;
                for item in view.scan(None).expect("scan") {
                    let (key, value) = item.expect("row");
                    if key == b"k" {
                        last = Some(value);
                    }
                }
                if let Some(value) = last {
                    assert_eq!(value.len(), 8);
                }
            }
        }));
    }
    let done = Arc::new(AtomicBool::new(false));
    let writer = {
        let db = Arc::clone(&db);
        let barrier = Arc::clone(&barrier);
        let count = commits();
        thread::spawn(move || {
            barrier.wait();
            for index in 0..count {
                let mut batch = db.begin_write().expect("batch");
                batch.put(b"k", &index.to_be_bytes()).expect("put");
                batch.commit(Durability::Full).expect("commit");
            }
        })
    };
    let checkpoint = {
        let db = Arc::clone(&db);
        let barrier = Arc::clone(&barrier);
        let done = Arc::clone(&done);
        thread::spawn(move || {
            barrier.wait();
            while !done.load(Ordering::Relaxed) {
                let _ = db.checkpoint();
            }
        })
    };
    writer.join().expect("writer");
    done.store(true, Ordering::Relaxed);
    checkpoint.join().expect("checkpoint");
    for reader in readers {
        reader.join().expect("reader");
    }
    let view = db.snapshot().unwrap();
    let last = commits() - 1;
    assert_eq!(
        view.get(b"k").unwrap().as_deref(),
        Some(last.to_be_bytes().as_slice())
    );
}
