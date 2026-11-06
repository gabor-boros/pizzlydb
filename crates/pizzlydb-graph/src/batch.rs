//! Staged graph writes over one storage write batch.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::collections::btree_map;
use std::iter::Peekable;
use std::ops::Bound;

use pizzlydb_storage::{
    Lsn, MAX_KEY_LEN, MAX_VALUE_LEN, ReadView, Scan, Storage, StorageError, Vfs, WriteBatch,
};

use crate::access::KeyRead;
use crate::error::GraphError;

type Staged = BTreeMap<Vec<u8>, Option<Vec<u8>>>;
type UndoLog = Vec<(Vec<u8>, Option<Option<Vec<u8>>>)>;

/// Graph writes staged on one storage write batch.
///
/// [`Self::begin`] holds the storage writer lock until the batch is committed
/// or dropped, then reads the latest committed snapshot. Reads through
/// [`KeyRead`] see that snapshot plus every write staged on this batch.
///
/// [`Self::commit`] publishes every staged write as one atomic storage batch.
/// Dropping the batch publishes nothing. A [`GraphStore`](crate::GraphStore)
/// call that returns an error leaves the staged writes as they were before the
/// call.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use std::path::Path;
///
/// use pizzlydb_core::NamespaceId;
/// use pizzlydb_graph::{GraphBatch, GraphError, GraphStore};
/// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
///
/// let vfs = MemoryVfs::new();
/// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
/// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
///
/// let mut batch = GraphBatch::begin(&storage)?;
/// let node = graph.create_node(&mut batch)?;
/// assert!(graph.get_node(&batch, node).is_ok());
/// drop(batch);
///
/// let view = storage.snapshot()?;
/// assert!(matches!(graph.get_node(&view, node), Err(GraphError::NotFound)));
/// # Ok(())
/// # }
/// ```
#[must_use = "dropping the batch rolls it back"]
pub struct GraphBatch<'db, V: Vfs> {
    storage: &'db Storage<V>,
    writer: WriteBatch<V>,
    base: ReadView<'db, V>,
    staged: Staged,
    undo: Option<UndoLog>,
}

impl<'db, V: Vfs> GraphBatch<'db, V> {
    /// Take the storage writer lock and read the latest committed snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Storage`] with [`StorageError::Busy`] when another
    /// write batch is open, and the other errors of
    /// [`Storage::begin_write`] and [`Storage::snapshot`].
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_graph::{GraphBatch, GraphError};
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage, StorageError};
    ///
    /// let vfs = MemoryVfs::new();
    /// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
    ///
    /// let batch = GraphBatch::begin(&storage)?;
    /// assert!(matches!(
    ///     GraphBatch::begin(&storage),
    ///     Err(GraphError::Storage(StorageError::Busy))
    /// ));
    /// drop(batch);
    /// # Ok(())
    /// # }
    /// ```
    pub fn begin(storage: &'db Storage<V>) -> Result<Self, GraphError> {
        let writer = storage.begin_write()?;
        let base = storage.snapshot()?;
        Ok(Self {
            storage,
            writer,
            base,
            staged: BTreeMap::new(),
            undo: None,
        })
    }

    /// Publish every staged write as one atomic storage batch.
    ///
    /// The batch uses the durability passed to [`Storage::create`] or
    /// [`Storage::open`]. A batch with no staged writes returns the current
    /// sequence and appends nothing.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Storage`] with the errors of
    /// [`Storage::atomic_write_sync`]. On error nothing is published.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_core::NamespaceId;
    /// use pizzlydb_graph::{GraphBatch, GraphStore};
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
    /// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
    ///
    /// let mut batch = GraphBatch::begin(&storage)?;
    /// let node = graph.create_node(&mut batch)?;
    /// let lsn = batch.commit()?;
    /// assert!(lsn.as_u64() > 0);
    /// assert!(graph.get_node(&storage.snapshot()?, node).is_ok());
    /// # Ok(())
    /// # }
    /// ```
    pub fn commit(self) -> Result<Lsn, GraphError> {
        let Self {
            storage,
            mut writer,
            base,
            staged,
            undo: _,
        } = self;

        drop(base);
        for (key, value) in &staged {
            match value {
                Some(bytes) => writer.put(key, bytes)?,
                None => writer.delete(key)?,
            }
        }

        Ok(storage.atomic_write_sync(writer)?)
    }

    /// Number of staged key writes and deletes.
    ///
    /// A call that changes nothing, such as interning an existing name, does
    /// not change this count.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_core::{LabelId, NamespaceId};
    /// use pizzlydb_graph::{GraphBatch, GraphStore};
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
    /// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
    ///
    /// let mut batch = GraphBatch::begin(&storage)?;
    /// assert!(batch.is_empty());
    /// let _: LabelId = graph.intern(&mut batch, "Person")?;
    /// let staged = batch.len();
    /// let _: LabelId = graph.intern(&mut batch, "Person")?;
    /// assert_eq!(batch.len(), staged);
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn len(&self) -> usize {
        self.staged.len()
    }

    /// `true` when nothing is staged.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::path::Path;
    ///
    /// use pizzlydb_graph::GraphBatch;
    /// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
    ///
    /// let vfs = MemoryVfs::new();
    /// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
    /// assert!(GraphBatch::begin(&storage)?.is_empty());
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.staged.is_empty()
    }

    /// Stage `value` at `key`.
    pub(crate) fn put(&mut self, key: Vec<u8>, value: Vec<u8>) -> Result<(), GraphError> {
        if key.len() > MAX_KEY_LEN {
            return Err(GraphError::KeyTooLarge);
        }

        if value.len() > MAX_VALUE_LEN {
            return Err(GraphError::ValueTooLarge);
        }

        self.stage(key, Some(value));
        Ok(())
    }

    /// Stage removal of `key`. An absent key stages nothing.
    pub(crate) fn delete(&mut self, key: Vec<u8>) -> Result<(), GraphError> {
        if key.len() > MAX_KEY_LEN {
            return Err(GraphError::KeyTooLarge);
        }

        if self.get(&key)?.is_some() {
            self.stage(key, None);
        }

        Ok(())
    }

    /// Run `op`. When it returns an error, restore every key it staged.
    ///
    /// A nested call joins the outermost one.
    pub(crate) fn atomic<T>(
        &mut self,
        op: impl FnOnce(&mut Self) -> Result<T, GraphError>,
    ) -> Result<T, GraphError> {
        if self.undo.is_some() {
            return op(self);
        }

        self.undo = Some(Vec::new());
        let result = op(self);
        let undo = self.undo.take().unwrap_or_default();
        if result.is_err() {
            for (key, previous) in undo.into_iter().rev() {
                match previous {
                    Some(entry) => {
                        self.staged.insert(key, entry);
                    }
                    None => {
                        self.staged.remove(&key);
                    }
                }
            }
        }

        result
    }

    fn stage(&mut self, key: Vec<u8>, value: Option<Vec<u8>>) {
        match &mut self.undo {
            Some(undo) => {
                let previous = self.staged.insert(key.clone(), value);
                undo.push((key, previous));
            }
            None => {
                self.staged.insert(key, value);
            }
        }
    }
}

