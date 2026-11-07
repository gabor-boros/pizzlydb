//! Read-set and write-set overlap.
//!
//! A point read records one key. A bounded scan records the half-open
//! predicate it covered. Overlapping ranges are merged. A point that falls
//! inside a range is not stored again.

use std::collections::BTreeSet;

use crate::error::TxnError;

#[derive(Clone, Debug, Default)]
pub(crate) struct ReadSet {
    points: BTreeSet<Vec<u8>>,
    ranges: Vec<HalfOpen>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct HalfOpen {
    start: Vec<u8>,
    /// `None` means there is no upper bound.
    end: Option<Vec<u8>>,
}

impl ReadSet {
    pub(crate) fn len(&self) -> usize {
        self.points.len() + self.ranges.len()
    }

    pub(crate) fn add_point(&mut self, key: &[u8], max_entries: usize) -> Result<(), TxnError> {
        if self.covers_point(key) {
            return Ok(());
        }

        if self.len() >= max_entries {
            return Err(too_big(self.len().saturating_add(1)));
        }

        self.points.insert(key.to_vec());
        Ok(())
    }

    pub(crate) fn add_range(
        &mut self,
        start: &[u8],
        end: Option<&[u8]>,
        max_entries: usize,
    ) -> Result<(), TxnError> {
        if let Some(end) = end
            && start >= end
        {
            return Ok(());
        }

        let incoming = HalfOpen {
            start: start.to_vec(),
            end: end.map(Vec::from),
        };

        let mut kept = Vec::with_capacity(self.ranges.len());
        let mut merged = incoming;
        for range in self.ranges.drain(..) {
            if let Some(next) = merge_range(&merged, &range) {
                merged = next;
            } else {
                kept.push(range);
            }
        }

        kept.push(merged);
        kept.sort_by(|left, right| left.start.cmp(&right.start));

        self.ranges = kept;
        let ranges = self.ranges.clone();

        self.points
            .retain(|key| !ranges.iter().any(|range| point_in(key, range)));
        if self.len() > max_entries {
            return Err(too_big(self.len()));
        }

        Ok(())
    }

    pub(crate) fn conflicts_point(&self, key: &[u8]) -> bool {
        self.points.contains(key) || self.covers_point(key)
    }

    pub(crate) fn conflicts_range(&self, start: &[u8], end: &[u8]) -> bool {
        if start >= end {
            return false;
        }

        if self
            .points
            .range(start.to_vec()..end.to_vec())
            .next()
            .is_some()
        {
            return true;
        }

        self.ranges
            .iter()
            .any(|range| ranges_overlap(range, start, end))
    }

    fn covers_point(&self, key: &[u8]) -> bool {
        self.ranges.iter().any(|range| point_in(key, range))
    }
}

fn point_in(key: &[u8], range: &HalfOpen) -> bool {
    key >= range.start.as_slice() && range.end.as_ref().is_none_or(|end| key < end.as_slice())
}

fn too_big(observed: usize) -> TxnError {
    TxnError::TransactionTooLarge {
        limit: "read set",
        observed: u64::try_from(observed).unwrap_or(u64::MAX),
    }
}

fn merge_range(left: &HalfOpen, right: &HalfOpen) -> Option<HalfOpen> {
    let (first, second) = if left.start <= right.start {
        (left, right)
    } else {
        (right, left)
    };

    let overlaps = match &first.end {
        None => true,
        Some(end) => end.as_slice() >= second.start.as_slice(),
    };

    if !overlaps {
        return None;
    }

    let end = match (&first.end, &second.end) {
        (None, _) | (_, None) => None,
        (Some(left_end), Some(right_end)) => Some(if left_end >= right_end {
            left_end.clone()
        } else {
            right_end.clone()
        }),
    };

    Some(HalfOpen {
        start: first.start.clone(),
        end,
    })
}

fn ranges_overlap(range: &HalfOpen, start: &[u8], end: &[u8]) -> bool {
    let range_end_after_start = range
        .end
        .as_ref()
        .is_none_or(|range_end| range_end.as_slice() > start);

    let start_before_end = range.start.as_slice() < end;
    range_end_after_start && start_before_end
}

#[derive(Clone, Debug)]
pub(crate) struct WriteHist {
    pub(crate) points: Vec<Vec<u8>>,
    pub(crate) ranges: Vec<(Vec<u8>, Vec<u8>)>,
}

impl WriteHist {
    pub(crate) fn conflicts(&self, reads: &ReadSet) -> bool {
        self.points.iter().any(|key| reads.conflicts_point(key))
            || self
                .ranges
                .iter()
                .any(|(start, end)| reads.conflicts_range(start, end))
    }

    pub(crate) fn bytes(&self) -> usize {
        self.points.iter().map(Vec::len).sum::<usize>()
            + self
                .ranges
                .iter()
                .map(|(start, end)| start.len() + end.len())
                .sum::<usize>()
    }
}

#[cfg(test)]
mod tests {
    use super::ReadSet;

    #[test]
    fn point_inside_a_range_is_covered() {
        let mut reads = ReadSet::default();
        reads.add_range(b"a", Some(b"z"), 8).unwrap();
        reads.add_point(b"m", 8).unwrap();
        assert!(reads.points.is_empty());
        assert!(reads.conflicts_point(b"m"));
        assert!(!reads.conflicts_point(b"z"));
    }

    #[test]
    fn overlapping_ranges_merge() {
        let mut reads = ReadSet::default();
        reads.add_range(b"a", Some(b"m"), 8).unwrap();
        reads.add_range(b"m", Some(b"z"), 8).unwrap();
        assert_eq!(reads.ranges.len(), 1);
        assert!(reads.conflicts_point(b"t"));
    }

    #[test]
    fn range_write_hits_a_point_read() {
        let mut reads = ReadSet::default();
        reads.add_point(b"m", 8).unwrap();
        let hist = super::WriteHist {
            points: Vec::new(),
            ranges: vec![(b"a".to_vec(), b"z".to_vec())],
        };
        assert!(hist.conflicts(&reads));
    }
}
