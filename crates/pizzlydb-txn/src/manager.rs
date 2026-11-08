//! Optimistic transactions over storage snapshots.
//!
//! A transaction reads one committed snapshot plus its own writes. Commit
//! takes the single commit lock, validates the read set against commits that
//! landed after the snapshot, assigns a [`CommitSeq`](pizzlydb_core::CommitSeq),
//! and writes the keys, the mutation log, and the commit record in one storage
//! batch. The durability wait happens after that lock is released.
//!
//! Read-only transactions are not validated. A single-shot transaction runs
//! inside the commit lock and is not validated, because no other commit can
//! land while it holds the lock.
//!
//! Identifier allocation reserves blocks outside the read set. Two creates in
//! the same namespace do not conflict on a counter key.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant};

use pizzlydb_core::{CommitSeq, GraphMutation, NamespaceId};
use pizzlydb_storage::{
    CommitAck, Direction, Lsn, MAX_VALUE_LEN, ReadView, SnapshotKind, Storage, StorageError, Vfs,
};

use crate::error::{ConflictReason, TxnError, map_storage};
use crate::keys::{
    self, GRAPH_LOGICAL_VERSION, commit_end, commit_key, commit_prefix, floor_end, floor_key,
    floor_prefix, format_key, frontier_key, id_key, log_key, log_prefix, log_prefix_after,
};
use crate::log::{self, CommitLog};
use crate::readset::{ReadSet, WriteHist};
use crate::record::{self, CommitRecord};

const ID_BLOCK: u128 = 1024;
const CLIENT_BIT: u128 = 1 << 127;
const DELETE_CHUNK: usize = 128;
/// Default cap on how far behind the frontier a retention floor may pin the log.
const DEFAULT_MAX_PIN: u64 = 1_000_000;

/// Limits that keep one transaction and the shared history bounded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TxnLimits {
    /// Maximum age of a transaction, from `begin`.
    pub max_lifetime: Duration,
    /// Maximum time between operations.
    pub max_idle: Duration,
    /// Maximum age of the pinned snapshot.
    pub max_snapshot_age: Duration,
    /// Maximum point reads plus ranges.
    pub max_read_set: usize,
    /// Maximum staged keys plus range tombstones.
    pub max_write_set: usize,
    /// Maximum bytes of retained write sets.
    pub max_history_bytes: usize,
}

impl Default for TxnLimits {
    fn default() -> Self {
        Self {
            max_lifetime: Duration::from_secs(60),
            max_idle: Duration::from_secs(30),
            max_snapshot_age: Duration::from_secs(60),
            max_read_set: 100_000,
            max_write_set: 100_000,
            max_history_bytes: 64 * 1024 * 1024,
        }
    }
}

impl TxnLimits {
    fn lifetime_ms(self) -> u64 {
        duration_ms(self.max_lifetime)
    }

    fn idle_ms(self) -> u64 {
        duration_ms(self.max_idle)
    }
}

/// Options for [`TxnManager::begin`].
#[derive(Clone, Debug)]
pub struct TxnOptions {
    /// Reject writes.
    pub read_only: bool,
    /// Accept ids with the high bit set. The caller has already authorized them.
    pub allow_client_ids: bool,
    /// Run inside the commit lock and skip validation.
    pub single_shot: bool,
    /// Snapshot class recorded for retention.
    pub kind: SnapshotKind,
    /// Limits. `None` uses [`TxnLimits::default`].
    pub limits: TxnLimits,
}

impl Default for TxnOptions {
    fn default() -> Self {
        Self {
            read_only: false,
            allow_client_ids: false,
            single_shot: false,
            kind: SnapshotKind::Transaction,
            limits: TxnLimits::default(),
        }
    }
}

impl TxnOptions {
    /// A read-only transaction.
    #[must_use]
    pub fn read_only() -> Self {
        Self {
            read_only: true,
            ..Self::default()
        }
    }
}

/// Counters for commits, aborts, and the last commit's sets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TxnStats {
    /// Commits that received a sequence.
    pub commits: u64,
    /// Aborts caused by validation.
    pub aborts_conflict: u64,
    /// Aborts caused by lifetime, idle time, or a reaped snapshot.
    pub aborts_expired: u64,
    /// Aborts caused by a size limit.
    pub aborts_too_large: u64,
    /// Nanoseconds spent in validation.
    pub validation_ns: u64,
    /// Read-set entries of the last commit attempt.
    pub last_read_set: u64,
    /// Write-set entries of the last commit attempt.
    pub last_write_set: u64,
    /// Bytes retained in the write-set history.
    pub history_bytes: u64,
}

/// Handle of a retention floor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FloorId(u64);

/// Shared transaction state for one storage handle.
pub struct TxnManager<V: Vfs> {
    shared: Arc<Shared<V>>,
}

type UndoLog = Vec<(Vec<u8>, Option<Option<Vec<u8>>>)>;
type Row = (Vec<u8>, Vec<u8>);

struct Shared<V: Vfs> {
    storage: Storage<V>,
    commit: Mutex<()>,
    state: Mutex<DbState>,
    alloc: Mutex<Allocator>,
    cv: Condvar,
    validate: AtomicBool,
    stats: Mutex<TxnStats>,
    next_active: AtomicU64,
    next_txn: AtomicU64,
    next_floor: AtomicU64,
}

struct DbState {
    frontier: Option<CommitSeq>,
    history: VecDeque<HistItem>,
    history_bytes: usize,
    evicted_through: Option<CommitSeq>,
    active: BTreeMap<u64, Option<CommitSeq>>,
    durable_floors: BTreeMap<u64, CommitSeq>,
    ephemeral_floors: BTreeMap<u64, CommitSeq>,
    max_pin: u64,
}

struct HistItem {
    seq: CommitSeq,
    write: WriteHist,
}

struct Allocator {
    lanes: HashMap<([u8; 16], u8), Lane>,
}

struct Lane {
    /// Exclusive end of the reserved block.
    reserved: u128,
    next: u128,
    persisted: u128,
    persisted_next: u128,
    loaded: bool,
}

