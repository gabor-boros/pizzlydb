//! MVCC snapshots, serializable validation, commit sequencing, and the logical
//! mutation log.
//!
//! Page ownership stays in `pizzlydb-storage`. This crate decides which
//! transactions committed.

mod log;
mod sequencer;
mod snapshot;
mod validate;
