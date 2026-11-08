//! Exact counters and sampled degree and distinct-value statistics.
//!
//! Exact counters use [`Txn::merge_add`](pizzlydb_txn::Txn::merge_add). The
//! update is not part of the read set, so two writers in one namespace do not
//! conflict because they both adjusted a counter. Reads of a counter are
//! ordinary point reads: do them on a snapshot, not inside the writing
//! transaction, or the read will conflict with the next counter update.
//!
//! Byte totals count key and value lengths of staged graph keys. They are
//! exact relative to that sum and are not a count of allocated pages.
//!
//! Degree histograms and index distinct-value estimates are samples. A sample
//! that visits every edge or index entry is exact and says so. A shorter
//! sample is the quantile or distinct count of the visited prefix, which is
//! biased toward low identifiers.

use std::collections::BTreeMap;

use pizzlydb_core::{GraphMutation, IndexId, LabelId, NamespaceId, TypeId};
use pizzlydb_storage::Vfs;
use pizzlydb_txn::stat_key;

use crate::access::{KeyRead, PrefixScan};
use crate::adjacency::Direction;
use crate::batch::GraphTxn;
use crate::error::GraphError;
use crate::index::IndexPhase;
use crate::keys::{self, ADJACENCY_LEN};
use crate::record_store::GraphStore;

const NODES: u8 = 1;
const EDGES: u8 = 2;
const LABEL: u8 = 3;
const TYPE: u8 = 4;
const INDEX: u8 = 5;
const BYTES: u8 = 6;
const DEGREE_OUT: u8 = 7;
const DEGREE_IN: u8 = 8;
const NDV: u8 = 9;

const DEGREE_BUCKETS: usize = 21;

/// Whether a statistic is the committed counter or a stored sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatAsOf {
    /// Exact counter at the snapshot being read.
    Snapshot,
    /// Sample written by a maintenance call. `at_us` is the caller's timestamp.
    Sampled {
        /// Microseconds supplied when the sample was stored.
        at_us: u64,
    },
}

/// One statistic and how it was produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Statistic<T> {
    /// Reported value.
    pub value: T,
    /// `true` when `value` is not a sample approximation.
    pub exact: bool,
    /// What the value is current as of.
    pub as_of: StatAsOf,
}

/// Read and refresh statistics for one namespace.
#[derive(Clone, Copy, Debug)]
pub struct GraphStats {
    namespace: NamespaceId,
}

impl GraphStore {
    /// Statistics handle for this namespace.
    #[must_use]
    pub const fn stats(&self) -> GraphStats {
        GraphStats {
            namespace: self.namespace(),
        }
    }
}

impl GraphStats {
    /// Exact number of live nodes.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Storage`] or [`GraphError::Corrupt`].
    pub fn node_count<R: KeyRead>(&self, read: &R) -> Result<Statistic<u64>, GraphError> {
        exact_counter(read, &stat_key(self.namespace.as_bytes(), NODES, None))
    }

    /// Exact number of live edges.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Storage`] or [`GraphError::Corrupt`].
    pub fn edge_count<R: KeyRead>(&self, read: &R) -> Result<Statistic<u64>, GraphError> {
        exact_counter(read, &stat_key(self.namespace.as_bytes(), EDGES, None))
    }

    /// Exact number of nodes that carry `label`.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Storage`] or [`GraphError::Corrupt`].
    pub fn label_count<R: KeyRead>(
        &self,
        read: &R,
        label: LabelId,
    ) -> Result<Statistic<u64>, GraphError> {
        exact_counter(
            read,
            &stat_key(self.namespace.as_bytes(), LABEL, Some(label.as_bytes())),
        )
    }

    /// Exact number of edges of `edge_type`.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Storage`] or [`GraphError::Corrupt`].
    pub fn type_count<R: KeyRead>(
        &self,
        read: &R,
        edge_type: TypeId,
    ) -> Result<Statistic<u64>, GraphError> {
        exact_counter(
            read,
            &stat_key(self.namespace.as_bytes(), TYPE, Some(edge_type.as_bytes())),
        )
    }

    /// Exact number of index entries.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Storage`] or [`GraphError::Corrupt`].
    pub fn index_count<R: KeyRead>(
        &self,
        read: &R,
        index: IndexId,
    ) -> Result<Statistic<u64>, GraphError> {
        exact_counter(
            read,
            &stat_key(self.namespace.as_bytes(), INDEX, Some(index.as_bytes())),
        )
    }

    /// Sum of graph key and value lengths in this namespace.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Storage`] or [`GraphError::Corrupt`].
    pub fn namespace_bytes<R: KeyRead>(&self, read: &R) -> Result<Statistic<u64>, GraphError> {
        exact_counter(read, &stat_key(self.namespace.as_bytes(), BYTES, None))
    }