/// One transaction.
///
/// Dropping it rolls the transaction back. [`Txn::commit`] publishes it.
pub struct Txn<V: Vfs> {
    shared: Arc<Shared<V>>,
    view: ReadView<V>,
    reads: std::cell::RefCell<ReadSet>,
    writes: BTreeMap<Vec<u8>, Option<Vec<u8>>>,
    merges: BTreeMap<Vec<u8>, i64>,
    ranges: Vec<(Vec<u8>, Vec<u8>)>,
    /// Offsets of `u64` commit-sequence stamps inside staged values.
    stamps: BTreeMap<Vec<u8>, usize>,
    undo: Option<UndoLog>,
    read_only: bool,
    allow_client_ids: bool,
    single_shot: bool,
    limits: TxnLimits,
    started_ms: u64,
    last_ms: std::cell::Cell<u64>,
    snapshot_seq: Option<CommitSeq>,
    active_id: Option<u64>,
    closed: bool,
}

enum Prepared<V: Vfs> {
    ReadOnly(Option<CommitSeq>),
    Committed(CommitAck<V>, CommitSeq),
}

impl<V: Vfs + 'static> TxnManager<V> {
    /// Attach the manager for `storage`, creating it on first use.
    ///
    /// The manager is stored on the storage handle. Later calls for the same
    /// handle return the same commit history and id allocator.
    ///
    /// # Errors
    ///
    /// Returns [`TxnError::Corrupt`] when the durable frontier has a gap, and
    /// [`TxnError::Storage`] when the database cannot be read.
    pub fn attach(storage: &Storage<V>) -> Result<Self, TxnError> {
        let mut slot = storage.extensions().lock().expect("extensions");
        if let Some(existing) = slot.as_ref()
            && let Some(weak) = existing.downcast_ref::<Weak<Shared<V>>>()
            && let Some(shared) = weak.upgrade()
        {
            return Ok(Self { shared });
        }
        let shared = Arc::new(Shared::open(storage.clone())?);
        spawn_reaper(&shared);
        *slot = Some(Box::new(Arc::downgrade(&shared)));
        Ok(Self { shared })
    }
}

impl<V: Vfs> TxnManager<V> {
    /// Start a transaction at the latest committed snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`TxnError::Storage`] when the snapshot cannot be opened.
    pub fn begin(&self, options: TxnOptions) -> Result<Txn<V>, TxnError> {
        let _ = self.shared.storage.reap_snapshots();
        let now = self.shared.storage.now_ms();
        let active_id = self.shared.next_active.fetch_add(1, Ordering::Relaxed);
        let snapshot_seq = {
            let mut state = self.shared.state.lock().expect("txn");
            let seq = state.frontier;
            state.active.insert(active_id, seq);
            seq
        };
        let view = self
            .shared
            .storage
            .snapshot_with(
                options.kind,
                format!("txn-{active_id}"),
                Some(options.limits.max_snapshot_age),
            )
            .map_err(map_storage)?;
        Ok(Txn {
            shared: Arc::clone(&self.shared),
            view,
            reads: std::cell::RefCell::new(ReadSet::default()),
            writes: BTreeMap::new(),
            merges: BTreeMap::new(),
            ranges: Vec::new(),
            stamps: BTreeMap::new(),
            undo: None,
            read_only: options.read_only,
            allow_client_ids: options.allow_client_ids,
            single_shot: options.single_shot,
            limits: options.limits,
            started_ms: now,
            last_ms: std::cell::Cell::new(now),
            snapshot_seq,
            active_id: Some(active_id),
            closed: false,
        })
    }

    /// Run `body` inside the commit lock and commit it without validation.
    ///
    /// # Errors
    ///
    /// Returns the error from `body` or from the commit.
    pub fn transact<T, E, F>(
        &self,
        mut options: TxnOptions,
        body: F,
    ) -> Result<(T, Option<CommitSeq>), TxnError>
    where
        F: FnOnce(&mut Txn<V>) -> Result<T, E>,
        TxnError: From<E>,
    {
        options.single_shot = true;
        let _guard = self.shared.commit.lock().expect("txn");
        let mut txn = self.begin(options)?;
        let value = body(&mut txn).map_err(TxnError::from)?;
        let seq = txn.prepare(&[])?;
        drop(_guard);
        match seq {
            Prepared::ReadOnly(seq) => Ok((value, seq)),
            Prepared::Committed(ack, seq) => {
                ack.acknowledge().map_err(map_storage)?;
                Ok((value, Some(seq)))
            }
        }
    }

    /// Turn validation off. The isolation suite uses this to prove the checker finds anomalies.
    pub fn set_validation_enabled(&self, enabled: bool) {
        self.shared.validate.store(enabled, Ordering::Relaxed);
    }

    /// Whether commit validates read sets.
    #[must_use]
    pub fn validation_enabled(&self) -> bool {
        self.shared.validate.load(Ordering::Relaxed)
    }

    /// Copy the counters.
    #[must_use]
    pub fn stats(&self) -> TxnStats {
        let mut stats = *self.shared.stats.lock().expect("txn");
        stats.history_bytes =
            u64::try_from(self.shared.state.lock().expect("txn").history_bytes).unwrap_or(u64::MAX);
        stats
    }

    /// Durable frontier, or [`CommitSeq::ZERO_FRONTIER`] before the first commit.
    #[must_use]
    pub fn frontier(&self) -> Option<CommitSeq> {
        self.shared.state.lock().expect("txn").frontier
    }

    /// Drop snapshots that outlived their limit.
    pub fn reap(&self) -> usize {
        self.shared.storage.reap_snapshots()
    }

    /// Registered snapshots.
    #[must_use]
    pub fn inspect_snapshots(&self) -> Vec<pizzlydb_storage::SnapshotInfo> {
        self.shared.storage.inspect_snapshots()
    }

    /// Block until the frontier moves past `after`, or `timeout` elapses.
    #[must_use]
    pub fn wait_for_commit(&self, after: Option<CommitSeq>, timeout: Duration) -> bool {
        let state = self.shared.state.lock().expect("txn");
        let (guard, result) = self
            .shared
            .cv
            .wait_timeout_while(state, timeout, |state| {
                !frontier_after(state.frontier, after)
            })
            .expect("txn");
        frontier_after(guard.frontier, after) && !result.timed_out()
            || frontier_after(guard.frontier, after)
    }

