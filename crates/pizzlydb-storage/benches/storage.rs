//! Storage micro-benchmarks and one physical sample.
//!
//! `PIZZLYDB_BENCH_QUICK=1` shortens Criterion's sample window.
//! `PIZZLYDB_BENCH_OUT=path.json` writes a [`pizzlydb_testkit::BenchReport`].
//! Warm cache timings reuse one database. Cold timings open a new one.
//! Syncs here are `MemoryVfs` copies, not a device flush. POSIX fsync counts
//! are whatever the counting wrapper records when a caller swaps the backend.

use std::hint::black_box;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use pizzlydb_storage::vfs_memory::MemoryFile;
use pizzlydb_storage::{
    Durability, FileRole, MemoryVfs, OpenOptions, PagerStats, Storage, Vfs, VfsError, VfsFile,
    wal_path,
};
use pizzlydb_testkit::{BenchReport, BenchResult, report_to_json};

fn bench_config() -> Criterion {
    if std::env::var_os("PIZZLYDB_BENCH_QUICK").is_some() {
        Criterion::default()
            .sample_size(10)
            .warm_up_time(Duration::from_millis(20))
            .measurement_time(Duration::from_millis(50))
    } else {
        Criterion::default()
    }
}

fn put_commit(c: &mut Criterion) {
    c.bench_function("storage_put_commit_none", |b| {
        b.iter_batched(
            || create(Durability::None),
            |db| {
                let mut batch = db.begin_write().expect("batch");
                batch.put(b"k", b"v").expect("put");
                batch.commit(Durability::None).expect("commit");
                let view = db.snapshot().expect("snapshot");
                assert_eq!(
                    view.get(b"k").expect("get").as_deref(),
                    Some(b"v".as_slice())
                );
            },
            BatchSize::SmallInput,
        );
    });
}

fn point_read_warm(c: &mut Criterion) {
    let db = create(Durability::None);
    let mut batch = db.begin_write().expect("batch");
    batch.put(b"k", b"v").expect("put");
    batch.commit(Durability::None).expect("commit");
    let _ = db.snapshot().expect("warm").get(b"k");
    c.bench_function("storage_point_read_warm", |b| {
        b.iter(|| {
            let view = db.snapshot().expect("snapshot");
            let value = view.get(b"k").expect("get");
            assert!(value.is_some());
            black_box(value);
        });
    });
}

fn physical(c: &mut Criterion) {
    let report = physical_report();
    if let Ok(path) = std::env::var("PIZZLYDB_BENCH_OUT") {
        let json = report_to_json(&report).expect("json");
        std::fs::write(&path, json).expect("write report");
    }
    c.bench_function("storage_physical_sample", |b| {
        b.iter(|| {
            let db = create(Durability::None);
            let mut batch = db.begin_write().expect("batch");
            batch.put(b"k", b"v").expect("put");
            black_box(batch.commit(Durability::None).expect("commit"));
        });
    });
}

