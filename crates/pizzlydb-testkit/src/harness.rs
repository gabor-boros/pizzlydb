//! Apply one history to a subject and to the reference graph.

use std::fmt;

use crate::reference::{GraphSnapshot, History, Operation, ReferenceGraph, StepError};

/// Engine or model that can apply a history.
///
/// `reset` discards previous state. `apply` reports the same [`StepError`]
/// variant as [`ReferenceGraph`] for the same operation. `snapshot` is the
/// observable state after that call.
pub trait Subject {
    /// Start from an empty database.
    fn reset(&mut self);

    /// Apply one operation.
    ///
    /// # Errors
    ///
    /// Returns the step error. The subject's observable state stays as it was
    /// before the call.
    fn apply(&mut self, op: &Operation) -> Result<(), StepError>;

    /// Observable state.
    ///
    /// # Errors
    ///
    /// Returns [`StepError::Other`] when the subject cannot be read.
    fn snapshot(&self) -> Result<GraphSnapshot, StepError>;
}

impl Subject for ReferenceGraph {
    fn reset(&mut self) {
        ReferenceGraph::reset(self);
    }

    fn apply(&mut self, op: &Operation) -> Result<(), StepError> {
        ReferenceGraph::apply(self, op)
    }

    fn snapshot(&self) -> Result<GraphSnapshot, StepError> {
        ReferenceGraph::snapshot(self)
    }
}

/// First difference between the reference graph and `subject`.
///
/// Display starts with `PIZZLYDB_SEED=<seed>`.
///
/// # Examples
///
/// ```
/// use pizzlydb_testkit::{History, Operation, ReferenceGraph, run_history};
///
/// let history = History {
///     seed: 4,
///     ops: vec![Operation::CreateNode { namespace: 1 }],
/// };
/// let mut model = ReferenceGraph::new();
/// assert!(run_history(&mut model, &history).is_ok());
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct Divergence {
    /// [`History::seed`](crate::History::seed).
    pub seed: u64,
    /// Index of the operation that disagreed.
    pub index: usize,
    /// The operation that disagreed.
    pub operation: Operation,
    /// What the two sides did.
    pub detail: String,
}

impl fmt::Display for Divergence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "PIZZLYDB_SEED={}", self.seed)?;
        writeln!(formatter, "diverged at operation {}", self.index)?;
        writeln!(formatter, "operation: {:?}", self.operation)?;
        write!(formatter, "{}", self.detail)
    }
}

/// Apply `history` to `subject` and to a fresh reference graph.
///
/// The subject is reset first. After every operation the error variant and
/// the snapshot must match. The first mismatch is returned.
///
/// # Errors
///
/// Returns the first divergence.
///
/// # Examples
///
/// ```
/// use pizzlydb_testkit::{Operation, ReferenceGraph, WorkloadBias, generate_history, run_history};
///
/// let history = generate_history(3, 4, &WorkloadBias::default());
/// let mut oracle = ReferenceGraph::new();
/// run_history(&mut oracle, &history).unwrap();
/// ```
#[allow(clippy::result_large_err)]
pub fn run_history<S: Subject>(subject: &mut S, history: &History) -> Result<(), Divergence> {
    subject.reset();
    let mut model = ReferenceGraph::new();
    for (index, op) in history.ops.iter().enumerate() {
        let model_result = model.apply(op);
        let subject_result = subject.apply(op);
        if class(&model_result) != class(&subject_result) {
            return Err(diverge(
                history,
                index,
                op,
                format!("model {model_result:?}, subject {subject_result:?}"),
            ));
        }

        let model_snapshot = model
            .snapshot()
            .map_err(|err| diverge(history, index, op, format!("model snapshot: {err}")))?;

        let subject_snapshot = subject
            .snapshot()
            .map_err(|err| diverge(history, index, op, format!("subject snapshot: {err}")))?;

        if model_snapshot != subject_snapshot {
            return Err(diverge(
                history,
                index,
                op,
                format!(
                    "snapshots differ\nmodel: {model_snapshot:?}\nsubject: {subject_snapshot:?}"
                ),
            ));
        }
    }
    Ok(())
}

