//! Readers share a database with one writer.

use std::path::Path;
use std::sync::{Arc, Barrier};
use std::thread;

use pizzlydb_storage::{Durability, MemoryVfs, Storage};

#[test]
fn readers_see_committed_batches() {
    let vfs = MemoryVfs::new();
    let db = Arc::new(
        Storage::create(&vfs, Path::new("shared"), 32, Durability::Full, |id| {
            id.fill(3)
        })
        .unwrap(),
    );
    let barrier = Arc::new(Barrier::new(5));
    let mut readers = Vec::new();
    for _ in 0..4 {
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
    let writer = {
        let db = Arc::clone(&db);
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || {
            barrier.wait();
            for index in 0..100u64 {
                let mut batch = db.begin_write().expect("batch");
                batch.put(b"k", &index.to_be_bytes()).expect("put");
                batch.commit(Durability::Full).expect("commit");
            }
        })
    };
    writer.join().expect("writer");
    for reader in readers {
        reader.join().expect("reader");
    }
    let view = db.snapshot().unwrap();
    assert_eq!(
        view.get(b"k").unwrap().as_deref(),
        Some(99u64.to_be_bytes().as_slice())
    );
}
