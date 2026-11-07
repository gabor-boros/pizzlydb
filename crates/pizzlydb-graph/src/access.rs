//! Read access to one consistent key state.

use std::marker::PhantomData;

use pizzlydb_core::{ID_LEN, Identifier};
use pizzlydb_storage::{Direction, ReadView, Scan, StorageError, Vfs};

use crate::error::{GraphError, corrupt};

/// Point reads and ordered scans over one consistent key state.
///
/// [`ReadView`] reads one committed snapshot.
/// [`GraphBatch`](crate::GraphBatch) reads that snapshot plus its own staged
/// writes. Every read method of [`GraphStore`](crate::GraphStore) takes a
/// `KeyRead`.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use std::cell::Cell;
/// use std::path::Path;
///
/// use pizzlydb_core::NamespaceId;
/// use pizzlydb_graph::{GraphBatch, GraphStore, KeyRead};
/// use pizzlydb_storage::{Durability, MemoryVfs, Storage, StorageError};
///
/// struct Counting<R> {
///     inner: R,
///     gets: Cell<usize>,
/// }
///
/// impl<R: KeyRead> KeyRead for Counting<R> {
///     type Scan<'a> = R::Scan<'a> where Self: 'a;
///
///     fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
///         self.gets.set(self.gets.get() + 1);
///         self.inner.get(key)
///     }
///
///     fn scan(&self, from: &[u8]) -> Result<Self::Scan<'_>, StorageError> {
///         self.inner.scan(from)
///     }
/// }
///
/// let vfs = MemoryVfs::new();
/// let storage = Storage::create(&vfs, Path::new("db"), 8, Durability::Full, |id| id.fill(1))?;
/// let graph = GraphStore::new(NamespaceId::from_u128(1)?);
/// let mut batch = GraphBatch::begin(&storage)?;
/// let node = graph.create_node(&mut batch)?;
/// batch.commit()?;
///
/// let view = Counting { inner: storage.snapshot()?, gets: Cell::new(0) };
/// graph.get_node(&view, node)?;
/// assert_eq!(view.gets.get(), 1);
/// # Ok(())
/// # }
/// ```
/// One stored key and its value.
pub type KeyValue = (Vec<u8>, Vec<u8>);

pub trait KeyRead {
    /// Ordered scan returned by [`Self::scan`].
    type Scan<'a>: Iterator<Item = Result<(Vec<u8>, Vec<u8>), StorageError>>
    where
        Self: 'a;

    /// Copy the value at `key`, or [`None`] when the key is absent.
    ///
    /// # Errors
    ///
    /// Returns the storage error of the underlying read.
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError>;

    /// Scan keys in unsigned byte order, starting at the first key that is
    /// greater than or equal to `from`.
    ///
    /// # Errors
    ///
    /// Returns the storage error of the underlying read.
    fn scan(&self, from: &[u8]) -> Result<Self::Scan<'_>, StorageError>;

    /// Keys in `[start, end)`, in ascending order.
    ///
    /// # Errors
    ///
    /// Returns the storage error of the underlying read.
    fn scan_range(&self, start: &[u8], end: &[u8]) -> Result<Vec<KeyValue>, StorageError> {
        let mut rows = Vec::new();
        for item in self.scan(start)? {
            let (key, value) = item?;
            if key.as_slice() >= end {
                break;
            }
            rows.push((key, value));
        }
        Ok(rows)
    }
}

impl<'db, V: Vfs> KeyRead for ReadView<'db, V> {
    type Scan<'a>
        = Scan<'a, 'db, V>
    where
        Self: 'a;

    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        ReadView::get(self, key)
    }

    fn scan(&self, from: &[u8]) -> Result<Self::Scan<'_>, StorageError> {
        ReadView::scan(self, Some(from))
    }

    fn scan_range(&self, start: &[u8], end: &[u8]) -> Result<Vec<KeyValue>, StorageError> {
        let mut rows = Vec::new();
        for item in ReadView::scan_range(self, Some(start), Some(end), Direction::Forward)? {
            rows.push(item?);
        }
        Ok(rows)
    }
}

/// Keys that start with one prefix, in key order.
///
/// The rows come from [`KeyRead::scan_range`]. The exclusive end is the
/// prefix successor, so the read stops at the range instead of the next key
/// outside it. A prefix of empty or all `0xff` bytes has no successor and
/// falls back to [`KeyRead::scan`].
pub(crate) struct PrefixScan<'r, R: KeyRead + 'r> {
    rows: std::vec::IntoIter<(Vec<u8>, Vec<u8>)>,
    prefix: Vec<u8>,
    done: bool,
    _read: PhantomData<&'r R>,
}

