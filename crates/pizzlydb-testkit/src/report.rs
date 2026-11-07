//! Machine-readable benchmark results.
//!
//! Wall-clock fields are nanoseconds. Page and byte fields are counts from
//! the storage engine, not estimates. `rss_is_peak` is false when the process
//! could only read its current resident size.

use serde::{Deserialize, Serialize};

/// One benchmark invocation.
///
/// # Examples
///
/// ```
/// use pizzlydb_testkit::{BenchReport, BenchResult};
///
/// let report = BenchReport {
///     toolchain: "1.98.1".to_owned(),
///     seed: 1,
///     results: vec![BenchResult {
///         name: "put".to_owned(),
///         p50_ns: 100,
///         p95_ns: 200,
///         p99_ns: 300,
///         throughput_per_s: 10.0,
///         pages_read: 1,
///         pages_written: 2,
///         wal_bytes: 64,
///         wal_bytes_per_mutation: 64.0,
///         fsyncs: 1,
///         rss_bytes: 0,
///         rss_is_peak: false,
///         hit_ratio: 0.5,
///         samples: 1,
///     }],
/// };
/// assert_eq!(report.results[0].name, "put");
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BenchReport {
    /// `rustc` version that produced the numbers.
    pub toolchain: String,
    /// Dataset seed.
    pub seed: u64,
    /// One entry per measured operation.
    pub results: Vec<BenchResult>,
}

/// Latencies and physical counts for one operation.
///
/// # Examples
///
/// ```
/// use pizzlydb_testkit::BenchResult;
///
/// let result = BenchResult {
///     name: "read".to_owned(),
///     p50_ns: 1,
///     p95_ns: 2,
///     p99_ns: 3,
///     throughput_per_s: 1.0,
///     pages_read: 0,
///     pages_written: 0,
///     wal_bytes: 0,
///     wal_bytes_per_mutation: 0.0,
///     fsyncs: 0,
///     rss_bytes: 0,
///     rss_is_peak: false,
///     hit_ratio: 1.0,
///     samples: 4,
/// };
/// assert_eq!(result.samples, 4);
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BenchResult {
    /// Operation name.
    pub name: String,
    /// Median latency.
    pub p50_ns: u64,
    /// 95th percentile latency.
    pub p95_ns: u64,
    /// 99th percentile latency.
    pub p99_ns: u64,
    /// Operations per second over the measured window.
    pub throughput_per_s: f64,
    /// Main-file page reads.
    pub pages_read: u64,
    /// Main-file page writes.
    pub pages_written: u64,
    /// WAL length after the measured mutations.
    pub wal_bytes: u64,
    /// WAL bytes divided by the mutation count.
    pub wal_bytes_per_mutation: f64,
    /// Successful file syncs.
    pub fsyncs: u64,
    /// Resident set size in bytes.
    pub rss_bytes: u64,
    /// True when `rss_bytes` is a high-water mark.
    pub rss_is_peak: bool,
    /// Cache hits divided by hits plus misses. Zero when both are zero.
    pub hit_ratio: f64,
    /// Timed samples.
    pub samples: u64,
}

/// Names whose median latency moved by more than `tolerance`.
///
/// `tolerance` is a fraction of the left-hand median. `0.05` is five percent.
/// A name present on only one side is reported too. Physical counters are not
/// compared here; callers assert those directly when they must be identical.
///
/// # Examples
///
/// ```
/// use pizzlydb_testkit::{BenchReport, BenchResult, compare_reports};
///
/// let left = sample("read", 100);
/// let right = sample("read", 106);
/// assert_eq!(compare_reports(&left, &right, 0.05).len(), 1);
/// assert!(compare_reports(&left, &right, 0.10).is_empty());
///
/// fn sample(name: &str, p50: u64) -> BenchReport {
///     BenchReport {
///         toolchain: "test".to_owned(),
///         seed: 1,
///         results: vec![BenchResult {
///             name: name.to_owned(),
///             p50_ns: p50,
///             p95_ns: p50,
///             p99_ns: p50,
///             throughput_per_s: 1.0,
///             pages_read: 0,
///             pages_written: 0,
///             wal_bytes: 0,
///             wal_bytes_per_mutation: 0.0,
///             fsyncs: 0,
///             rss_bytes: 0,
///             rss_is_peak: false,
///             hit_ratio: 0.0,
///             samples: 1,
///         }],
///     }
/// }
/// ```
#[must_use]
pub fn compare_reports(left: &BenchReport, right: &BenchReport, tolerance: f64) -> Vec<String> {
    let mut notes = Vec::new();

    for item in &left.results {
        let Some(other) = right
            .results
            .iter()
            .find(|candidate| candidate.name == item.name)
        else {
            notes.push(format!("missing {}", item.name));
            continue;
        };

        let base = item.p50_ns.max(1) as f64;
        let delta = (item.p50_ns as f64 - other.p50_ns as f64).abs() / base;
        if delta > tolerance {
            notes.push(format!(
                "{} p50 differs by {:.1}%",
                item.name,
                delta * 100.0
            ));
        }
    }

    for item in &right.results {
        if !left
            .results
            .iter()
            .any(|candidate| candidate.name == item.name)
        {
            notes.push(format!("unexpected {}", item.name));
        }
    }

    notes
}

/// Read a report from JSON.
///
/// # Errors
///
/// Returns the JSON error.
///
/// # Examples
///
/// ```
/// use pizzlydb_testkit::{BenchReport, report_from_json};
///
/// let parsed = report_from_json(r#"{"toolchain":"t","seed":1,"results":[]}"#).unwrap();
/// assert_eq!(parsed, BenchReport { toolchain: "t".into(), seed: 1, results: Vec::new() });
/// ```
pub fn report_from_json(bytes: &str) -> Result<BenchReport, serde_json::Error> {
    serde_json::from_str(bytes)
}

/// Serialize `report`.
///
/// # Errors
///
/// Returns the JSON error.
///
/// # Examples
///
/// ```
/// use pizzlydb_testkit::{BenchReport, report_to_json};
///
/// let json = report_to_json(&BenchReport {
///     toolchain: "t".into(),
///     seed: 1,
///     results: Vec::new(),
/// }).unwrap();
/// assert!(json.contains("\"seed\""));
/// ```
pub fn report_to_json(report: &BenchReport) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(report)
}