    /// Read commits strictly after `after`, preserving commit boundaries.
    ///
    /// # Errors
    ///
    /// Returns [`TxnError::Corrupt`] when a chunk does not decode, and
    /// [`TxnError::Storage`] when the scan fails.
    pub fn read_log(
        &self,
        after: Option<CommitSeq>,
        max_records: usize,
        max_bytes: usize,
    ) -> Result<Vec<CommitLog>, TxnError> {
        let view = self.shared.storage.snapshot().map_err(map_storage)?;
        let start = match after {
            Some(seq) => log_prefix_after(seq),
            None => log_prefix(),
        };
        let end = keys::system_key(keys::KIND_LOG.saturating_add(1), &[]);
        let mut grouped: BTreeMap<CommitSeq, Vec<(u32, Vec<GraphMutation>)>> = BTreeMap::new();
        let mut bytes = 0usize;
        let mut records = 0usize;
        for item in view
            .scan_range(Some(&start), Some(&end), Direction::Forward)
            .map_err(map_storage)?
        {
            let (_key, value) = item.map_err(map_storage)?;
            let (seq, index, _count, mutations) = log::decode_chunk(&value)?;
            if after.is_some_and(|after| seq <= after) {
                continue;
            }
            let adding = mutations.len();
            if !grouped.is_empty()
                && (records + adding > max_records || bytes + value.len() > max_bytes)
            {
                break;
            }
            records += adding;
            bytes += value.len();
            grouped.entry(seq).or_default().push((index, mutations));
        }
        let mut out = Vec::new();
        for (seq, chunks) in grouped {
            out.push(CommitLog {
                seq,
                mutations: log::assemble(chunks)?,
            });
        }
        Ok(out)
    }

    /// Register a floor. Durable floors are written to the system key space.
    ///
    /// # Errors
    ///
    /// Returns [`TxnError::Storage`] when a durable floor cannot be written.
    pub fn register_floor(&self, durable: bool, at: CommitSeq) -> Result<FloorId, TxnError> {
        let id = self.shared.next_floor.fetch_add(1, Ordering::Relaxed);
        if durable {
            let _guard = self.shared.commit.lock().expect("txn");
            let mut batch = self.shared.storage.begin_write().map_err(map_storage)?;
            batch
                .put(&floor_key(id), &at.to_be_bytes())
                .map_err(map_storage)?;
            let durability = self.shared.storage.durability();
            batch.commit(durability).map_err(map_storage)?;
            self.shared
                .state
                .lock()
                .expect("txn")
                .durable_floors
                .insert(id, at);
        } else {
            self.shared
                .state
                .lock()
                .expect("txn")
                .ephemeral_floors
                .insert(id, at);
        }
        Ok(FloorId(id))
    }

    /// Move a floor forward. The floor must already be registered.
    ///
    /// # Errors
    ///
    /// Returns [`TxnError::Corrupt`] when `id` is unknown, and
    /// [`TxnError::Storage`] when a durable update fails.
    pub fn update_floor(&self, id: FloorId, at: CommitSeq) -> Result<(), TxnError> {
        let mut state = self.shared.state.lock().expect("txn");
        if state.durable_floors.contains_key(&id.0) {
            drop(state);
            let _guard = self.shared.commit.lock().expect("txn");
            let mut batch = self.shared.storage.begin_write().map_err(map_storage)?;
            batch
                .put(&floor_key(id.0), &at.to_be_bytes())
                .map_err(map_storage)?;
            batch
                .commit(self.shared.storage.durability())
                .map_err(map_storage)?;
            self.shared
                .state
                .lock()
                .expect("txn")
                .durable_floors
                .insert(id.0, at);
            return Ok(());
        }
        if let Some(slot) = state.ephemeral_floors.get_mut(&id.0) {
            *slot = at;
            return Ok(());
        }
        Err(TxnError::Corrupt)
    }

    /// Forget a floor. A durable floor is removed from the database.
    ///
    /// # Errors
    ///
    /// Returns [`TxnError::Storage`] when the delete fails.
    pub fn release_floor(&self, id: FloorId) -> Result<(), TxnError> {
        let mut state = self.shared.state.lock().expect("txn");
        let durable = state.durable_floors.remove(&id.0).is_some();
        state.ephemeral_floors.remove(&id.0);
        drop(state);
        if durable {
            let _guard = self.shared.commit.lock().expect("txn");
            let mut batch = self.shared.storage.begin_write().map_err(map_storage)?;
            batch.delete(&floor_key(id.0)).map_err(map_storage)?;
            batch
                .commit(self.shared.storage.durability())
                .map_err(map_storage)?;
        }
        Ok(())
    }

    /// Delete log chunks below every live floor.
    ///
    /// A floor further than the pin limit behind the frontier does not count.
    /// The call deletes in bounded batches.
    ///
    /// # Errors
    ///
    /// Returns [`TxnError::Storage`] when a delete fails.
    pub fn truncate_log(&self) -> Result<u64, TxnError> {
        let _guard = self.shared.commit.lock().expect("txn");
        let min = {
            let state = self.shared.state.lock().expect("txn");
            retention_min(&state)
        };
        let Some(min) = min else {
            return Ok(0);
        };
        let end = log_key(min, 0);
        let mut removed = 0u64;
        loop {
            let view = self.shared.storage.snapshot().map_err(map_storage)?;
            let mut keys = Vec::new();
            for item in view
                .scan_range(Some(&log_prefix()), Some(&end), Direction::Forward)
                .map_err(map_storage)?
            {
                keys.push(item.map_err(map_storage)?.0);
                if keys.len() == DELETE_CHUNK {
                    break;
                }
            }
            if keys.is_empty() {
                break;
            }
            let count = keys.len() as u64;
            let mut batch = self.shared.storage.begin_write().map_err(map_storage)?;
            for key in keys {
                batch.delete(&key).map_err(map_storage)?;
            }
            batch
                .commit(self.shared.storage.durability())
                .map_err(map_storage)?;
            removed += count;
        }
        Ok(removed)
    }
}