fn exclusive_prefix_end(prefix: &[u8]) -> Option<Vec<u8>> {
    if prefix.is_empty() {
        return None;
    }

    let mut end = prefix.to_vec();
    while let Some(byte) = end.pop() {
        if byte < u8::MAX {
            end.push(byte + 1);
            return Some(end);
        }
    }

    None
}

fn collect_prefix<R: KeyRead + ?Sized>(
    read: &R,
    prefix: &[u8],
    start: &[u8],
) -> Result<Vec<KeyValue>, StorageError> {
    let Some(end) = exclusive_prefix_end(prefix) else {
        let mut rows = Vec::new();
        for item in read.scan(start)? {
            let (key, value) = item?;
            if !key.starts_with(prefix) {
                break;
            }
            rows.push((key, value));
        }
        return Ok(rows);
    };

    if start >= end.as_slice() {
        return Ok(Vec::new());
    }

    read.scan_range(start, &end)
}

impl<'r, R: KeyRead + 'r> PrefixScan<'r, R> {
    pub(crate) fn new(read: &'r R, prefix: Vec<u8>) -> Result<Self, GraphError> {
        let rows = collect_prefix(read, &prefix, &prefix)?;
        Ok(Self {
            rows: rows.into_iter(),
            prefix,
            done: false,
            _read: PhantomData,
        })
    }

    /// Scan keys under `prefix`, starting at `start`.
    pub(crate) fn starting_at(
        read: &'r R,
        prefix: Vec<u8>,
        start: &[u8],
    ) -> Result<Self, GraphError> {
        let rows = collect_prefix(read, &prefix, start)?;
        Ok(Self {
            rows: rows.into_iter(),
            prefix,
            done: false,
            _read: PhantomData,
        })
    }

    pub(crate) fn prefix_len(&self) -> usize {
        self.prefix.len()
    }

    pub(crate) fn finish(&mut self) {
        self.done = true;
    }
}

impl<'r, R: KeyRead + 'r> Iterator for PrefixScan<'r, R> {
    type Item = Result<(Vec<u8>, Vec<u8>), GraphError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }

        match self.rows.next() {
            Some((key, value)) if key.starts_with(&self.prefix) => Some(Ok((key, value))),
            Some(_) | None => {
                self.done = true;
                None
            }
        }
    }
}

/// Identifiers stored as the last field of the keys under one prefix.
///
/// [`GraphStore::labels`](crate::GraphStore::labels),
/// [`GraphStore::nodes_with_label`](crate::GraphStore::nodes_with_label), and
/// [`GraphStore::edges_with_type`](crate::GraphStore::edges_with_type) return
/// this iterator. It borrows the read view and yields identifiers in byte
/// order. A storage error ends the scan after it is yielded.
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
/// let person: LabelId = graph.intern(&mut batch, "Person")?;
/// let node = graph.create_node(&mut batch)?;
/// graph.add_label(&mut batch, node, person)?;
/// batch.commit()?;
///
/// let view = storage.snapshot()?;
/// let members = graph.nodes_with_label(&view, person)?.collect::<Result<Vec<_>, _>>()?;
/// assert_eq!(members, vec![node]);
/// # Ok(())
/// # }
/// ```
#[must_use = "a scan must be driven to read keys"]
pub struct IdScan<'r, R: KeyRead + 'r, I> {
    scan: PrefixScan<'r, R>,
    marker: PhantomData<fn() -> I>,
}

impl<'r, R: KeyRead + 'r, I> IdScan<'r, R, I> {
    pub(crate) fn new(read: &'r R, prefix: Vec<u8>) -> Result<Self, GraphError> {
        Ok(Self {
            scan: PrefixScan::new(read, prefix)?,
            marker: PhantomData,
        })
    }
}

impl<'r, R: KeyRead + 'r, I: Identifier> Iterator for IdScan<'r, R, I> {
    type Item = Result<I, GraphError>;

    fn next(&mut self) -> Option<Self::Item> {
        let expected = self.scan.prefix_len() + ID_LEN;
        let item = self.scan.next()?;
        Some(item.and_then(|(key, _)| {
            if key.len() == expected {
                id_at(&key, expected - ID_LEN)
            } else {
                Err(GraphError::Corrupt)
            }
        }))
    }
}

/// Decode the identifier stored at `offset` in `bytes`.
pub(crate) fn id_at<I: Identifier>(bytes: &[u8], offset: usize) -> Result<I, GraphError> {
    let end = offset.checked_add(ID_LEN).ok_or(GraphError::Corrupt)?;
    let slice = bytes.get(offset..end).ok_or(GraphError::Corrupt)?;
    let array = <[u8; ID_LEN]>::try_from(slice).map_err(corrupt)?;
    I::from_bytes(array).map_err(corrupt)
}
