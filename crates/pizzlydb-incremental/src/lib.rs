//! Signed-delta operators, indexed arrangements, and maintained-state
//! checkpoints.
//!
//! This crate consumes query IR and logical graph mutations. It does not read
//! gRPC messages, and it does not decide whether a transaction committed.

mod arrangement;
mod checkpoint;
mod delta;