impl<V: Vfs> Txn<V> {
    /// Copy a value. The key is recorded in the read set unless it is an own write.
    ///
    /// # Errors
    ///
    /// Returns [`TxnError::Expired`], [`TxnError::SnapshotTooOld`],
    /// [`TxnError::TransactionTooLarge`], or [`TxnError::Storage`].
    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, TxnError> {
        self.lookup(key, true)
    }

    /// Copy a value without recording a read. System metadata uses this.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::get`] other than a full read set.
    pub fn get_untracked(&self, key: &[u8]) -> Result<Option<Vec<u8>>, TxnError> {
        self.lookup(key, false)
    }

    /// Copy a snapshot value, ignoring staged writes and the read set.
    ///
    /// # Errors
    ///
    /// Returns [`TxnError::Expired`], [`TxnError::SnapshotTooOld`], or
    /// [`TxnError::Storage`].
    pub fn snapshot_get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, TxnError> {
        self.check_open()?;
        self.view.get(key).map_err(map_storage)
    }

    /// Visit staged keys. `None` is a delete.
    pub fn for_each_staged(&self, mut visit: impl FnMut(&[u8], Option<&[u8]>)) {
        for (key, value) in &self.writes {
            visit(key, value.as_deref());
        }
    }

    /// At commit, write this transaction's [`CommitSeq`](pizzlydb_core::CommitSeq)
    /// into the staged value at `key`, as a big-endian `u64` at `offset`.
    ///
    /// # Errors
    ///
    /// Returns [`TxnError::Closed`] or [`TxnError::Expired`].
    pub fn stamp_commit(&mut self, key: &[u8], offset: usize) -> Result<(), TxnError> {
        self.check_open()?;
        self.stamps.insert(key.to_vec(), offset);
        Ok(())
    }

    /// Keys in `[start, end)`, at most `cap` visible rows.
    ///
    /// The whole range is recorded in the read set. `truncated` is set when
    /// another visible row exists past `cap`.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::scan_range`].
    pub fn scan_capped(
        &self,
        start: &[u8],
        end: &[u8],
        cap: usize,
    ) -> Result<(Vec<Row>, bool), TxnError> {
        self.check_open()?;
        self.reads
            .borrow_mut()
            .add_range(start, Some(end), self.limits.max_read_set)?;
        let mut base = self
            .view
            .scan_range(Some(start), Some(end), Direction::Forward)
            .map_err(map_storage)?;
        let staged: Vec<(&Vec<u8>, &Option<Vec<u8>>)> = self
            .writes
            .range(start.to_vec()..)
            .take_while(|(key, _)| key.as_slice() < end)
            .collect();
        let mut rows = Vec::new();
        let mut base_next = next_row(&mut base)?;
        let mut staged_at = 0;
        let mut truncated = false;
        loop {
            if rows.len() > cap {
                truncated = true;
                rows.pop();
                break;
            }
            let base_key = base_next.as_ref().map(|(key, _)| key.as_slice());
            let staged_key = staged.get(staged_at).map(|(key, _)| key.as_slice());
            match (base_key, staged_key) {
                (None, None) => break,
                (Some(_), None) => {
                    if let Some(row) = base_next.take() {
                        rows.push(row);
                    }
                    base_next = next_row(&mut base)?;
                }
                (None, Some(_)) => {
                    if let Some(value) = staged[staged_at].1.as_ref() {
                        rows.push((staged[staged_at].0.clone(), value.clone()));
                    }
                    staged_at += 1;
                }
                (Some(left), Some(right)) => match left.cmp(right) {
                    std::cmp::Ordering::Less => {
                        if let Some(row) = base_next.take() {
                            rows.push(row);
                        }
                        base_next = next_row(&mut base)?;
                    }
                    std::cmp::Ordering::Greater => {
                        if let Some(value) = staged[staged_at].1.as_ref() {
                            rows.push((staged[staged_at].0.clone(), value.clone()));
                        }
                        staged_at += 1;
                    }
                    std::cmp::Ordering::Equal => {
                        base_next = next_row(&mut base)?;
                        if let Some(value) = staged[staged_at].1.as_ref() {
                            rows.push((staged[staged_at].0.clone(), value.clone()));
                        }
                        staged_at += 1;
                    }
                },
            }
        }
        Ok((rows, truncated))
    }

    /// Keys in `[start, end)`, merged with staged writes. The range is recorded.
    ///
    /// `end` of [`None`] reads through the end of the map.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::get`].
    pub fn scan_range(&self, start: &[u8], end: Option<&[u8]>) -> Result<Vec<Row>, TxnError> {
        self.scan(start, end, false)
    }

    /// [`Self::scan_range`] yielded from the last key.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::scan_range`].
    pub fn scan_reverse(&self, start: &[u8], end: Option<&[u8]>) -> Result<Vec<Row>, TxnError> {
        self.scan(start, end, true)
    }

    /// Stage `value` at `key`.
    ///
    /// # Errors
    ///
    /// Returns [`TxnError::ReadOnlyViolation`] on a read-only transaction and
    /// [`TxnError::TransactionTooLarge`] when the write set is full.
    pub fn put(&mut self, key: &[u8], value: &[u8]) -> Result<(), TxnError> {
        self.stage(key.to_vec(), Some(value.to_vec()))
    }

    /// Stage removal of `key`.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::put`].
    pub fn delete(&mut self, key: &[u8]) -> Result<(), TxnError> {
        self.stage(key.to_vec(), None)
    }

    /// Stage removal of `[start, end)` and record the range for validation.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::scan_range`] and [`Self::delete`].
    pub fn delete_range(&mut self, start: &[u8], end: &[u8]) -> Result<(), TxnError> {
        if start >= end {
            return Ok(());
        }
        let keys: Vec<Vec<u8>> = self
            .scan_range(start, Some(end))?
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        for key in keys {
            self.delete(&key)?;
        }
        self.ranges.push((start.to_vec(), end.to_vec()));
        self.check_write_limit(0)?;
        Ok(())
    }

    /// Add `delta` to the signed counter at `key`.
    ///
    /// The update is applied under the commit lock and is not part of validation.
    /// Absent keys start at zero. The stored form is a big-endian `i64`.
    ///
    /// # Errors
    ///
    /// Returns [`TxnError::ReadOnlyViolation`] on a read-only transaction.
    pub fn merge_add(&mut self, key: &[u8], delta: i64) -> Result<(), TxnError> {
        self.check_open()?;
        if self.read_only {
            return Err(TxnError::ReadOnlyViolation);
        }
        let entry = self.merges.entry(key.to_vec()).or_insert(0);
        *entry = entry.saturating_add(delta);
        Ok(())
    }

