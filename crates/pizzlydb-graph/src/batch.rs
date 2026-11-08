//! Graph transactions over [`pizzlydb_txn::Txn`].
//!
//! [`GraphTxn::begin`] does not take the storage writer lock. Many transactions
//! can run at once. Commit validates reads, assigns a commit sequence, and
//! writes the staged keys together with their logical mutations.

use std::collections::BTreeSet;
use std::vec::IntoIter;

use pizzlydb_core::{CommitSeq, GraphMutation, NamespaceId};
use pizzlydb_storage::{LimitKind, Storage, StorageError, Vfs};
use pizzlydb_txn::{Txn, TxnError, TxnManager, TxnOptions};

use crate::access::{KeyRead, KeyValue};
use crate::error::GraphError;
use crate::property::Owner;

/// Graph writes staged on one optimistic transaction.
///
/// [`Self::begin`] reads the latest committed snapshot. Reads through
/// [`KeyRead`] see that snapshot plus every write staged on this transaction.
///
/// [`Self::commit`] validates the read set and publishes the staged writes,
/// the mutation log, and the commit record as one storage batch. Dropping the
/// transaction publishes nothing. A [`GraphStore`](crate::GraphStore) call
/// that returns an error leaves the staged writes as they were before the call.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use std::path::Path;
///
/// use pizzlydb_core::NamespaceId;
/// use pizzlydb_graph::{GraphError, GraphStore, GraphTxn};
/// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
///
/// let vfs = MemoryVfs::new();
/// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
/// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
///
/// let mut txn = GraphTxn::begin(&storage)?;
/// let node = graph.create_node(&mut txn)?;
/// assert!(graph.get_node(&txn, node).is_ok());
/// drop(txn);
///
/// let view = storage.snapshot()?;
/// assert!(matches!(graph.get_node(&view, node), Err(GraphError::NotFound)));
/// # Ok(())
/// # }
/// ```
#[must_use = "dropping the transaction rolls it back"]
pub struct GraphTxn<V: Vfs> {
    txn: Txn<V>,
    mutations: Vec<GraphMutation>,
    touched: BTreeSet<(NamespaceId, Owner)>,
    schema_bumped: BTreeSet<NamespaceId>,
}

/// Same type as [`GraphTxn`].
pub type GraphBatch<V> = GraphTxn<V>;

impl<V: Vfs + 'static> GraphTxn<V> {
    /// Open a transaction on the manager attached to `storage`.
    ///
    /// A second call does not wait for this transaction to commit.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Storage`] or [`GraphError::Transaction`] when the
    /// database cannot be read.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_graph::GraphTxn;
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
    ///
    /// let first = GraphTxn::begin(&storage)?;
    /// let second = GraphTxn::begin(&storage)?;
    /// drop(first);
    /// drop(second);
    /// # Ok(())
    /// # }
    /// ```
    pub fn begin(storage: &Storage<V>) -> Result<Self, GraphError> {
        Self::begin_with(storage, TxnOptions::default())
    }

    /// [`Self::begin`] with explicit options.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::begin`].
    pub fn begin_with(storage: &Storage<V>, options: TxnOptions) -> Result<Self, GraphError> {
        let manager = TxnManager::attach(storage)?;
        let txn = manager.begin(options)?;
        Ok(Self {
            txn,
            mutations: Vec::new(),
            touched: BTreeSet::new(),
            schema_bumped: BTreeSet::new(),
        })
    }
}

impl<V: Vfs> GraphTxn<V> {
    /// Publish staged writes, mutations, and the commit record.
    ///
    /// An empty transaction returns the snapshot sequence and appends nothing.
    /// `None` means the database has no commits yet.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Transaction`] when validation fails or the
    /// transaction expired. On error nothing is published.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_core::NamespaceId;
    /// use pizzlydb_graph::{GraphStore, GraphTxn};
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
    /// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
    ///
    /// let mut txn = GraphTxn::begin(&storage)?;
    /// let node = graph.create_node(&mut txn)?;
    /// let seq = txn.commit()?.expect("a user commit");
    /// assert!(seq.get() > 0);
    /// assert!(graph.get_node(&storage.snapshot()?, node).is_ok());
    /// # Ok(())
    /// # }
    /// ```
    pub fn commit(mut self) -> Result<Option<CommitSeq>, GraphError> {
        crate::constraint::validate_touched(&mut self)?;
        crate::stats::apply(&mut self)?;
        let Self { txn, mutations, .. } = self;
        Ok(txn.commit(&mutations)?)
    }

    /// Mutations recorded by successful operations, in order.
    #[must_use]
    pub fn mutations(&self) -> &[GraphMutation] {
        &self.mutations
    }

    /// Number of staged key writes and deletes.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_core::{LabelId, NamespaceId};
    /// use pizzlydb_graph::{GraphStore, GraphTxn};
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
    /// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
    ///
    /// let mut txn = GraphTxn::begin(&storage)?;
    /// assert!(txn.is_empty());
    /// let _: LabelId = graph.intern(&mut txn, "Person")?;
    /// let staged = txn.len();
    /// let _: LabelId = graph.intern(&mut txn, "Person")?;
    /// assert_eq!(txn.len(), staged);
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn len(&self) -> usize {
        self.txn.staged_len()
    }