impl<'db, V: Vfs> KeyRead for GraphBatch<'db, V> {
    type Scan<'a>
        = BatchScan<'a, 'db, V>
    where
        Self: 'a;

    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        match self.staged.get(key) {
            Some(entry) => Ok(entry.clone()),
            None => self.base.get(key),
        }
    }

    fn scan(&self, from: &[u8]) -> Result<Self::Scan<'_>, StorageError> {
        let base = self.base.scan(Some(from))?.peekable();
        let staged = self
            .staged
            .range::<[u8], _>((Bound::Included(from), Bound::Unbounded))
            .peekable();

        Ok(BatchScan { base, staged })
    }
}

/// Ordered scan of a [`GraphBatch`]: the snapshot merged with staged writes.
///
/// A staged write replaces the snapshot entry with the same key. A staged
/// delete hides it.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use std::path::Path;
///
/// use pizzlydb_core::NamespaceId;
/// use pizzlydb_graph::{GraphBatch, GraphStore, KeyRead};
/// use pizzlydb_storage::{Durability, MemoryVfs, Storage};
///
/// let vfs = MemoryVfs::new();
/// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
/// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
///
/// let mut batch = GraphBatch::begin(&storage)?;
/// graph.create_node(&mut batch)?;
/// let staged_keys = batch.scan(&[])?.count();
/// assert_eq!(staged_keys, batch.len());
/// # Ok(())
/// # }
/// ```
#[must_use = "a scan must be driven to read keys"]
pub struct BatchScan<'a, 'db, V: Vfs> {
    base: Peekable<Scan<'a, 'db, V>>,
    staged: Peekable<btree_map::Range<'a, Vec<u8>, Option<Vec<u8>>>>,
}

enum Next {
    Base,
    Staged,
    Both,
    Done,
}

impl<V: Vfs> Iterator for BatchScan<'_, '_, V> {
    type Item = Result<(Vec<u8>, Vec<u8>), StorageError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let next = match (self.base.peek(), self.staged.peek()) {
                (Some(Err(_)), _) => return self.base.next(),
                (None, None) => Next::Done,
                (Some(Ok(_)), None) => Next::Base,
                (None, Some(_)) => Next::Staged,
                (Some(Ok((base, _))), Some((staged, _))) => match base.as_slice().cmp(staged) {
                    Ordering::Less => Next::Base,
                    Ordering::Equal => Next::Both,
                    Ordering::Greater => Next::Staged,
                },
            };
            match next {
                Next::Done => return None,
                Next::Base => return self.base.next(),
                Next::Both | Next::Staged => {
                    if matches!(next, Next::Both) {
                        self.base.next();
                    }
                    if let Some((key, Some(value))) = self.staged.next() {
                        return Some(Ok((key.clone(), value.clone())));
                    }
                }
            }
        }
    }
}