/// Drop operations until the failure disappears, then keep the smallest prefix
/// that still fails.
///
/// `history` that does not fail is returned unchanged.
///
/// # Examples
///
/// ```
/// use pizzlydb_testkit::{History, ReferenceGraph, minimize};
///
/// let history = History { seed: 1, ops: Vec::new() };
/// let mut model = ReferenceGraph::new();
/// assert!(minimize(&mut model, &history).ops.is_empty());
/// ```
pub fn minimize<S: Subject>(subject: &mut S, history: &History) -> History {
    if run_history(subject, history).is_ok() {
        return history.clone();
    }

    let mut current = history.ops.clone();
    let mut parts = 2usize;

    while current.len() > 1 {
        let chunks = split(&current, parts);
        let mut reduced = false;
        for chunk in &chunks {
            if fails(subject, history.seed, chunk) {
                current = chunk.clone();
                parts = 2;
                reduced = true;
                break;
            }
        }

        if reduced {
            continue;
        }

        for index in 0..chunks.len() {
            let candidate = without(&chunks, index);
            if fails(subject, history.seed, &candidate) {
                current = candidate;
                parts = parts.saturating_sub(1).max(2);
                reduced = true;
                break;
            }
        }

        if reduced {
            continue;
        }

        if parts >= current.len() {
            break;
        }

        parts = (parts.saturating_mul(2)).min(current.len());
    }

    History {
        seed: history.seed,
        ops: current,
    }
}

fn fails<S: Subject>(subject: &mut S, seed: u64, ops: &[Operation]) -> bool {
    let history = History {
        seed,
        ops: ops.to_vec(),
    };
    run_history(subject, &history).is_err()
}

fn split(ops: &[Operation], parts: usize) -> Vec<Vec<Operation>> {
    let parts = parts.max(1).min(ops.len());
    let width = ops.len().div_ceil(parts);
    ops.chunks(width).map(<[Operation]>::to_vec).collect()
}

fn without(chunks: &[Vec<Operation>], skip: usize) -> Vec<Operation> {
    chunks
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != skip)
        .flat_map(|(_, chunk)| chunk.clone())
        .collect()
}

fn class(result: &Result<(), StepError>) -> Option<String> {
    match result {
        Ok(()) => None,
        Err(StepError::Other(message)) => Some(format!("other:{message}")),
        Err(err) => Some(format!("{err:?}")),
    }
}

fn diverge(history: &History, index: usize, op: &Operation, detail: String) -> Divergence {
    Divergence {
        seed: history.seed,
        index,
        operation: op.clone(),
        detail,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{Subject, minimize, run_history};
    use crate::generate::{WorkloadBias, generate_history};
    use crate::reference::{
        DIR_OUT, GraphSnapshot, History, NeighborFact, Operation, ReferenceGraph, StepError,
    };

    struct SkipAdjacency {
        inner: ReferenceGraph,
        extra: BTreeSet<NeighborFact>,
    }

    impl Subject for SkipAdjacency {
        fn reset(&mut self) {
            self.inner.reset();
            self.extra.clear();
        }

        fn apply(&mut self, op: &Operation) -> Result<(), StepError> {
            let before = match op {
                Operation::DeleteEdge { namespace, edge } => self
                    .inner
                    .edge(*namespace, *edge)
                    .map(|(source, target, edge_type)| {
                        (*namespace, *edge, source, target, edge_type)
                    }),
                _ => None,
            };
            let result = self.inner.apply(op);
            if result.is_ok()
                && let Some((namespace, edge, source, target, edge_type)) = before
            {
                self.extra.insert(NeighborFact {
                    namespace,
                    node: source,
                    direction: DIR_OUT,
                    edge_type,
                    neighbor: target,
                    edge,
                });
            }
            result
        }

        fn snapshot(&self) -> Result<GraphSnapshot, StepError> {
            let mut snapshot = self.inner.snapshot()?;
            snapshot.neighbors.extend(self.extra.iter().cloned());
            Ok(snapshot)
        }
    }

    #[test]
    fn a_skipped_adjacency_key_is_minimized() {
        let mut ops = vec![
            Operation::InternType {
                namespace: 1,
                name: "KNOWS".into(),
            },
            Operation::CreateNode { namespace: 1 },
            Operation::CreateEdge {
                namespace: 1,
                source: 1,
                target: 1,
                edge_type: 1,
            },
            Operation::DeleteEdge {
                namespace: 1,
                edge: 1,
            },
        ];
        for index in 0..12 {
            ops.push(Operation::InternLabel {
                namespace: 1,
                name: format!("Pad{index}"),
            });
        }
        let history = History { seed: 11, ops };
        let mut subject = SkipAdjacency {
            inner: ReferenceGraph::new(),
            extra: BTreeSet::new(),
        };
        let err = run_history(&mut subject, &history).unwrap_err();
        assert!(err.to_string().contains("PIZZLYDB_SEED=11"));
        let minimal = minimize(&mut subject, &history);
        assert!(
            minimal.ops.len() <= 10,
            "minimized to {}",
            minimal.ops.len()
        );
        assert!(run_history(&mut subject, &minimal).is_err());
    }

    #[test]
    fn generated_histories_agree_with_themselves() {
        let bias = WorkloadBias::default();
        for seed in 0..25 {
            let history = generate_history(seed, 16, &bias);
            let mut model = ReferenceGraph::new();
            run_history(&mut model, &history).unwrap();
        }
    }
}