    /// Reserve the next id for `(namespace, kind)`.
    ///
    /// The counter is not part of the read set. Gaps are allowed. Ids are never
    /// reused. Values stay below the client-id bit.
    ///
    /// # Errors
    ///
    /// Returns [`TxnError::IdExhausted`] when the allocator range is full, and
    /// [`TxnError::Storage`] when the watermark cannot be read.
    pub fn allocate_id(&mut self, namespace: &[u8; 16], kind: u8) -> Result<u128, TxnError> {
        self.check_open()?;
        self.shared
            .alloc
            .lock()
            .expect("txn")
            .allocate(&self.shared.storage, namespace, kind)
    }

    /// Require a client id: the high bit is set and client ids are enabled.
    ///
    /// # Errors
    ///
    /// Returns [`TxnError::ClientIdsDisabled`] or [`TxnError::InvalidClientId`].
    pub fn check_client_id(&self, id: u128) -> Result<(), TxnError> {
        if !self.allow_client_ids {
            return Err(TxnError::ClientIdsDisabled);
        }
        if id & CLIENT_BIT == 0 || id == 0 {
            return Err(TxnError::InvalidClientId);
        }
        Ok(())
    }

    /// `true` when this call started a savepoint. Nested calls join it.
    pub fn begin_savepoint(&mut self) -> bool {
        if self.undo.is_some() {
            return false;
        }
        self.undo = Some(Vec::new());
        true
    }

    /// Drop the savepoint and keep the staged keys.
    pub fn commit_savepoint(&mut self) {
        self.undo = None;
    }

    /// Restore keys staged since [`Self::begin_savepoint`].
    pub fn rollback_savepoint(&mut self) {
        let undo = self.undo.take().unwrap_or_default();
        for (key, previous) in undo.into_iter().rev() {
            match previous {
                Some(entry) => {
                    self.writes.insert(key, entry);
                }
                None => {
                    self.writes.remove(&key);
                }
            }
        }
    }

    /// Number of staged key changes.
    #[must_use]
    pub fn staged_len(&self) -> usize {
        self.writes.len()
    }

    /// Sequence this transaction's snapshot serializes at.
    #[must_use]
    pub fn snapshot_seq(&self) -> Option<CommitSeq> {
        self.snapshot_seq
    }

    /// Publish the transaction and its logical mutations.
    ///
    /// An empty transaction returns the snapshot sequence and writes nothing.
    ///
    /// # Errors
    ///
    /// Returns [`TxnError::Conflict`] when validation fails,
    /// [`TxnError::ReadOnlyViolation`] when a read-only transaction has writes,
    /// and the other commit errors.
    pub fn commit(mut self, mutations: &[GraphMutation]) -> Result<Option<CommitSeq>, TxnError> {
        let shared = Arc::clone(&self.shared);
        let prepared = {
            let _guard = shared.commit.lock().expect("txn");
            self.prepare(mutations)
        };
        match prepared {
            Ok(Prepared::ReadOnly(seq)) => Ok(seq),
            Ok(Prepared::Committed(ack, seq)) => {
                ack.acknowledge().map_err(map_storage)?;
                Ok(Some(seq))
            }
            Err(err) => {
                note_abort(&shared, &err);
                Err(err)
            }
        }
    }

    /// Abandon the transaction.
    pub fn rollback(self) {}

    fn prepare(&mut self, mutations: &[GraphMutation]) -> Result<Prepared<V>, TxnError> {
        self.check_open()?;
        let writing = !self.writes.is_empty()
            || !self.merges.is_empty()
            || !self.ranges.is_empty()
            || !mutations.is_empty();
        if self.read_only && writing {
            return Err(TxnError::ReadOnlyViolation);
        }
        if !writing {
            return Ok(Prepared::ReadOnly(self.snapshot_seq));
        }

        let started = Instant::now();
        if self.shared.validate.load(Ordering::Relaxed) && !self.single_shot {
            self.validate()?;
        }
        let elapsed = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        {
            let mut stats = self.shared.stats.lock().expect("txn");
            stats.validation_ns = stats.validation_ns.saturating_add(elapsed);
        }

        let seq = {
            let state = self.shared.state.lock().expect("txn");
            match state.frontier {
                None => CommitSeq::first(),
                Some(current) => current.next()?,
            }
        };
        let txn_id = self.shared.next_txn.fetch_add(1, Ordering::Relaxed);
        self.apply_merges()?;
        let watermarks = self.shared.alloc.lock().expect("txn").dirty_watermarks();
        let write_hist = self.write_hist();
        let chunks = log::chunk_mutations(seq, mutations, MAX_VALUE_LEN)?;
        let namespaces = namespaces_of(mutations);

        let mut batch = self.shared.storage.begin_write().map_err(map_storage)?;
        for (key, value) in &self.writes {
            match value {
                Some(bytes) => {
                    if let Some(stamped) = self.stamped(key, bytes, seq)? {
                        batch.put(key, &stamped).map_err(map_storage)?;
                    } else {
                        batch.put(key, bytes).map_err(map_storage)?;
                    }
                }
                None => batch.delete(key).map_err(map_storage)?,
            }
        }
        for (key, value) in &watermarks {
            batch.put(key, value).map_err(map_storage)?;
        }
        for (index, chunk) in chunks.iter().enumerate() {
            let index = u32::try_from(index).map_err(|_| TxnError::Corrupt)?;
            batch
                .put(&log_key(seq, index), chunk)
                .map_err(map_storage)?;
        }
        batch
            .put(&frontier_key(), &seq.to_be_bytes())
            .map_err(map_storage)?;
        if self.get_untracked(&format_key())?.is_none() {
            batch
                .put(&format_key(), &GRAPH_LOGICAL_VERSION.to_be_bytes())
                .map_err(map_storage)?;
        }

        let mutation_count = u32::try_from(mutations.len()).unwrap_or(u32::MAX);
        let durability = self.shared.storage.durability();
        let timestamp = record::now_us();
        let ack = batch
            .commit_unacked(durability, move |lsn: Lsn| {
                let record = CommitRecord {
                    seq,
                    lsn: lsn.as_u64(),
                    timestamp_us: timestamp,
                    txn_id: u128::from(txn_id),
                    namespaces: namespaces.clone(),
                    mutation_count,
                    principal: Vec::new(),
                    idempotency_key: Vec::new(),
                };
                let bytes = record::encode_record(&record).map_err(|_| StorageError::Corrupt)?;
                Ok(vec![(commit_key(seq), Some(bytes))])
            })
            .map_err(map_storage)?;

        {
            let mut state = self.shared.state.lock().expect("txn");
            state.frontier = Some(seq);
            state.history_bytes = state.history_bytes.saturating_add(write_hist.bytes());
            state.history.push_back(HistItem {
                seq,
                write: write_hist,
            });
            evict_history(&mut state, self.limits.max_history_bytes);
            let mut stats = self.shared.stats.lock().expect("txn");
            stats.commits = stats.commits.saturating_add(1);
            stats.last_read_set = u64::try_from(self.reads.borrow().len()).unwrap_or(u64::MAX);
            stats.last_write_set =
                u64::try_from(self.writes.len() + self.ranges.len()).unwrap_or(u64::MAX);
            stats.history_bytes = u64::try_from(state.history_bytes).unwrap_or(u64::MAX);
        }
        self.shared.cv.notify_all();
        self.unregister();
        self.closed = true;
        Ok(Prepared::Committed(ack, seq))
    }

