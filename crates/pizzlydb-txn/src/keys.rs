//! System key space.
//!
//! Every system key starts with 16 zero bytes. That prefix is not a valid
//! [`NamespaceId`](pizzlydb_core::NamespaceId), so a graph key cannot collide
//! with it. The next byte is the system kind.
//!
//! | Kind | Key | Value |
//! | --- | --- | --- |
//! | 1 | `frontier` | last durable [`CommitSeq`](pizzlydb_core::CommitSeq), 8 bytes |
//! | 2 | `commit` + seq | versioned commit record |
//! | 3 | `log` + seq + chunk | one chunk of the logical mutation log |
//! | 4 | `floor` + id | durable retention floor |
//! | 5 | `id` + namespace + kind | reserved id high-water, two `u128`s |
//! | 6 | `format` | graph logical format version, `u16` |
//! | 7 | `ns` + id | namespace registry record |
//! | 8 | `ns_by_name` + name | namespace id |
//! | 9 | `stat` + namespace + code + optional id | counter or sample |

use pizzlydb_core::CommitSeq;

/// Width of the reserved namespace prefix.
pub const PREFIX_LEN: usize = 16;

pub(crate) const KIND_FRONTIER: u8 = 1;
pub(crate) const KIND_COMMIT: u8 = 2;
pub(crate) const KIND_LOG: u8 = 3;
pub(crate) const KIND_FLOOR: u8 = 4;
pub(crate) const KIND_ID: u8 = 5;
pub(crate) const KIND_FORMAT: u8 = 6;

/// Logical graph format written beside the first commit that uses this allocator.
pub const GRAPH_LOGICAL_VERSION: u16 = 1;

/// `true` when `key` is in the reserved system prefix.
#[must_use]
pub fn is_system_key(key: &[u8]) -> bool {
    key.len() >= PREFIX_LEN && key[..PREFIX_LEN] == [0; PREFIX_LEN]
}

pub(crate) fn system_key(kind: u8, rest: &[u8]) -> Vec<u8> {
    let mut key = Vec::with_capacity(PREFIX_LEN + 1 + rest.len());
    key.extend_from_slice(&[0; PREFIX_LEN]);
    key.push(kind);
    key.extend_from_slice(rest);
    key
}

pub(crate) fn frontier_key() -> Vec<u8> {
    system_key(KIND_FRONTIER, &[])
}

pub(crate) fn commit_key(seq: CommitSeq) -> Vec<u8> {
    system_key(KIND_COMMIT, &seq.to_be_bytes())
}

pub(crate) fn commit_prefix() -> Vec<u8> {
    system_key(KIND_COMMIT, &[])
}

pub(crate) fn commit_end() -> Vec<u8> {
    system_key(KIND_COMMIT.saturating_add(1), &[])
}

pub(crate) fn log_key(seq: CommitSeq, chunk: u32) -> Vec<u8> {
    let mut rest = Vec::with_capacity(12);
    rest.extend_from_slice(&seq.to_be_bytes());
    rest.extend_from_slice(&chunk.to_be_bytes());
    system_key(KIND_LOG, &rest)
}

pub(crate) fn log_prefix() -> Vec<u8> {
    system_key(KIND_LOG, &[])
}

pub(crate) fn log_prefix_after(seq: CommitSeq) -> Vec<u8> {
    match seq.next() {
        Ok(next) => log_key(next, 0),
        Err(_) => system_key(KIND_LOG.saturating_add(1), &[]),
    }
}

pub(crate) fn floor_key(id: u64) -> Vec<u8> {
    system_key(KIND_FLOOR, &id.to_be_bytes())
}

pub(crate) fn floor_prefix() -> Vec<u8> {
    system_key(KIND_FLOOR, &[])
}

pub(crate) fn floor_end() -> Vec<u8> {
    system_key(KIND_FLOOR.saturating_add(1), &[])
}

pub(crate) fn id_key(namespace: &[u8; 16], kind: u8) -> Vec<u8> {
    let mut rest = namespace.to_vec();
    rest.push(kind);
    system_key(KIND_ID, &rest)
}

pub(crate) fn format_key() -> Vec<u8> {
    system_key(KIND_FORMAT, &[])
}

pub(crate) const KIND_NAMESPACE: u8 = 7;
pub(crate) const KIND_NAMESPACE_NAME: u8 = 8;
pub(crate) const KIND_STAT: u8 = 9;

/// `sys/ns/<id>`.
#[must_use]
pub fn namespace_record_key(id: &[u8; 16]) -> Vec<u8> {
    system_key(KIND_NAMESPACE, id)
}

/// Prefix of every namespace record.
#[must_use]
pub fn namespace_record_prefix() -> Vec<u8> {
    system_key(KIND_NAMESPACE, &[])
}

/// Exclusive end of the namespace record range.
#[must_use]
pub fn namespace_record_end() -> Vec<u8> {
    system_key(KIND_NAMESPACE.saturating_add(1), &[])
}

/// `sys/ns_by_name/<name>`.
#[must_use]
pub fn namespace_name_key(name: &[u8]) -> Vec<u8> {
    system_key(KIND_NAMESPACE_NAME, name)
}

/// Prefix of the namespace name index.
#[must_use]
pub fn namespace_name_prefix() -> Vec<u8> {
    system_key(KIND_NAMESPACE_NAME, &[])
}

/// `sys/stat/<namespace>/<code>` plus an optional 16-byte id.
#[must_use]
pub fn stat_key(namespace: &[u8; 16], code: u8, id: Option<&[u8; 16]>) -> Vec<u8> {
    let mut rest = Vec::with_capacity(17 + id.map_or(0, |_| 16));
    rest.extend_from_slice(namespace);
    rest.push(code);
    if let Some(id) = id {
        rest.extend_from_slice(id);
    }
    system_key(KIND_STAT, &rest)
}

/// Prefix of every statistic for `namespace`.
#[must_use]
pub fn stat_prefix(namespace: &[u8; 16]) -> Vec<u8> {
    system_key(KIND_STAT, namespace)
}