    /// Sampled degree quantile for `edge_type`.
    ///
    /// `quantile` is clamped to `0.0..=1.0`. The value is exact when the stored
    /// sample visited every edge of the type.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::NotFound`] when no sample is stored, and
    /// [`GraphError::Storage`] or [`GraphError::Corrupt`] on a failed read.
    pub fn degree_estimate<R: KeyRead>(
        &self,
        read: &R,
        edge_type: TypeId,
        direction: Direction,
        quantile: f64,
    ) -> Result<Statistic<u64>, GraphError> {
        let code = if direction == Direction::Out {
            DEGREE_OUT
        } else {
            DEGREE_IN
        };
        let bytes = read
            .get(&stat_key(
                self.namespace.as_bytes(),
                code,
                Some(edge_type.as_bytes()),
            ))?
            .ok_or(GraphError::NotFound)?;
        let (at_us, _edges, buckets, exact) = decode_degree(&bytes)?;
        Ok(Statistic {
            value: bucket_quantile(&buckets, quantile),
            exact,
            as_of: StatAsOf::Sampled { at_us },
        })
    }

    /// Distinct values in `index`.
    ///
    /// A unique index uses the exact entry counter. A non-unique index uses
    /// the stored sample, or [`GraphError::NotFound`] when none has been taken.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::NotFound`], [`GraphError::Storage`], or
    /// [`GraphError::Corrupt`].
    pub fn index_ndv<R: KeyRead>(
        &self,
        read: &R,
        index: IndexId,
    ) -> Result<Statistic<u64>, GraphError> {
        let graph = GraphStore::new(self.namespace);
        let state = graph.index_state(read, index)?;
        if state.definition.unique && state.phase == IndexPhase::Ready {
            return self.index_count(read, index);
        }
        let bytes = read
            .get(&stat_key(
                self.namespace.as_bytes(),
                NDV,
                Some(index.as_bytes()),
            ))?
            .ok_or(GraphError::NotFound)?;
        let (at_us, ndv, exact) = decode_ndv(&bytes)?;
        Ok(Statistic {
            value: ndv,
            exact,
            as_of: StatAsOf::Sampled { at_us },
        })
    }

    /// Replace the degree sample for `edge_type` using up to `limit` edges.
    ///
    /// The sample is exact when `limit` covers every edge of the type. On a
    /// power-law graph a full scan matches the true quantile. A partial scan
    /// is the quantile of the lowest edge ids and can miss the hub; treat it
    /// as a lower-biased estimate, not a confidence interval.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Storage`] or [`GraphError::Corrupt`].
    pub fn refresh_degrees<V: Vfs>(
        &self,
        batch: &mut GraphTxn<V>,
        edge_type: TypeId,
        direction: Direction,
        limit: u32,
        at_us: u64,
    ) -> Result<(), GraphError> {
        let graph = GraphStore::new(self.namespace);
        let mut buckets = [0_u64; DEGREE_BUCKETS];
        let mut seen = 0_u64;
        for edge in graph.edges_with_type(batch, edge_type)? {
            if seen == u64::from(limit) {
                break;
            }
            let edge = edge?;
            let record = graph.get_edge(batch, edge)?;
            let node = if direction == Direction::Out {
                record.source
            } else {
                record.target
            };
            let degree = count_degree(batch, self.namespace, node, direction, edge_type)?;
            buckets[bucket_index(degree)] += 1;
            seen += 1;
        }
        let total = self.type_count(batch, edge_type)?.value;
        let exact = seen == total;
        let code = if direction == Direction::Out {
            DEGREE_OUT
        } else {
            DEGREE_IN
        };
        batch.put(
            stat_key(self.namespace.as_bytes(), code, Some(edge_type.as_bytes())),
            encode_degree(at_us, seen, &buckets, exact),
        )?;
        Ok(())
    }

    /// Replace the distinct-value sample for `index` using up to `limit` entries.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::InvalidIdentity`] for an unknown index, and
    /// [`GraphError::Storage`] or [`GraphError::Corrupt`].
    pub fn refresh_index_ndv<V: Vfs>(
        &self,
        batch: &mut GraphTxn<V>,
        index: IndexId,
        limit: u32,
        at_us: u64,
    ) -> Result<(), GraphError> {
        let graph = GraphStore::new(self.namespace);
        let state = graph.index_state(batch, index)?;
        let prefix = keys::index_entries(self.namespace, index, state.definition.unique);
        let mut distinct = BTreeMap::<Vec<u8>, ()>::new();
        let mut exhausted = true;
        for (seen, entry) in PrefixScan::new(batch, prefix.clone())?.enumerate() {
            if seen == usize::try_from(limit).unwrap_or(usize::MAX) {
                exhausted = false;
                break;
            }
            let (key, _) = entry?;
            let encoded = key.get(prefix.len()..).ok_or(GraphError::Corrupt)?;
            let value_bytes = if state.definition.unique {
                encoded
            } else if encoded.len() >= 16 {
                &encoded[..encoded.len() - 16]
            } else {
                return Err(GraphError::Corrupt);
            };
            distinct.insert(value_bytes.to_vec(), ());
        }
        let ndv = u64::try_from(distinct.len()).unwrap_or(u64::MAX);
        batch.put(
            stat_key(self.namespace.as_bytes(), NDV, Some(index.as_bytes())),
            encode_ndv(at_us, ndv, exhausted),
        )?;
        Ok(())
    }
}

