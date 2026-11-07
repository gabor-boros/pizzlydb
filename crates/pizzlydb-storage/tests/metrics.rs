//! Physical counters used by the storage baseline.
//!
//! Wall-clock samples are recorded and are not compared here. `compare-bench`
//! applies the 5% median tolerance. Syncs are `MemoryVfs` copies, not a device
//! flush. On macOS, `rss_bytes` is the current resident size (`rss_is_peak` is
//! false). On Linux it is `VmHWM` (`rss_is_peak` is true).

use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;
use std::time::Instant;

use pizzlydb_storage::vfs_memory::MemoryFile;
use pizzlydb_storage::{
    Durability, FileRole, MemoryVfs, OpenOptions, Storage, Vfs, VfsError, VfsFile, wal_path,
};
use pizzlydb_testkit::{BenchReport, BenchResult, report_to_json};

const WRITES: u64 = 32;

struct CountingVfs {
    inner: MemoryVfs,
    syncs: Rc<Cell<u64>>,
}

struct CountingFile {
    inner: MemoryFile,
    syncs: Rc<Cell<u64>>,
}

struct Measured {
    report: BenchReport,
}

#[test]
fn physical_counters_match_across_two_runs() {
    let left = measure();
    let right = measure();
    assert_eq!(
        counter(&left.report, "insert_full", |item| item.wal_bytes),
        counter(&right.report, "insert_full", |item| item.wal_bytes)
    );
    assert_eq!(
        counter(&left.report, "btree_split_pages_written", |item| item
            .pages_written),
        counter(&right.report, "btree_split_pages_written", |item| item
            .pages_written)
    );
    assert_eq!(
        counter(&left.report, "insert_full", |item| item.fsyncs),
        counter(&right.report, "insert_full", |item| item.fsyncs)
    );
    let wal = counter(&left.report, "insert_full", |item| item.wal_bytes);
    assert!(wal > 64, "wal length {wal} is only a header");
    assert!(
        counter(&left.report, "btree_split_pages_written", |item| item
            .pages_written)
            > 0
    );
    assert!(counter(&left.report, "insert_full", |item| item.fsyncs) > 0);
    let after_checkpoint = counter(&left.report, "point_read_after_checkpoint_ns", |item| {
        item.wal_bytes
    });
    let after_extra = counter(&left.report, "point_read_after_extra_wal_ns", |item| {
        item.wal_bytes
    });
    assert!(
        after_extra > after_checkpoint,
        "extra commits did not grow the WAL"
    );
    if let Ok(path) = std::env::var("PIZZLYDB_BASELINE_OUT") {
        std::fs::write(path, report_to_json(&left.report).expect("json")).expect("write report");
    }
}

fn counter(report: &BenchReport, name: &str, pick: impl Fn(&BenchResult) -> u64) -> u64 {
    let item = report
        .results
        .iter()
        .find(|item| item.name == name)
        .expect("named result");
    pick(item)
}