fn physical_report() -> BenchReport {
    let counts = Arc::new(AtomicU64::new(0));
    let vfs = CountingVfs {
        inner: MemoryVfs::new(),
        syncs: Arc::clone(&counts),
    };
    let db = Storage::create(&vfs, Path::new("db"), 32, Durability::Full, |id| id.fill(1))
        .expect("create");
    let mut samples = Vec::new();
    let writes = if std::env::var_os("PIZZLYDB_BENCH_QUICK").is_some() {
        32
    } else {
        200
    };
    for index in 0..writes {
        let started = Instant::now();
        let mut batch = db.begin_write().expect("batch");
        let key = (index as u64).to_be_bytes();
        batch.put(&key, &key).expect("put");
        batch.commit(Durability::Full).expect("commit");
        samples.push(u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX));
    }
    let wal = wal_len(&vfs.inner);
    let before = db.stats();
    db.checkpoint().expect("checkpoint");
    let after = db.stats();
    let pages_written = after.write_io().saturating_sub(before.write_io());
    let wal_after_checkpoint = wal_len(&vfs.inner);
    let cold = point_read_nanos(&db, 0);
    for index in 0..writes * 4 {
        let mut batch = db.begin_write().expect("batch");
        let key = ((writes as u64) + (index as u64)).to_be_bytes();
        batch.put(&key, &key).expect("put");
        batch.commit(Durability::Full).expect("commit");
    }
    let wal_after_extra = wal_len(&vfs.inner);
    let warm_after_wal = point_read_nanos(&db, 0);
    let stats = db.stats();
    let (rss_bytes, rss_is_peak) = rss();
    let mutations = writes as f64;
    BenchReport {
        toolchain: rustc_version(),
        seed: 1,
        results: vec![
            result(
                "insert_full",
                &samples,
                wal,
                wal as f64 / mutations,
                counts.load(Ordering::Relaxed),
                before.read_io(),
                0,
                &before,
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
                fsyncs: counts.load(Ordering::Relaxed),
                rss_bytes,
                rss_is_peak,
                hit_ratio: ratio(&after),
                samples: 1,
            },
            BenchResult {
                name: "point_read_after_checkpoint_ns".to_owned(),
                p50_ns: cold,
                p95_ns: cold,
                p99_ns: cold,
                throughput_per_s: 0.0,
                pages_read: stats.read_io(),
                pages_written: stats.write_io(),
                wal_bytes: wal_after_checkpoint,
                wal_bytes_per_mutation: 0.0,
                fsyncs: counts.load(Ordering::Relaxed),
                rss_bytes,
                rss_is_peak,
                hit_ratio: ratio(&stats),
                samples: 1,
            },
            BenchResult {
                name: "point_read_after_extra_wal_ns".to_owned(),
                p50_ns: warm_after_wal,
                p95_ns: warm_after_wal,
                p99_ns: warm_after_wal,
                throughput_per_s: 0.0,
                pages_read: db.stats().read_io(),
                pages_written: db.stats().write_io(),
                wal_bytes: wal_after_extra,
                wal_bytes_per_mutation: 0.0,
                fsyncs: counts.load(Ordering::Relaxed),
                rss_bytes,
                rss_is_peak,
                hit_ratio: ratio(&db.stats()),
                samples: 1,
            },
        ],
    }
}

#[allow(clippy::too_many_arguments)]
fn result(
    name: &str,
    samples: &[u64],
    wal: u64,
    wal_per: f64,
    fsyncs: u64,
    pages_read: u64,
    pages_written: u64,
    stats: &PagerStats,
    rss_bytes: u64,
    rss_is_peak: bool,
) -> BenchResult {
    let elapsed = samples.iter().sum::<u64>().max(1);
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
        hit_ratio: ratio(stats),
        samples: samples.len() as u64,
    }
}

fn point_read_nanos(db: &Storage<CountingVfs>, key: u64) -> u64 {
    let started = Instant::now();
    let view = db.snapshot().expect("snapshot");
    let found = view.get(&key.to_be_bytes()).expect("get");
    assert!(found.is_some());
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn create(durability: Durability) -> Storage<MemoryVfs> {
    let vfs = MemoryVfs::new();
    Storage::create(&vfs, Path::new("db"), 32, durability, |id| id.fill(1)).expect("create")
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

fn ratio(stats: &PagerStats) -> f64 {
    let hits = stats.hits();
    let total = hits.saturating_add(stats.misses());
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
    let index = (last as f64 * quantile).round() as usize;
    ordered[index.min(last)]
}

fn rustc_version() -> String {
    std::env::var("PIZZLYDB_RUSTC").unwrap_or_else(|_| "unknown".to_owned())
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

struct CountingVfs {
    inner: MemoryVfs,
    syncs: Arc<AtomicU64>,
}

struct CountingFile {
    inner: MemoryFile,
    syncs: Arc<AtomicU64>,
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
            syncs: Arc::clone(&self.syncs),
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
        self.syncs.fetch_add(1, Ordering::Relaxed);
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

criterion_group! {
    name = storage;
    config = bench_config();
    targets = put_commit, point_read_warm, physical
}
criterion_main!(storage);
