//! Snapshot bootstrap, the durable result-delta log, and resume cursors.
//!
//! This crate owns the client-facing result lifecycle. It cannot mutate
//! committed graph state.

mod bootstrap;
mod cursor;
mod output_log;
