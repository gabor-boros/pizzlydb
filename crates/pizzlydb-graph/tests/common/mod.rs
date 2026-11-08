//! Helpers shared by the graph integration suites.
//!
//! Each integration binary compiles this file on its own, so a helper used by
//! one suite is unused in the others.

#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;

use pizzlydb_core::{NamespaceId, PropertyKeyId};
use pizzlydb_graph::{GraphStore, KeyRead};
use pizzlydb_storage::{Durability, MemoryVfs, Storage, StorageError};

pub const PATH: &str = "graph";

/// Kind byte offset in every graph key: after the 16-byte namespace.
pub const KIND_AT: usize = 16;
pub const ADJACENCY: u8 = 0x06;
pub const DECLARED_CELL: u8 = 0x0B;
pub const UNDECLARED_CELL: u8 = 0x0C;

pub fn create(vfs: &MemoryVfs) -> Storage<MemoryVfs> {
    Storage::create(vfs, Path::new(PATH), 16, Durability::Full, |id| id.fill(7))
        .expect("create storage")
}

/// Drop `storage`, discard unsynced bytes, and open the file again.
pub fn crash_and_reopen(vfs: &MemoryVfs, storage: Storage<MemoryVfs>) -> Storage<MemoryVfs> {
    drop(storage);
    vfs.crash();
    Storage::open(vfs, Path::new(PATH), 16, Durability::Full).expect("reopen storage")
}

pub fn graph(namespace: u128) -> GraphStore {
    GraphStore::new(NamespaceId::from_u128(namespace).expect("namespace"))
}

/// Every key and value visible to `read`.
pub fn dump(read: &impl KeyRead) -> BTreeMap<Vec<u8>, Vec<u8>> {
    read.scan(&[])
        .expect("scan")
        .map(|entry| entry.expect("entry"))
        .filter(|(key, _)| key.len() < 16 || key[..16] != [0; 16])
        .collect()
}

/// Number of keys of `kind` in `read`.
pub fn count_kind(read: &impl KeyRead, kind: u8) -> usize {
    dump(read)
        .keys()
        .filter(|key| key.get(KIND_AT) == Some(&kind))
        .count()
}

/// Undeclared cell key of a node property, built from the documented layout.
pub fn undeclared_node_cell(graph: &GraphStore, node: &[u8; 16], key: PropertyKeyId) -> Vec<u8> {
    let mut out = graph.namespace().to_bytes().to_vec();
    out.extend_from_slice(&[DECLARED_CELL, 1]);
    out.extend_from_slice(node);
    out.extend_from_slice(key.as_bytes());
    out
}

/// SplitMix64.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..bound`. `bound` must be non-zero.
    pub fn below(&mut self, bound: usize) -> usize {
        usize::try_from(self.next() % bound as u64).expect("fits usize")
    }

    pub fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        if items.is_empty() {
            None
        } else {
            items.get(self.below(items.len()))
        }
    }
}

/// [`KeyRead`] wrapper that records every point read, scan start, and
/// scanned key.
pub struct Tracing<R> {
    pub inner: R,
    pub gets: RefCell<Vec<Vec<u8>>>,
    pub scans: RefCell<Vec<Vec<u8>>>,
    pub scanned: RefCell<Vec<Vec<u8>>>,
}

impl<R> Tracing<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            gets: RefCell::new(Vec::new()),
            scans: RefCell::new(Vec::new()),
            scanned: RefCell::new(Vec::new()),
        }
    }
}

impl<R: KeyRead> KeyRead for Tracing<R> {
    type Scan<'a>
        = TracingScan<'a, R>
    where
        Self: 'a;

    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        self.gets.borrow_mut().push(key.to_vec());
        self.inner.get(key)
    }

    fn scan(&self, from: &[u8]) -> Result<Self::Scan<'_>, StorageError> {
        self.scans.borrow_mut().push(from.to_vec());
        Ok(TracingScan {
            inner: self.inner.scan(from)?,
            scanned: &self.scanned,
        })
    }
}

pub struct TracingScan<'a, R: KeyRead + 'a> {
    inner: R::Scan<'a>,
    scanned: &'a RefCell<Vec<Vec<u8>>>,
}

impl<'a, R: KeyRead + 'a> Iterator for TracingScan<'a, R> {
    type Item = Result<(Vec<u8>, Vec<u8>), StorageError>;

    fn next(&mut self) -> Option<Self::Item> {
        let item = self.inner.next()?;
        if let Ok((key, _)) = &item {
            self.scanned.borrow_mut().push(key.clone());
        }
        Some(item)
    }
}