    fn validate(&self) -> Result<(), TxnError> {
        let state = self.shared.state.lock().expect("txn");
        if !self.read_only
            && history_gap(
                self.snapshot_seq,
                state.frontier,
                &state.history,
                state.evicted_through,
            )
        {
            return Err(TxnError::Conflict {
                reason: ConflictReason::HistoryEvicted,
            });
        }
        let reads = self.reads.borrow();
        for item in &state.history {
            let after = match self.snapshot_seq {
                None => true,
                Some(seq) => item.seq > seq,
            };
            if after && item.write.conflicts(&reads) {
                return Err(TxnError::Conflict {
                    reason: ConflictReason::ReadWrite,
                });
            }
        }
        Ok(())
    }

    fn apply_merges(&mut self) -> Result<(), TxnError> {
        if self.merges.is_empty() {
            return Ok(());
        }
        let latest = self.shared.storage.snapshot().map_err(map_storage)?;
        let merges = std::mem::take(&mut self.merges);
        for (key, delta) in merges {
            let base = if let Some(staged) = self.writes.get(&key) {
                match staged {
                    Some(bytes) => decode_i64(bytes)?,
                    None => 0,
                }
            } else {
                match latest.get(&key).map_err(map_storage)? {
                    Some(bytes) => decode_i64(&bytes)?,
                    None => 0,
                }
            };
            let next = base.checked_add(delta).ok_or(TxnError::Corrupt)?;
            self.writes.insert(key, Some(next.to_be_bytes().to_vec()));
        }
        Ok(())
    }

    fn write_hist(&self) -> WriteHist {
        let mut points: Vec<Vec<u8>> = self.writes.keys().cloned().collect();
        points.extend(self.merges.keys().cloned());
        points.sort();
        points.dedup();
        WriteHist {
            points,
            ranges: self.ranges.clone(),
        }
    }

    fn lookup(&self, key: &[u8], track: bool) -> Result<Option<Vec<u8>>, TxnError> {
        self.check_open()?;
        if let Some(delta) = self.merges.get(key) {
            let base = self.staged_or_snapshot(key)?;
            let current = base.unwrap_or(0).saturating_add(*delta);
            return Ok(Some(current.to_be_bytes().to_vec()));
        }
        if let Some(staged) = self.writes.get(key) {
            return Ok(staged.clone());
        }
        let value = self.view.get(key).map_err(map_storage)?;
        if track {
            self.reads
                .borrow_mut()
                .add_point(key, self.limits.max_read_set)?;
        }
        Ok(value)
    }

    fn staged_or_snapshot(&self, key: &[u8]) -> Result<Option<i64>, TxnError> {
        if let Some(staged) = self.writes.get(key) {
            return match staged {
                Some(bytes) => Ok(Some(decode_i64(bytes)?)),
                None => Ok(Some(0)),
            };
        }
        match self.view.get(key).map_err(map_storage)? {
            Some(bytes) => Ok(Some(decode_i64(&bytes)?)),
            None => Ok(None),
        }
    }

    fn scan(&self, start: &[u8], end: Option<&[u8]>, reverse: bool) -> Result<Vec<Row>, TxnError> {
        self.check_open()?;
        self.reads
            .borrow_mut()
            .add_range(start, end, self.limits.max_read_set)?;
        let mut base = Vec::new();
        for item in self
            .view
            .scan_range(Some(start), end, Direction::Forward)
            .map_err(map_storage)?
        {
            base.push(item.map_err(map_storage)?);
        }
        let end_bound = end.unwrap_or(&[]);
        let staged: Vec<(&Vec<u8>, &Option<Vec<u8>>)> = self
            .writes
            .range(start.to_vec()..)
            .take_while(|(key, _)| end.is_none_or(|end| key.as_slice() < end))
            .collect();
        let _ = end_bound;
        let mut rows = Vec::new();
        let mut base_at = 0;
        let mut staged_at = 0;
        while base_at < base.len() || staged_at < staged.len() {
            let base_key = base.get(base_at).map(|(key, _)| key.as_slice());
            let staged_key = staged.get(staged_at).map(|(key, _)| key.as_slice());
            match (base_key, staged_key) {
                (None, None) => break,
                (Some(_), None) => {
                    rows.push(base[base_at].clone());
                    base_at += 1;
                }
                (None, Some(_)) => {
                    if let Some(value) = staged[staged_at].1.as_ref() {
                        rows.push((staged[staged_at].0.clone(), value.clone()));
                    }
                    staged_at += 1;
                }
                (Some(left), Some(right)) => match left.cmp(right) {
                    std::cmp::Ordering::Less => {
                        rows.push(base[base_at].clone());
                        base_at += 1;
                    }
                    std::cmp::Ordering::Greater => {
                        if let Some(value) = staged[staged_at].1.as_ref() {
                            rows.push((staged[staged_at].0.clone(), value.clone()));
                        }
                        staged_at += 1;
                    }
                    std::cmp::Ordering::Equal => {
                        base_at += 1;
                        if let Some(value) = staged[staged_at].1.as_ref() {
                            rows.push((staged[staged_at].0.clone(), value.clone()));
                        }
                        staged_at += 1;
                    }
                },
            }
        }
        if reverse {
            rows.reverse();
        }
        let _ = staged;
        Ok(rows)
    }