fn measure() -> Measured {
    let syncs = Rc::new(Cell::new(0u64));
    let vfs = CountingVfs {
        inner: MemoryVfs::new(),
        syncs: Rc::clone(&syncs),
    };
    let db = Storage::create(&vfs, Path::new("db"), 32, Durability::Full, |id| id.fill(1))
        .expect("create");
    let mut samples = Vec::new();
    for index in 0..WRITES {
        let started = Instant::now();
        let mut batch = db.begin_write().expect("batch");
        let key = index.to_be_bytes();
        batch.put(&key, &key).expect("put");
        batch.commit(Durability::Full).expect("commit");
        samples.push(u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX));
    }
    let view = db.snapshot().expect("snapshot");
    assert_eq!(
        view.get(&0u64.to_be_bytes()).expect("get").as_deref(),
        Some(0u64.to_be_bytes().as_slice())
    );
    drop(view);
    let wal = wal_len(&vfs.inner);
    let before = db.stats();
    db.checkpoint().expect("checkpoint");
    let after = db.stats();
    let wal_after_checkpoint = wal_len(&vfs.inner);
    let cold = point_read_samples(&db, 0);
    for index in 0..WRITES * 4 {
        let mut batch = db.begin_write().expect("batch");
        let key = (WRITES + index).to_be_bytes();
        batch.put(&key, &key).expect("put");
        batch.commit(Durability::Full).expect("commit");
    }
    let wal_after_extra = wal_len(&vfs.inner);
    let warm = point_read_samples(&db, 0);
    let (rss_bytes, rss_is_peak) = rss();
    let pages_written = after.write_io().saturating_sub(before.write_io());
    let mutations = f64::from(u32::try_from(WRITES).unwrap_or(u32::MAX));
    let stats = db.stats();
    Measured {
        report: BenchReport {
            toolchain: std::env::var("PIZZLYDB_RUSTC").unwrap_or_else(|_| "unknown".to_owned()),
            seed: 1,
            results: vec![
                latency(
                    "insert_full",
                    &samples,
                    wal,
                    wal as f64 / mutations,
                    syncs.get(),
                    before.read_io(),
                    0,
                    ratio(before.hits(), before.misses()),
                    rss_bytes,
                    rss_is_peak,
                ),
                BenchResult {
                    name: "btree_split_pages_written".to_owned(),
                    p50_ns: 0,
                    p95_ns: 0,
                    p99_ns: 0,
                    throughput_per_s: 0.0,
                    pages_read: after.read_io(),
                    pages_written,
                    wal_bytes: wal,
                    wal_bytes_per_mutation: wal as f64 / mutations,
                    fsyncs: syncs.get(),
                    rss_bytes,
                    rss_is_peak,
                    hit_ratio: ratio(after.hits(), after.misses()),
                    samples: 1,
                },
                latency(
                    "point_read_after_checkpoint_ns",
                    &cold,
                    wal_after_checkpoint,
                    0.0,
                    syncs.get(),
                    stats.read_io(),
                    stats.write_io(),
                    ratio(stats.hits(), stats.misses()),
                    rss_bytes,
                    rss_is_peak,
                ),
                latency(
                    "point_read_after_extra_wal_ns",
                    &warm,
                    wal_after_extra,
                    0.0,
                    syncs.get(),
                    db.stats().read_io(),
                    db.stats().write_io(),
                    ratio(db.stats().hits(), db.stats().misses()),
                    rss_bytes,
                    rss_is_peak,
                ),
            ],
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn latency(
    name: &str,
    samples: &[u64],
    wal: u64,
    wal_per: f64,
    fsyncs: u64,
    pages_read: u64,
    pages_written: u64,
    hit_ratio: f64,
    rss_bytes: u64,
    rss_is_peak: bool,
) -> BenchResult {
    let elapsed = samples.iter().copied().sum::<u64>().max(1);
    BenchResult {
        name: name.to_owned(),
        p50_ns: percentile(samples, 0.50),
        p95_ns: percentile(samples, 0.95),
        p99_ns: percentile(samples, 0.99),
        throughput_per_s: (samples.len() as f64) * 1_000_000_000.0 / (elapsed as f64),
        pages_read,
        pages_written,
        wal_bytes: wal,
        wal_bytes_per_mutation: wal_per,
        fsyncs,
        rss_bytes,
        rss_is_peak,
        hit_ratio,
        samples: samples.len() as u64,
    }
}

fn point_read_samples(db: &Storage<CountingVfs>, key: u64) -> Vec<u64> {
    let mut samples = Vec::with_capacity(32);
    for _ in 0..32 {
        let started = Instant::now();
        let view = db.snapshot().expect("snapshot");
        let found = view.get(&key.to_be_bytes()).expect("get");
        assert!(found.is_some());
        samples.push(u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX));
    }
    samples
}

fn wal_len(vfs: &MemoryVfs) -> u64 {
    vfs.open(
        &wal_path(Path::new("db")),
        FileRole::Wal,
        &OpenOptions::new(),
    )
    .and_then(|file| file.len())
    .unwrap_or(0)
}

fn ratio(hits: u64, misses: u64) -> f64 {
    let total = hits.saturating_add(misses);
    if total == 0 {
        0.0
    } else {
        hits as f64 / total as f64
    }
}

fn percentile(samples: &[u64], quantile: f64) -> u64 {
    if samples.is_empty() {
        return 0;
    }
    let mut ordered = samples.to_vec();
    ordered.sort_unstable();
    let last = ordered.len() - 1;
    let index = ((last as f64) * quantile).round() as usize;
    ordered[index.min(last)]
}

fn rss() -> (u64, bool) {
    #[cfg(target_os = "linux")]
    {
        if let Ok(text) = std::fs::read_to_string("/proc/self/status") {
            for line in text.lines() {
                if let Some(rest) = line.strip_prefix("VmHWM:") {
                    let kb = rest
                        .split_whitespace()
                        .next()
                        .unwrap_or("0")
                        .parse::<u64>()
                        .unwrap_or(0);
                    return (kb.saturating_mul(1024), true);
                }
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
        {
            let text = String::from_utf8_lossy(&output.stdout);
            let kb = text
                .split_whitespace()
                .next()
                .unwrap_or("0")
                .parse::<u64>()
                .unwrap_or(0);
            return (kb.saturating_mul(1024), false);
        }
    }
    (0, false)
}

impl Vfs for CountingVfs {
    type File = CountingFile;

    fn open(
        &self,
        path: &Path,
        role: FileRole,
        options: &OpenOptions,
    ) -> Result<Self::File, VfsError> {
        Ok(CountingFile {
            inner: self.inner.open(path, role, options)?,
            syncs: Rc::clone(&self.syncs),
        })
    }

    fn sync_dir(&self, path: &Path) -> Result<(), VfsError> {
        self.inner.sync_dir(path)
    }

    fn rename(&self, from: &Path, to: &Path, role: FileRole) -> Result<(), VfsError> {
        self.inner.rename(from, to, role)
    }

    fn remove(&self, path: &Path, role: FileRole) -> Result<(), VfsError> {
        self.inner.remove(path, role)
    }
}

impl VfsFile for CountingFile {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, VfsError> {
        self.inner.read_at(offset, buf)
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<usize, VfsError> {
        self.inner.write_at(offset, data)
    }

    fn sync(&mut self) -> Result<(), VfsError> {
        self.inner.sync()?;
        self.syncs.set(self.syncs.get().saturating_add(1));
        Ok(())
    }

    fn len(&self) -> Result<u64, VfsError> {
        self.inner.len()
    }

    fn close(self) -> Result<(), VfsError> {
        self.inner.close()
    }

    fn role(&self) -> FileRole {
        self.inner.role()
    }
}
