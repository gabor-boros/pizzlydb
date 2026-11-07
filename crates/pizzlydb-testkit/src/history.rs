//! Isolation history checker.
//!
//! The checker infers write-write, write-read, and read-write edges from a
//! recorded history and reports a cycle. Predicate reads add a phantom edge
//! when a committed write inside the predicate is missing from the observed
//! result. Real-time order is an extra edge: a transaction that finishes
//! before another starts must not be ordered after it.

use std::collections::{BTreeMap, BTreeSet};

/// Anomaly classes named by ADR-004.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnomalyClass {
    /// Write-write cycle.
    G0,
    /// Read of an aborted write.
    G1a,
    /// Read of an intermediate write.
    G1b,
    /// Circular information flow.
    G1c,
    /// Read skew of one item.
    GSingle,
    /// Item-level anti-dependency cycle.
    G2Item,
    /// Predicate anti-dependency cycle.
    G2Phantom,
    /// Commit order contradicts real time.
    RealTime,
}

/// One recorded operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistOp {
    /// Point read.
    Read {
        /// Key.
        key: Vec<u8>,
        /// Observed value. `None` is absence.
        value: Option<Vec<u8>>,
    },
    /// Point write.
    Write {
        /// Key.
        key: Vec<u8>,
        /// Written value.
        value: Vec<u8>,
    },
    /// Inclusive-exclusive scan.
    Scan {
        /// First key.
        start: Vec<u8>,
        /// Exclusive end.
        end: Vec<u8>,
        /// Observed pairs.
        rows: Vec<(Vec<u8>, Vec<u8>)>,
    },
}

/// One transaction in a history.
#[derive(Clone, Debug)]
pub struct HistTxn {
    /// Caller id.
    pub id: u64,
    /// Operations in execution order.
    pub ops: Vec<HistOp>,
    /// Commit sequence, or `None` when the transaction aborted.
    pub commit: Option<u64>,
    /// Real-time start, in an arbitrary monotonic unit.
    pub started: u64,
    /// Real-time finish.
    pub finished: u64,
}

/// A cycle the checker can show to a caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cycle {
    /// Anomaly class.
    pub class: AnomalyClass,
    /// Transaction ids on the cycle, in order.
    pub txns: Vec<u64>,
    /// Seed the caller passed in, so the history can be replayed.
    pub seed: u64,
}

impl std::fmt::Display for Cycle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:?} cycle {:?} seed {}",
            self.class, self.txns, self.seed
        )
    }
}

