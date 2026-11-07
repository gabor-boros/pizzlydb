# pizzlydb-txn

Optimistic transactions, commit sequencing, and the logical mutation log.

## Purpose

`pizzlydb-txn` decides which transactions committed. A transaction begins at the latest committed storage snapshot. Its reads observe that snapshot plus its own writes. Commit validates the read set, assigns one `CommitSeq`, and writes the keys, the mutation log, and the commit record in one storage batch. The durability wait happens after the commit lock is released.

Read-only transactions are not validated. A single-shot transaction runs inside the commit lock and is not validated.

The crate depends on `pizzlydb-core` and `pizzlydb-storage`.

## Usage

```rust
use std::path::Path;

use pizzlydb_storage::{Durability, MemoryVfs, Storage};
use pizzlydb_txn::{TxnManager, TxnOptions};

let vfs = MemoryVfs::new();
let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1)).unwrap();

let manager = TxnManager::attach(&storage).unwrap();
let mut txn = manager.begin(TxnOptions::default()).unwrap();
txn.put(b"a", b"1").unwrap();

let seq = txn.commit(&[]).unwrap().unwrap();
assert_eq!(seq.get(), 1);
```

## Modules

- `manager` — `TxnManager`, `Txn`, limits, floors, and the commit pipeline
- `log` — chunked logical mutation log
- `record` — versioned commit record
- `readset` — point and range read sets
- `keys` — system key space
- `error` — retryable and permanent transaction errors

## License

Licensed under the [Apache License 2.0](../../LICENSE).
