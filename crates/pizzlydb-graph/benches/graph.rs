//! Graph micro-benchmarks.
//!
//! `PIZZLYDB_BENCH_QUICK=1` uses a smaller dataset and a shorter Criterion
//! window. The fsync device is `MemoryVfs`: sync copies bytes in the process.

use std::hint::black_box;
use std::path::Path;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use pizzlydb_core::{LabelId, NamespaceId, PropertyKeyId, TypeId, Value};
use pizzlydb_graph::{Direction, GraphBatch, GraphStore, Scope};
use pizzlydb_storage::{Durability, MemoryVfs, Storage};
use pizzlydb_testkit::power_law_graph;

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

struct Loaded {
    storage: &'static Storage<MemoryVfs>,
    graph: GraphStore,
    label: LabelId,
    key: PropertyKeyId,
    index: pizzlydb_core::IndexId,
}

fn load() -> Loaded {
    let nodes = if std::env::var_os("PIZZLYDB_BENCH_QUICK").is_some() {
        16
    } else {
        128
    };
    let dataset = power_law_graph(1, nodes, nodes * 2);
    let vfs = MemoryVfs::new();
    let storage = Box::leak(Box::new(
        Storage::create(&vfs, Path::new("db"), 64, Durability::None, |id| id.fill(1))
            .expect("create"),
    ));
    let graph = GraphStore::new(NamespaceId::from_u128(1).expect("namespace"));
    let mut batch = GraphBatch::begin(storage).expect("batch");
    let knows: TypeId = graph.intern(&mut batch, "KNOWS").expect("type");
    let label: LabelId = graph.intern(&mut batch, "Person").expect("label");
    let key: PropertyKeyId = graph.intern(&mut batch, "score").expect("key");
    for _ in &dataset.nodes {
        let node = graph.create_node(&mut batch).expect("node");
        graph.add_label(&mut batch, node, label).expect("label");
        graph
            .set_property(
                &mut batch,
                node,
                key,
                &Value::Int(i64::try_from(node.to_u128()).unwrap_or(0)),
            )
            .expect("property");
    }
    for (source, target) in &dataset.edges {
        graph
            .create_edge(
                &mut batch,
                pizzlydb_core::NodeId::from_u128(*source).expect("source"),
                pizzlydb_core::NodeId::from_u128(*target).expect("target"),
                knows,
            )
            .expect("edge");
    }
    graph
        .create_edge(
            &mut batch,
            pizzlydb_core::NodeId::from_u128(2).expect("source"),
            pizzlydb_core::NodeId::from_u128(1).expect("hub"),
            knows,
        )
        .expect("hub edge");
    let index = graph
        .create_index(&mut batch, Scope::Label(label), key, false)
        .expect("index");
    batch.commit().expect("commit");
    let view = storage.snapshot().expect("snapshot");
    let neighbors = graph
        .neighbors(
            &view,
            pizzlydb_core::NodeId::from_u128(1).expect("hub"),
            Direction::In,
        )
        .expect("neighbors")
        .count();
    assert!(neighbors > 0, "the hub has no incoming edges");
    Loaded {
        storage,
        graph,
        label,
        key,
        index,
    }
}

fn expand(c: &mut Criterion) {
    let loaded = load();
    c.bench_function("graph_hub_expand", |b| {
        b.iter(|| {
            let view = loaded.storage.snapshot().expect("snapshot");
            let count = loaded
                .graph
                .neighbors(
                    &view,
                    pizzlydb_core::NodeId::from_u128(1).expect("hub"),
                    Direction::In,
                )
                .expect("neighbors")
                .count();
            assert!(count > 0);
            black_box(count);
        });
    });
}

fn lookup(c: &mut Criterion) {
    let loaded = load();
    c.bench_function("graph_index_lookup", |b| {
        b.iter(|| {
            let view = loaded.storage.snapshot().expect("snapshot");
            let hits = loaded
                .graph
                .index_lookup(&view, loaded.index, &Value::Int(1))
                .expect("lookup")
                .count();
            assert!(hits >= 1, "the indexed score is missing");
            black_box(hits);
            let _ = (loaded.label, loaded.key);
        });
    });
}

criterion_group! {
    name = graph;
    config = bench_config();
    targets = expand, lookup
}
criterion_main!(graph);