pub(crate) fn apply<V: Vfs>(batch: &mut GraphTxn<V>) -> Result<(), GraphError> {
    let mut nodes: BTreeMap<NamespaceId, i64> = BTreeMap::new();
    let mut edges: BTreeMap<NamespaceId, i64> = BTreeMap::new();
    let mut labels: BTreeMap<(NamespaceId, LabelId), i64> = BTreeMap::new();
    let mut types: BTreeMap<(NamespaceId, TypeId), i64> = BTreeMap::new();
    for mutation in batch.mutations() {
        match mutation {
            GraphMutation::NodeCreate { namespace, .. } => {
                *nodes.entry(*namespace).or_default() += 1
            }
            GraphMutation::NodeDelete { namespace, .. } => {
                *nodes.entry(*namespace).or_default() -= 1
            }
            GraphMutation::EdgeCreate {
                namespace,
                edge_type,
                ..
            } => {
                *edges.entry(*namespace).or_default() += 1;
                *types.entry((*namespace, *edge_type)).or_default() += 1;
            }
            GraphMutation::EdgeDelete {
                namespace,
                edge_type,
                ..
            } => {
                *edges.entry(*namespace).or_default() -= 1;
                *types.entry((*namespace, *edge_type)).or_default() -= 1;
            }
            GraphMutation::LabelAdd {
                namespace, label, ..
            } => *labels.entry((*namespace, *label)).or_default() += 1,
            GraphMutation::LabelRemove {
                namespace, label, ..
            } => *labels.entry((*namespace, *label)).or_default() -= 1,
            _ => {}
        }
    }
    for (namespace, delta) in nodes {
        merge(batch, namespace, NODES, None, delta)?;
    }
    for (namespace, delta) in edges {
        merge(batch, namespace, EDGES, None, delta)?;
    }
    for ((namespace, label), delta) in labels {
        merge(batch, namespace, LABEL, Some(label.as_bytes()), delta)?;
    }
    for ((namespace, edge_type), delta) in types {
        merge(batch, namespace, TYPE, Some(edge_type.as_bytes()), delta)?;
    }

    let mut index_delta: BTreeMap<(NamespaceId, [u8; 16]), i64> = BTreeMap::new();
    let mut byte_delta: BTreeMap<NamespaceId, i64> = BTreeMap::new();
    let mut staged = Vec::new();
    batch.for_each_staged(|key, value| staged.push((key.to_vec(), value.map(<[u8]>::to_vec))));
    for (key, value) in staged {
        if key.len() < 17 || key[..16] == [0; 16] {
            continue;
        }
        let namespace = namespace_of(&key)?;
        let old = batch.snapshot_get(&key)?;
        let old_len = old.as_ref().map(|body| key.len() + body.len()).unwrap_or(0);
        let new_len = value
            .as_ref()
            .map(|body| key.len() + body.len())
            .unwrap_or(0);
        let delta =
            i64::try_from(new_len).unwrap_or(i64::MAX) - i64::try_from(old_len).unwrap_or(i64::MAX);
        if delta != 0 {
            *byte_delta.entry(namespace).or_default() += delta;
        }
        if let Some(index) = index_id(&key) {
            let existed = i64::from(old.is_some());
            let exists = i64::from(value.is_some());
            *index_delta.entry((namespace, index)).or_default() += exists - existed;
        }
    }
    for (namespace, delta) in byte_delta {
        merge(batch, namespace, BYTES, None, delta)?;
    }
    for ((namespace, index), delta) in index_delta {
        merge(batch, namespace, INDEX, Some(&index), delta)?;
    }
    Ok(())
}

fn merge<V: Vfs>(
    batch: &mut GraphTxn<V>,
    namespace: NamespaceId,
    code: u8,
    id: Option<&[u8; 16]>,
    delta: i64,
) -> Result<(), GraphError> {
    if delta == 0 {
        return Ok(());
    }
    batch.merge_add(&stat_key(namespace.as_bytes(), code, id), delta)
}

