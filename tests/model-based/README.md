# Isolation model tests

The history checker lives in `pizzlydb-testkit` (`find_anomaly`). Hand-built anomaly histories are the unit tests in that module.

Engine histories run in:

- `crates/pizzlydb-txn/tests/protocol.rs` for key-level lost update, write skew, phantoms, and validation disabled
- `crates/pizzlydb-graph/tests/transactions.rs` for unique-index races and concurrent edge creation

`PIZZLYDB_COMMIT_SEQS` raises the crash-restart commit count. `PIZZLYDB_STRESS_TXNS` raises the bank-transfer count per worker. The nightly workflow can set both.