    fn stage(&mut self, key: Vec<u8>, value: Option<Vec<u8>>) -> Result<(), TxnError> {
        self.check_open()?;
        if self.read_only {
            return Err(TxnError::ReadOnlyViolation);
        }
        self.check_write_limit(usize::from(!self.writes.contains_key(&key)))?;
        if let Some(undo) = &mut self.undo {
            let previous = self.writes.insert(key.clone(), value);
            undo.push((key, previous));
        } else {
            self.writes.insert(key, value);
        }
        Ok(())
    }

    fn check_write_limit(&self, adding: usize) -> Result<(), TxnError> {
        let observed = self.writes.len() + self.merges.len() + self.ranges.len() + adding;
        if observed > self.limits.max_write_set {
            return Err(TxnError::TransactionTooLarge {
                limit: "write set",
                observed: u64::try_from(observed).unwrap_or(u64::MAX),
            });
        }
        Ok(())
    }

    fn check_open(&self) -> Result<(), TxnError> {
        if self.closed {
            return Err(TxnError::Closed);
        }
        if self.view.is_expired() {
            return Err(TxnError::SnapshotTooOld);
        }
        let now = self.shared.storage.now_ms();
        if now.saturating_sub(self.started_ms) > self.limits.lifetime_ms() {
            return Err(TxnError::Expired);
        }
        if now.saturating_sub(self.last_ms.get()) > self.limits.idle_ms() {
            return Err(TxnError::Expired);
        }
        self.last_ms.set(now);
        Ok(())
    }

    fn stamped(
        &self,
        key: &[u8],
        bytes: &[u8],
        seq: CommitSeq,
    ) -> Result<Option<Vec<u8>>, TxnError> {
        let Some(&offset) = self.stamps.get(key) else {
            return Ok(None);
        };
        if offset.checked_add(8).is_none_or(|end| end > bytes.len()) {
            return Err(TxnError::Corrupt);
        }
        let mut owned = bytes.to_vec();
        owned[offset..offset + 8].copy_from_slice(&seq.to_be_bytes());
        Ok(Some(owned))
    }

    fn unregister(&mut self) {
        if let Some(id) = self.active_id.take() {
            self.shared.state.lock().expect("txn").active.remove(&id);
        }
    }
}

impl<V: Vfs> Drop for Txn<V> {
    fn drop(&mut self) {
        self.unregister();
    }
}

impl<V: Vfs> Shared<V> {
    fn open(storage: Storage<V>) -> Result<Self, TxnError> {
        let frontier = load_frontier(&storage)?;
        let durable_floors = load_floors(&storage)?;
        let next_floor = durable_floors
            .keys()
            .next_back()
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        Ok(Self {
            storage,
            commit: Mutex::new(()),
            state: Mutex::new(DbState {
                frontier,
                history: VecDeque::new(),
                history_bytes: 0,
                evicted_through: None,
                active: BTreeMap::new(),
                durable_floors,
                ephemeral_floors: BTreeMap::new(),
                max_pin: DEFAULT_MAX_PIN,
            }),
            alloc: Mutex::new(Allocator {
                lanes: HashMap::new(),
            }),
            cv: Condvar::new(),
            validate: AtomicBool::new(true),
            stats: Mutex::new(TxnStats::default()),
            next_active: AtomicU64::new(1),
            next_txn: AtomicU64::new(1),
            next_floor: AtomicU64::new(next_floor),
        })
    }
}

fn next_row<V: Vfs>(scan: &mut pizzlydb_storage::Scan<'_, V>) -> Result<Option<Row>, TxnError> {
    match scan.next() {
        Some(row) => row.map(Some).map_err(map_storage),
        None => Ok(None),
    }
}

impl Allocator {
    fn allocate<V: Vfs>(
        &mut self,
        storage: &Storage<V>,
        namespace: &[u8; 16],
        kind: u8,
    ) -> Result<u128, TxnError> {
        let lane = self.lanes.entry((*namespace, kind)).or_insert(Lane {
            reserved: 0,
            next: 1,
            persisted: 0,
            persisted_next: 1,
            loaded: false,
        });
        if !lane.loaded {
            load_lane(storage, namespace, kind, lane)?;
        }
        if lane.next >= CLIENT_BIT {
            return Err(TxnError::IdExhausted);
        }
        if lane.next >= lane.reserved {
            let end = lane.next.saturating_add(ID_BLOCK).min(CLIENT_BIT);
            if lane.next >= end {
                return Err(TxnError::IdExhausted);
            }
            lane.reserved = end;
        }
        let id = lane.next;
        lane.next = lane.next.saturating_add(1);
        Ok(id)
    }

    fn dirty_watermarks(&mut self) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut out = Vec::new();
        for ((namespace, kind), lane) in &mut self.lanes {
            if lane.reserved > lane.persisted || lane.next > lane.persisted_next {
                let mut value = Vec::with_capacity(32);
                value.extend_from_slice(&lane.reserved.to_be_bytes());
                value.extend_from_slice(&lane.next.to_be_bytes());
                out.push((id_key(namespace, *kind), value));
                lane.persisted = lane.reserved;
                lane.persisted_next = lane.next;
            }
        }
        out
    }
}