    /// `true` when nothing is staged.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_graph::GraphTxn;
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
    /// assert!(GraphTxn::begin(&storage)?.is_empty());
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.txn.staged_len() == 0
    }

    /// Append a logical mutation. A failed [`Self::atomic`] call drops it.
    pub(crate) fn record(&mut self, mutation: GraphMutation) {
        self.mutations.push(mutation);
    }

    /// Remember `owner` for commit-time constraint checks.
    pub(crate) fn note_owner(&mut self, namespace: NamespaceId, owner: Owner) {
        self.touched.insert((namespace, owner));
    }

    pub(crate) fn touched(&self) -> Vec<(NamespaceId, Owner)> {
        self.touched.iter().copied().collect()
    }

    /// `true` after this transaction has already incremented `namespace`'s schema version.
    pub(crate) fn schema_was_bumped(&self, namespace: NamespaceId) -> bool {
        self.schema_bumped.contains(&namespace)
    }

    pub(crate) fn mark_schema_bumped(&mut self, namespace: NamespaceId) {
        self.schema_bumped.insert(namespace);
    }

    pub(crate) fn merge_add(&mut self, key: &[u8], delta: i64) -> Result<(), GraphError> {
        self.txn.merge_add(key, delta)?;
        Ok(())
    }

    pub(crate) fn snapshot_get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, GraphError> {
        Ok(self.txn.snapshot_get(key)?)
    }

    pub(crate) fn for_each_staged(&self, visit: impl FnMut(&[u8], Option<&[u8]>)) {
        self.txn.for_each_staged(visit);
    }

    pub(crate) fn stamp_commit(&mut self, key: &[u8], offset: usize) -> Result<(), GraphError> {
        self.txn.stamp_commit(key, offset)?;
        Ok(())
    }

    pub(crate) fn scan_capped(
        &self,
        start: &[u8],
        end: &[u8],
        cap: usize,
    ) -> Result<(Vec<KeyValue>, bool), GraphError> {
        let (rows, truncated) = self.txn.scan_capped(start, end, cap)?;
        Ok((rows, truncated))
    }

    /// Reserve the next id of `kind` in `namespace`.
    pub(crate) fn allocate_id(
        &mut self,
        namespace: pizzlydb_core::NamespaceId,
        kind: u8,
    ) -> Result<u128, GraphError> {
        self.allocate_raw(namespace.as_bytes(), kind)
    }

    pub(crate) fn allocate_raw(
        &mut self,
        namespace: &[u8; 16],
        kind: u8,
    ) -> Result<u128, GraphError> {
        Ok(self.txn.allocate_id(namespace, kind)?)
    }

    /// Stage `value` at `key`.
    pub(crate) fn put(&mut self, key: Vec<u8>, value: Vec<u8>) -> Result<(), GraphError> {
        if key.len() > pizzlydb_storage::MAX_KEY_LEN {
            return Err(GraphError::KeyTooLarge);
        }
        if value.len() > pizzlydb_storage::MAX_VALUE_LEN {
            return Err(GraphError::ValueTooLarge);
        }
        self.txn.put(&key, &value)?;
        Ok(())
    }

    /// Stage removal of `key` when it is visible.
    pub(crate) fn delete(&mut self, key: Vec<u8>) -> Result<(), GraphError> {
        if key.len() > pizzlydb_storage::MAX_KEY_LEN {
            return Err(GraphError::KeyTooLarge);
        }
        if self.txn.get(&key)?.is_some() {
            self.txn.delete(&key)?;
        }
        Ok(())
    }

    /// Run `op`. When it returns an error, restore every key and mutation it staged.
    ///
    /// A nested call joins the outermost one.
    pub(crate) fn atomic<T>(
        &mut self,
        op: impl FnOnce(&mut Self) -> Result<T, GraphError>,
    ) -> Result<T, GraphError> {
        let owned = self.txn.begin_savepoint();
        let mark = self.mutations.len();
        let touched = owned.then(|| self.touched.clone());
        let bumped = owned.then(|| self.schema_bumped.clone());
        let result = op(self);
        if owned {
            if result.is_err() {
                self.mutations.truncate(mark);
                if let Some(touched) = touched {
                    self.touched = touched;
                }
                if let Some(bumped) = bumped {
                    self.schema_bumped = bumped;
                }
                self.txn.rollback_savepoint();
            } else {
                self.txn.commit_savepoint();
            }
        }
        result
    }

    fn read(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        self.txn.get(key).map_err(storage_error)
    }
}

impl<V: Vfs> KeyRead for GraphTxn<V> {
    type Scan<'a>
        = GraphScan
    where
        Self: 'a;

    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        self.read(key)
    }

    fn scan(&self, from: &[u8]) -> Result<Self::Scan<'_>, StorageError> {
        let rows = self.txn.scan_range(from, None).map_err(storage_error)?;
        Ok(GraphScan {
            rows: rows.into_iter(),
        })
    }

    fn scan_range(&self, start: &[u8], end: &[u8]) -> Result<Vec<KeyValue>, StorageError> {
        self.txn.scan_range(start, Some(end)).map_err(storage_error)
    }
}

/// Owned scan of a [`GraphTxn`].
#[must_use = "a scan must be driven to read keys"]
pub struct GraphScan {
    rows: IntoIter<(Vec<u8>, Vec<u8>)>,
}

impl Iterator for GraphScan {
    type Item = Result<(Vec<u8>, Vec<u8>), StorageError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.rows.next().map(Ok)
    }
}

fn storage_error(err: TxnError) -> StorageError {
    match err {
        TxnError::Storage(err) => err,
        TxnError::SnapshotTooOld | TxnError::Expired => StorageError::SnapshotTooOld,
        TxnError::Closed => StorageError::Closed,
        TxnError::TransactionTooLarge { observed, .. } => StorageError::TransactionTooLarge {
            limit: LimitKind::Entries,
            observed,
        },
        TxnError::Conflict { .. }
        | TxnError::ReadOnlyViolation
        | TxnError::ClientIdsDisabled
        | TxnError::InvalidClientId
        | TxnError::IdExhausted
        | TxnError::Corrupt
        | TxnError::Core(_) => StorageError::Corrupt,
    }
}
