//! Logical mutation log chunks.
//!
//! A chunk is one storage value. Its key is `sys/log/<seq>/<chunk>`. The value
//! is versioned and carries the chunk index so a reader can reassemble a
//! commit without depending on map iteration order inside the chunk.

use pizzlydb_core::{
    CommitSeq, CoreError, GraphMutation, decode_mutation_list, encode_mutation_list,
};

use crate::error::TxnError;

const VERSION: u8 = 1;
const HEADER: usize = 1 + 8 + 4 + 4;

/// One commit's mutations, in log order.
#[derive(Clone, Debug, PartialEq)]
pub struct CommitLog {
    /// Commit the mutations belong to.
    pub seq: CommitSeq,
    /// Mutations in the order they were recorded.
    pub mutations: Vec<GraphMutation>,
}

pub(crate) fn chunk_mutations(
    seq: CommitSeq,
    mutations: &[GraphMutation],
    max_len: usize,
) -> Result<Vec<Vec<u8>>, TxnError> {
    if max_len <= HEADER {
        return Err(TxnError::TransactionTooLarge {
            limit: "log value",
            observed: u64::try_from(HEADER).unwrap_or(u64::MAX),
        });
    }

    let mut groups: Vec<Vec<GraphMutation>> = vec![Vec::new()];
    for mutation in mutations {
        let trial_last = groups.last().expect("a group exists");
        let mut trial = trial_last.clone();
        trial.push(mutation.clone());

        let encoded = encode_mutation_list(&trial)?;
        if HEADER + encoded.len() <= max_len {
            groups
                .last_mut()
                .expect("a group exists")
                .push(mutation.clone());
            continue;
        }

        if trial_last.is_empty() {
            return Err(TxnError::TransactionTooLarge {
                limit: "log value",
                observed: u64::try_from(HEADER + encoded.len()).unwrap_or(u64::MAX),
            });
        }

        groups.push(vec![mutation.clone()]);
        let alone = encode_mutation_list(groups.last().expect("pushed"))?;
        if HEADER + alone.len() > max_len {
            return Err(TxnError::TransactionTooLarge {
                limit: "log value",
                observed: u64::try_from(HEADER + alone.len()).unwrap_or(u64::MAX),
            });
        }
    }

    if mutations.is_empty() {
        groups = vec![Vec::new()];
    }

    let count = u32::try_from(groups.len()).map_err(|_| TxnError::Corrupt)?;
    let mut out = Vec::with_capacity(groups.len());
    for (index, group) in groups.iter().enumerate() {
        let payload = encode_mutation_list(group)?;
        let mut chunk = Vec::with_capacity(HEADER + payload.len());
        chunk.push(VERSION);
        chunk.extend_from_slice(&seq.to_be_bytes());
        chunk.extend_from_slice(&(u32::try_from(index).unwrap_or(u32::MAX)).to_be_bytes());
        chunk.extend_from_slice(&count.to_be_bytes());
        chunk.extend_from_slice(&payload);
        out.push(chunk);
    }

    Ok(out)
}

pub(crate) fn decode_chunk(
    bytes: &[u8],
) -> Result<(CommitSeq, u32, u32, Vec<GraphMutation>), CoreError> {
    if bytes.len() < HEADER {
        return Err(CoreError::Truncated);
    }

    if bytes[0] != VERSION {
        return Err(CoreError::InvalidMutation);
    }

    let seq = CommitSeq::from_be_bytes(bytes[1..9].try_into().map_err(|_| CoreError::Truncated)?)?;
    let index = u32::from_be_bytes(bytes[9..13].try_into().map_err(|_| CoreError::Truncated)?);
    let count = u32::from_be_bytes(bytes[13..17].try_into().map_err(|_| CoreError::Truncated)?);
    let mutations = decode_mutation_list(&bytes[HEADER..])?;

    Ok((seq, index, count, mutations))
}

/// Reassemble chunks of one commit. `chunks` is `(index, mutations)`.
pub(crate) fn assemble(
    mut chunks: Vec<(u32, Vec<GraphMutation>)>,
) -> Result<Vec<GraphMutation>, TxnError> {
    chunks.sort_by_key(|(index, _)| *index);
    let mut out = Vec::new();
    for (expected, (index, mutations)) in chunks.into_iter().enumerate() {
        if index as usize != expected {
            return Err(TxnError::Corrupt);
        }
        out.extend(mutations);
    }

    Ok(out)
}