/// Search `history` for an isolation cycle.
///
/// `seed` is copied into the cycle so a failure can name its reproduction input.
#[must_use]
pub fn find_anomaly(history: &[HistTxn], seed: u64) -> Option<Cycle> {
    if let Some(cycle) = aborted_read(history, seed).or_else(|| intermediate_read(history, seed)) {
        return Some(cycle);
    }
    let mut edges: BTreeMap<u64, Vec<(u64, EdgeKind)>> = BTreeMap::new();
    infer(&mut edges, history);
    let mut reported: Option<Cycle> = None;
    for start in edges.keys().copied() {
        let mut stack = vec![start];
        let mut color = BTreeMap::new();
        if let Some(cycle) = dfs(start, &edges, &mut color, &mut stack) {
            let class = classify(&cycle, &edges);
            let found = Cycle {
                class,
                txns: cycle,
                seed,
            };
            if reported.is_none() || rank(found.class) > rank(reported.as_ref().unwrap().class) {
                reported = Some(found);
            }
        }
    }
    reported
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum EdgeKind {
    Ww,
    Wr,
    Rw,
    Predicate,
    RealTime,
}

fn aborted_read(history: &[HistTxn], seed: u64) -> Option<Cycle> {
    for txn in history.iter().filter(|txn| txn.commit.is_some()) {
        for op in &txn.ops {
            let HistOp::Read {
                key,
                value: Some(value),
            } = op
            else {
                continue;
            };
            let aborted = history.iter().find(|candidate| {
                candidate.commit.is_none()
                    && candidate.ops.iter().any(|op| {
                        matches!(op, HistOp::Write { key: written, value: body } if written == key && body == value)
                    })
            });
            let committed_writer = history.iter().any(|candidate| {
                candidate.commit.is_some()
                    && candidate.ops.iter().any(|op| {
                        matches!(op, HistOp::Write { key: written, value: body } if written == key && body == value)
                    })
            });
            if let Some(writer) = aborted
                && !committed_writer
            {
                return Some(Cycle {
                    class: AnomalyClass::G1a,
                    txns: vec![writer.id, txn.id],
                    seed,
                });
            }
        }
    }
    None
}

fn intermediate_read(history: &[HistTxn], seed: u64) -> Option<Cycle> {
    for writer in history.iter().filter(|txn| txn.commit.is_some()) {
        let mut seen: BTreeMap<&[u8], Vec<&[u8]>> = BTreeMap::new();
        for op in &writer.ops {
            if let HistOp::Write { key, value } = op {
                seen.entry(key).or_default().push(value);
            }
        }
        for (key, values) in seen {
            if values.len() < 2 {
                continue;
            }
            let intermediate = &values[..values.len() - 1];
            for reader in history {
                if reader.id == writer.id {
                    continue;
                }
                for op in &reader.ops {
                    if let HistOp::Read {
                        key: read_key,
                        value: Some(value),
                    } = op
                        && read_key.as_slice() == key
                        && intermediate.contains(&value.as_slice())
                    {
                        return Some(Cycle {
                            class: AnomalyClass::G1b,
                            txns: vec![writer.id, reader.id],
                            seed,
                        });
                    }
                }
            }
        }
    }
    None
}

fn infer(edges: &mut BTreeMap<u64, Vec<(u64, EdgeKind)>>, history: &[HistTxn]) {
    let committed: Vec<&HistTxn> = history.iter().filter(|txn| txn.commit.is_some()).collect();
    type Version<'a> = (u64, u64, &'a [u8]);
    let mut writes: BTreeMap<&[u8], Vec<Version<'_>>> = BTreeMap::new();
    for txn in &committed {
        let seq = txn.commit.expect("filtered");
        for op in &txn.ops {
            if let HistOp::Write { key, value } = op {
                writes.entry(key).or_default().push((seq, txn.id, value));
            }
        }
    }
    for versions in writes.values_mut() {
        versions.sort_by_key(|(seq, _, _)| *seq);
        for pair in versions.windows(2) {
            add(edges, pair[0].1, pair[1].1, EdgeKind::Ww);
        }
    }

    for txn in history {
        for op in &txn.ops {
            match op {
                HistOp::Read { key, value } => {
                    let versions = writes.get(key.as_slice());
                    if value.is_none() {
                        if let Some(versions) = versions {
                            for (seq, writer, _) in versions {
                                if txn.commit.is_none_or(|commit| *seq < commit) {
                                    add(edges, txn.id, *writer, EdgeKind::Rw);
                                }
                            }
                        }
                        continue;
                    }
                    let Some(value) = value else { continue };
                    let matched = versions.and_then(|versions| {
                        versions
                            .iter()
                            .rev()
                            .find(|(_, _, written)| *written == value.as_slice())
                    });
                    match matched {
                        Some((seq, writer, _)) => {
                            let writer_aborted = history
                                .iter()
                                .find(|candidate| candidate.id == *writer)
                                .is_some_and(|candidate| candidate.commit.is_none());
                            if writer_aborted {
                                add(edges, *writer, txn.id, EdgeKind::Wr);
                            } else {
                                add(edges, *writer, txn.id, EdgeKind::Wr);
                                if let Some(versions) = versions {
                                    for (later_seq, later, _) in versions {
                                        if *later_seq > *seq
                                            && txn.commit.is_none_or(|commit| *later_seq < commit)
                                        {
                                            add(edges, txn.id, *later, EdgeKind::Rw);
                                        }
                                    }
                                }
                            }
                        }
                        None => {
                            let aborted = history.iter().find(|candidate| {
                                candidate.commit.is_none()
                                    && candidate.ops.iter().any(|op| {
                                        matches!(op, HistOp::Write { key: written, value: body } if written == key && body == value)
                                    })
                            });
                            if let Some(writer) = aborted {
                                add(edges, writer.id, txn.id, EdgeKind::Wr);
                            }
                        }
                    }
                }
                HistOp::Scan { start, end, rows } => {
                    for (key, versions) in &writes {
                        if *key >= start.as_slice() && *key < end.as_slice() {
                            for (seq, writer, value) in versions {
                                let seen = rows.iter().any(|(row_key, row_value)| {
                                    row_key == key && row_value == value
                                });
                                if !seen && txn.commit.is_none_or(|commit| *seq < commit) {
                                    add(edges, txn.id, *writer, EdgeKind::Predicate);
                                }
                            }
                        }
                    }
                }
                HistOp::Write { .. } => {}
            }
        }
    }

    for earlier in history {
        for later in history {
            if earlier.id != later.id && earlier.finished < later.started {
                add(edges, earlier.id, later.id, EdgeKind::RealTime);
            }
        }
    }
}

fn add(edges: &mut BTreeMap<u64, Vec<(u64, EdgeKind)>>, from: u64, to: u64, kind: EdgeKind) {
    if from == to {
        return;
    }
    let list = edges.entry(from).or_default();
    if !list.iter().any(|(id, edge)| *id == to && *edge == kind) {
        list.push((to, kind));
    }
    edges.entry(to).or_default();
}

