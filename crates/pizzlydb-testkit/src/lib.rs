//! Reference graph, deterministic workloads, and benchmark reports.
//!
//! [`ReferenceGraph`] is the oracle for graph histories. [`generate_history`]
//! and [`history_strategy`] build those histories. [`run_history`] applies one
//! history to a [`Subject`] and to the oracle. Dataset helpers build graphs
//! for benchmarks and do not open a database.
//!
//! This crate depends on `pizzlydb-core` only. An engine adapter lives in the
//! crate under test and uses that crate's public API.
//!
//! # Examples
//!
//! ```
//! use pizzlydb_testkit::{ReferenceGraph, WorkloadBias, generate_history, run_history};
//!
//! let history = generate_history(1, 4, &WorkloadBias::default());
//! let mut model = ReferenceGraph::new();
//! run_history(&mut model, &history).unwrap();
//! ```

mod dataset;
mod generate;
mod harness;
mod reference;
mod report;

pub use dataset::{Dataset, power_law_graph, social_graph, uniform_graph};
pub use generate::{PROPERTY_STEPS, WorkloadBias, generate_history, history_strategy};
pub use harness::{Divergence, Subject, minimize, run_history};
pub use reference::{
    Counters, DIR_IN, DIR_OUT, DeclFact, EdgeFact, GraphSnapshot, History, IndexFact, IndexHit,
    ModelScope, NAME_KEY, NAME_LABEL, NAME_TYPE, NeighborFact, OWNER_EDGE, OWNER_NODE, Operation,
    ReferenceGraph, SCOPE_LABEL, SCOPE_TYPE, StepError,
};
pub use report::{BenchReport, BenchResult, compare_reports, report_from_json, report_to_json};
