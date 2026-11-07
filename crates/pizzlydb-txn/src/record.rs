//! Commit record stored at `sys/commit/<seq>`.
//!
//! Version 1 fields, in order: storage LSN, UTC timestamp in microseconds,
//! transaction id, namespaces touched (sorted, unique), mutation count, and
//! the reserved principal and idempotency key. Both reserved fields are empty
//! until a later issue fills them.

use pizzlydb_core::{CommitSeq, CoreError, NamespaceId};

const VERSION: u8 = 1;

/// One durable commit record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitRecord {
    /// Logical sequence.
    pub seq: CommitSeq,
    /// Storage log sequence of the same batch.
    pub lsn: u64,
    /// Unix time in microseconds. Zero when the clock is before the epoch.
    pub timestamp_us: i64,
    /// Identifier of the transaction, unique inside this process.
    pub txn_id: u128,
    /// Namespaces named by the mutation list.
    pub namespaces: Vec<NamespaceId>,
    /// Number of logical mutations in the commit.
    pub mutation_count: u32,
    /// Reserved. Empty in this version.
    pub principal: Vec<u8>,
    /// Reserved. Empty in this version.
    pub idempotency_key: Vec<u8>,
}

pub(crate) fn encode_record(record: &CommitRecord) -> Result<Vec<u8>, CoreError> {
    let namespaces =
        u16::try_from(record.namespaces.len()).map_err(|_| CoreError::InvalidMutation)?;

    let principal =
        u16::try_from(record.principal.len()).map_err(|_| CoreError::InvalidMutation)?;

    let idempotency =
        u16::try_from(record.idempotency_key.len()).map_err(|_| CoreError::InvalidMutation)?;

    let mut out = Vec::new();
    out.push(VERSION);
    out.extend_from_slice(&record.seq.to_be_bytes());
    out.extend_from_slice(&record.lsn.to_be_bytes());
    out.extend_from_slice(&record.timestamp_us.to_be_bytes());
    out.extend_from_slice(&record.txn_id.to_be_bytes());
    out.extend_from_slice(&record.mutation_count.to_be_bytes());
    out.extend_from_slice(&namespaces.to_be_bytes());

    for namespace in &record.namespaces {
        out.extend_from_slice(namespace.as_bytes());
    }

    out.extend_from_slice(&principal.to_be_bytes());
    out.extend_from_slice(&record.principal);
    out.extend_from_slice(&idempotency.to_be_bytes());
    out.extend_from_slice(&record.idempotency_key);

    Ok(out)
}

pub(crate) fn decode_record(bytes: &[u8]) -> Result<CommitRecord, CoreError> {
    let mut cur = bytes;
    let version = take(1, &mut cur)?[0];
    if version != VERSION {
        return Err(CoreError::InvalidMutation);
    }

    let seq = CommitSeq::from_be_bytes(take_array(&mut cur)?)?;
    let lsn = u64::from_be_bytes(take_array(&mut cur)?);
    let timestamp_us = i64::from_be_bytes(take_array(&mut cur)?);
    let txn_id = u128::from_be_bytes(take_array(&mut cur)?);
    let mutation_count = u32::from_be_bytes(take_array(&mut cur)?);

    let namespaces_len = u16::from_be_bytes(take_array(&mut cur)?) as usize;
    let mut namespaces = Vec::with_capacity(namespaces_len);
    for _ in 0..namespaces_len {
        let bytes = take_array::<16>(&mut cur)?;
        namespaces.push(NamespaceId::from_bytes(bytes).map_err(|_| CoreError::InvalidMutation)?);
    }

    let principal_len = u16::from_be_bytes(take_array(&mut cur)?) as usize;
    let principal = take(principal_len, &mut cur)?.to_vec();
    let idempotency_len = u16::from_be_bytes(take_array(&mut cur)?) as usize;
    let idempotency_key = take(idempotency_len, &mut cur)?.to_vec();
    if !cur.is_empty() {
        return Err(CoreError::InvalidMutation);
    }

    Ok(CommitRecord {
        seq,
        lsn,
        timestamp_us,
        txn_id,
        namespaces,
        mutation_count,
        principal,
        idempotency_key,
    })
}

fn take<'a>(len: usize, input: &mut &'a [u8]) -> Result<&'a [u8], CoreError> {
    if input.len() < len {
        return Err(CoreError::Truncated);
    }

    let (head, tail) = input.split_at(len);
    *input = tail;
    Ok(head)
}

fn take_array<const N: usize>(input: &mut &[u8]) -> Result<[u8; N], CoreError> {
    take(N, input)?.try_into().map_err(|_| CoreError::Truncated)
}

pub(crate) fn now_us() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_micros()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