fn dfs(
    node: u64,
    edges: &BTreeMap<u64, Vec<(u64, EdgeKind)>>,
    color: &mut BTreeMap<u64, u8>,
    stack: &mut Vec<u64>,
) -> Option<Vec<u64>> {
    color.insert(node, 1);
    if let Some(nexts) = edges.get(&node) {
        for (next, _) in nexts {
            match color.get(next).copied().unwrap_or(0) {
                1 => {
                    let start = stack.iter().position(|id| id == next).unwrap_or(0);
                    let mut cycle = stack[start..].to_vec();
                    cycle.push(*next);
                    return Some(cycle);
                }
                0 => {
                    stack.push(*next);
                    if let Some(cycle) = dfs(*next, edges, color, stack) {
                        return Some(cycle);
                    }
                    stack.pop();
                }
                _ => {}
            }
        }
    }
    color.insert(node, 2);
    None
}

fn classify(cycle: &[u64], edges: &BTreeMap<u64, Vec<(u64, EdgeKind)>>) -> AnomalyClass {
    let mut kinds = BTreeSet::new();
    for pair in cycle.windows(2) {
        if let Some(list) = edges.get(&pair[0]) {
            for (id, kind) in list {
                if *id == pair[1] {
                    kinds.insert(*kind);
                }
            }
        }
    }
    if kinds.contains(&EdgeKind::Predicate) {
        AnomalyClass::G2Phantom
    } else if kinds.contains(&EdgeKind::RealTime) && kinds.len() == 1 {
        AnomalyClass::RealTime
    } else if kinds.contains(&EdgeKind::Rw) {
        AnomalyClass::G2Item
    } else if kinds.contains(&EdgeKind::Ww) && !kinds.contains(&EdgeKind::Wr) {
        AnomalyClass::G0
    } else if kinds.contains(&EdgeKind::Wr) {
        AnomalyClass::G1c
    } else {
        AnomalyClass::GSingle
    }
}

fn rank(class: AnomalyClass) -> u8 {
    match class {
        AnomalyClass::G2Phantom => 7,
        AnomalyClass::G2Item => 6,
        AnomalyClass::G1c => 5,
        AnomalyClass::G1b => 4,
        AnomalyClass::G1a => 3,
        AnomalyClass::G0 => 2,
        AnomalyClass::GSingle => 1,
        AnomalyClass::RealTime => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::{AnomalyClass, HistOp, HistTxn, find_anomaly};

    fn txn(id: u64, commit: Option<u64>, ops: Vec<HistOp>) -> HistTxn {
        HistTxn {
            id,
            ops,
            commit,
            started: id,
            finished: id,
        }
    }

    #[test]
    fn write_skew_is_g2_item() {
        let history = vec![
            txn(
                1,
                Some(1),
                vec![
                    HistOp::Read {
                        key: b"x".to_vec(),
                        value: Some(b"0".to_vec()),
                    },
                    HistOp::Write {
                        key: b"y".to_vec(),
                        value: b"1".to_vec(),
                    },
                ],
            ),
            txn(
                2,
                Some(2),
                vec![
                    HistOp::Read {
                        key: b"y".to_vec(),
                        value: Some(b"0".to_vec()),
                    },
                    HistOp::Write {
                        key: b"x".to_vec(),
                        value: b"1".to_vec(),
                    },
                ],
            ),
            txn(
                0,
                Some(0),
                vec![
                    HistOp::Write {
                        key: b"x".to_vec(),
                        value: b"0".to_vec(),
                    },
                    HistOp::Write {
                        key: b"y".to_vec(),
                        value: b"0".to_vec(),
                    },
                ],
            ),
        ];
        let cycle = find_anomaly(&history, 7).expect("cycle");
        assert_eq!(cycle.class, AnomalyClass::G2Item);
        assert_eq!(cycle.seed, 7);
        assert!(cycle.txns.len() >= 2);
    }

    #[test]
    fn aborted_read_is_reported() {
        let history = vec![
            txn(
                1,
                None,
                vec![HistOp::Write {
                    key: b"x".to_vec(),
                    value: b"aborted".to_vec(),
                }],
            ),
            txn(
                2,
                Some(1),
                vec![HistOp::Read {
                    key: b"x".to_vec(),
                    value: Some(b"aborted".to_vec()),
                }],
            ),
        ];
        let cycle = find_anomaly(&history, 1);
        assert!(cycle.is_some());
    }

    #[test]
    fn serial_history_has_no_cycle() {
        let history = vec![
            txn(
                1,
                Some(1),
                vec![HistOp::Write {
                    key: b"x".to_vec(),
                    value: b"1".to_vec(),
                }],
            ),
            txn(
                2,
                Some(2),
                vec![HistOp::Read {
                    key: b"x".to_vec(),
                    value: Some(b"1".to_vec()),
                }],
            ),
        ];
        assert!(find_anomaly(&history, 0).is_none());
    }
}