fn exact_counter<R: KeyRead>(read: &R, key: &[u8]) -> Result<Statistic<u64>, GraphError> {
    let value = match read.get(key)? {
        None => 0,
        Some(bytes) if bytes.len() == 8 => {
            let mut buf = [0_u8; 8];
            buf.copy_from_slice(&bytes);
            let signed = i64::from_be_bytes(buf);
            u64::try_from(signed.max(0)).unwrap_or(0)
        }
        Some(_) => return Err(GraphError::Corrupt),
    };
    Ok(Statistic {
        value,
        exact: true,
        as_of: StatAsOf::Snapshot,
    })
}

fn namespace_of(key: &[u8]) -> Result<NamespaceId, GraphError> {
    let mut buf = [0_u8; 16];
    buf.copy_from_slice(&key[..16]);
    NamespaceId::from_bytes(buf).map_err(|_| GraphError::Corrupt)
}

fn index_id(key: &[u8]) -> Option<[u8; 16]> {
    if key.len() < 33 || (key[16] != 0x0F && key[16] != 0x10) {
        return None;
    }
    let mut id = [0_u8; 16];
    id.copy_from_slice(&key[17..33]);
    Some(id)
}

fn count_degree<R: KeyRead>(
    read: &R,
    namespace: NamespaceId,
    node: pizzlydb_core::NodeId,
    direction: Direction,
    edge_type: TypeId,
) -> Result<u64, GraphError> {
    let prefix = keys::adjacency_toward(namespace, node, direction.byte(), Some(edge_type));
    let mut count = 0_u64;
    for entry in PrefixScan::new(read, prefix)? {
        let (key, _) = entry?;
        if key.len() == ADJACENCY_LEN {
            count += 1;
        }
    }
    Ok(count)
}

fn bucket_index(degree: u64) -> usize {
    if degree == 0 {
        0
    } else {
        (usize::try_from(degree.ilog2()).unwrap_or(0) + 1).min(DEGREE_BUCKETS - 1)
    }
}

fn bucket_quantile(buckets: &[u64; DEGREE_BUCKETS], quantile: f64) -> u64 {
    let total: u64 = buckets.iter().sum();
    if total == 0 {
        return 0;
    }
    let q = quantile.clamp(0.0, 1.0);
    let target = ((total as f64) * q).ceil().max(1.0) as u64;
    let mut seen = 0_u64;
    for (index, count) in buckets.iter().enumerate() {
        seen += count;
        if seen >= target {
            return if index == 0 { 0 } else { 1_u64 << (index - 1) };
        }
    }
    1_u64 << (DEGREE_BUCKETS - 2)
}

fn encode_degree(at_us: u64, edges: u64, buckets: &[u64; DEGREE_BUCKETS], exact: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + DEGREE_BUCKETS * 8 + 1);
    out.extend_from_slice(&at_us.to_be_bytes());
    out.extend_from_slice(&edges.to_be_bytes());
    for bucket in buckets {
        out.extend_from_slice(&bucket.to_be_bytes());
    }
    out.push(u8::from(exact));
    out
}

fn decode_degree(bytes: &[u8]) -> Result<(u64, u64, [u64; DEGREE_BUCKETS], bool), GraphError> {
    let expect = 16 + DEGREE_BUCKETS * 8 + 1;
    if bytes.len() != expect {
        return Err(GraphError::Corrupt);
    }
    let at_us = u64::from_be_bytes(bytes[0..8].try_into().map_err(|_| GraphError::Corrupt)?);
    let edges = u64::from_be_bytes(bytes[8..16].try_into().map_err(|_| GraphError::Corrupt)?);
    let mut buckets = [0_u64; DEGREE_BUCKETS];
    for (index, bucket) in buckets.iter_mut().enumerate() {
        let start = 16 + index * 8;
        *bucket = u64::from_be_bytes(
            bytes[start..start + 8]
                .try_into()
                .map_err(|_| GraphError::Corrupt)?,
        );
    }
    let exact = match bytes[expect - 1] {
        0 => false,
        1 => true,
        _ => return Err(GraphError::Corrupt),
    };
    Ok((at_us, edges, buckets, exact))
}

fn encode_ndv(at_us: u64, ndv: u64, exact: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(17);
    out.extend_from_slice(&at_us.to_be_bytes());
    out.extend_from_slice(&ndv.to_be_bytes());
    out.push(u8::from(exact));
    out
}

fn decode_ndv(bytes: &[u8]) -> Result<(u64, u64, bool), GraphError> {
    if bytes.len() != 17 {
        return Err(GraphError::Corrupt);
    }
    let at_us = u64::from_be_bytes(bytes[0..8].try_into().map_err(|_| GraphError::Corrupt)?);
    let ndv = u64::from_be_bytes(bytes[8..16].try_into().map_err(|_| GraphError::Corrupt)?);
    let exact = match bytes[16] {
        0 => false,
        1 => true,
        _ => return Err(GraphError::Corrupt),
    };
    Ok((at_us, ndv, exact))
}