fn load_lane<V: Vfs>(
    storage: &Storage<V>,
    namespace: &[u8; 16],
    kind: u8,
    lane: &mut Lane,
) -> Result<(), TxnError> {
    let view = storage.snapshot().map_err(map_storage)?;
    if let Some(bytes) = view.get(&id_key(namespace, kind)).map_err(map_storage)? {
        if bytes.len() != 32 {
            return Err(TxnError::Corrupt);
        }
        let reserved = u128::from_be_bytes(bytes[..16].try_into().map_err(|_| TxnError::Corrupt)?);
        let next = u128::from_be_bytes(bytes[16..].try_into().map_err(|_| TxnError::Corrupt)?);
        lane.reserved = reserved;
        lane.persisted = reserved;
        lane.next = if next == 0 { 1 } else { next };
        lane.persisted_next = lane.next;
    }
    lane.loaded = true;
    Ok(())
}

fn load_frontier<V: Vfs>(storage: &Storage<V>) -> Result<Option<CommitSeq>, TxnError> {
    let view = storage.snapshot().map_err(map_storage)?;
    let stored = match view.get(&frontier_key()).map_err(map_storage)? {
        None => None,
        Some(bytes) => {
            let array: [u8; 8] = bytes.as_slice().try_into().map_err(|_| TxnError::Corrupt)?;
            Some(CommitSeq::from_be_bytes(array)?)
        }
    };
    let mut expected = 1u64;
    for item in view
        .scan_range(
            Some(&commit_prefix()),
            Some(&commit_end()),
            Direction::Forward,
        )
        .map_err(map_storage)?
    {
        let (key, value) = item.map_err(map_storage)?;
        if key.len() < commit_prefix().len() + 8 {
            return Err(TxnError::Corrupt);
        }
        let seq_bytes: [u8; 8] = key[key.len() - 8..]
            .try_into()
            .map_err(|_| TxnError::Corrupt)?;
        let seq = CommitSeq::from_be_bytes(seq_bytes)?;
        if seq.get() != expected {
            return Err(TxnError::Corrupt);
        }
        let record = record::decode_record(&value)?;
        if record.seq != seq {
            return Err(TxnError::Corrupt);
        }
        expected = expected.saturating_add(1);
    }
    let last = expected.saturating_sub(1);
    match stored {
        None if last == 0 => Ok(None),
        Some(seq) if seq.get() == last => Ok(Some(seq)),
        _ => Err(TxnError::Corrupt),
    }
}

fn load_floors<V: Vfs>(storage: &Storage<V>) -> Result<BTreeMap<u64, CommitSeq>, TxnError> {
    let view = storage.snapshot().map_err(map_storage)?;
    let mut floors = BTreeMap::new();
    for item in view
        .scan_range(
            Some(&floor_prefix()),
            Some(&floor_end()),
            Direction::Forward,
        )
        .map_err(map_storage)?
    {
        let (key, value) = item.map_err(map_storage)?;
        if key.len() < floor_prefix().len() + 8 || value.len() != 8 {
            return Err(TxnError::Corrupt);
        }
        let id_bytes: [u8; 8] = key[key.len() - 8..]
            .try_into()
            .map_err(|_| TxnError::Corrupt)?;
        let seq_bytes: [u8; 8] = value.as_slice().try_into().map_err(|_| TxnError::Corrupt)?;
        floors.insert(
            u64::from_be_bytes(id_bytes),
            CommitSeq::from_be_bytes(seq_bytes)?,
        );
    }
    Ok(floors)
}

fn history_gap(
    snapshot: Option<CommitSeq>,
    frontier: Option<CommitSeq>,
    history: &VecDeque<HistItem>,
    evicted_through: Option<CommitSeq>,
) -> bool {
    let Some(frontier) = frontier else {
        return false;
    };
    let need = match snapshot {
        None => CommitSeq::first(),
        Some(seq) => match seq.next() {
            Ok(next) => next,
            Err(_) => return false,
        },
    };
    if need > frontier {
        return false;
    }
    if evicted_through.is_some_and(|evicted| need <= evicted) {
        return true;
    }
    match history.front() {
        None => true,
        Some(oldest) => oldest.seq > need,
    }
}

fn evict_history(state: &mut DbState, max_bytes: usize) {
    while state.history_bytes > max_bytes {
        let Some(old) = state.history.pop_front() else {
            break;
        };
        state.history_bytes = state.history_bytes.saturating_sub(old.write.bytes());
        state.evicted_through = Some(old.seq);
    }
}

fn retention_min(state: &DbState) -> Option<CommitSeq> {
    let frontier = state.frontier?;
    let mut live = Vec::new();
    for seq in state
        .durable_floors
        .values()
        .chain(state.ephemeral_floors.values())
    {
        let distance = frontier.get().saturating_sub(seq.get());
        if distance <= state.max_pin {
            live.push(*seq);
        }
    }
    live.into_iter().min()
}

fn frontier_after(frontier: Option<CommitSeq>, after: Option<CommitSeq>) -> bool {
    match (frontier, after) {
        (Some(frontier), Some(after)) => frontier > after,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

fn namespaces_of(mutations: &[GraphMutation]) -> Vec<NamespaceId> {
    let mut names: Vec<NamespaceId> = mutations.iter().map(GraphMutation::namespace).collect();
    names.sort();
    names.dedup();
    names
}

fn decode_i64(bytes: &[u8]) -> Result<i64, TxnError> {
    let array: [u8; 8] = bytes.try_into().map_err(|_| TxnError::Corrupt)?;
    Ok(i64::from_be_bytes(array))
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn note_abort<V: Vfs>(shared: &Shared<V>, err: &TxnError) {
    let mut stats = shared.stats.lock().expect("txn");
    match err {
        TxnError::Conflict { .. } => {
            stats.aborts_conflict = stats.aborts_conflict.saturating_add(1)
        }
        TxnError::Expired | TxnError::SnapshotTooOld => {
            stats.aborts_expired = stats.aborts_expired.saturating_add(1);
        }
        TxnError::TransactionTooLarge { .. } => {
            stats.aborts_too_large = stats.aborts_too_large.saturating_add(1);
        }
        _ => {}
    }
}

fn spawn_reaper<V: Vfs + 'static>(shared: &Arc<Shared<V>>) {
    let weak = Arc::downgrade(shared);
    thread::spawn(move || reaper_loop(weak));
}

fn reaper_loop<V: Vfs>(weak: Weak<Shared<V>>) {
    while let Some(shared) = weak.upgrade() {
        let _ = shared.storage.reap_snapshots();
        drop(shared);
        thread::sleep(Duration::from_millis(200));
    }
}
