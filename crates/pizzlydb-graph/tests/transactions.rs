//! Concurrent graph transactions and the mutation log.

use std::sync::{Arc, Barrier};
use std::thread;

use pizzlydb_core::{NamespaceId, Value};
use pizzlydb_graph::{GraphError, GraphStore, GraphTxn, Scope};
use pizzlydb_storage::{Durability, MemoryVfs, Storage};
use pizzlydb_txn::{TxnError, TxnManager};

fn setup() -> (Storage<MemoryVfs>, GraphStore) {
    let vfs = MemoryVfs::new();
    let storage = Storage::create(
        &vfs,
        std::path::Path::new("g"),
        16,
        Durability::None,
        |id| id.fill(2),
    )
    .unwrap();
    // Keep the vfs alive by leaking it for the process lifetime of this test.
    std::mem::forget(vfs);
    (storage, GraphStore::new(NamespaceId::from_u128(1).unwrap()))
}

#[test]
fn namespace_zero_cannot_be_a_graph_namespace() {
    assert!(NamespaceId::from_u128(0).is_err());
    assert!(NamespaceId::from_bytes([0; 16]).is_err());
}

#[test]
fn create_node_records_one_mutation() {
    let (storage, graph) = setup();
    let mut txn = GraphTxn::begin(&storage).unwrap();
    let node = graph.create_node(&mut txn).unwrap();
    assert_eq!(txn.mutations().len(), 1);
    txn.commit().unwrap();
    let manager = TxnManager::attach(&storage).unwrap();
    let log = manager.read_log(None, 10, usize::MAX).unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].mutations.len(), 1);
    assert_eq!(log[0].mutations[0].namespace(), graph.namespace());
    let _ = node;
}

#[test]
fn unique_index_race_commits_one_writer() {
    let (storage, graph) = setup();
    let mut txn = GraphTxn::begin(&storage).unwrap();
    let label = graph.intern(&mut txn, "Person").unwrap();
    let email = graph.intern(&mut txn, "email").unwrap();
    let index = graph
        .create_index(&mut txn, Scope::Label(label), email, true)
        .unwrap();
    let left = graph.create_node(&mut txn).unwrap();
    let right = graph.create_node(&mut txn).unwrap();
    graph.add_label(&mut txn, left, label).unwrap();
    graph.add_label(&mut txn, right, label).unwrap();
    txn.commit().unwrap();

    let barrier = Arc::new(Barrier::new(2));
    let storage = Arc::new(storage);
    let mut threads = Vec::new();
    for node in [left, right] {
        let storage = Arc::clone(&storage);
        let barrier = Arc::clone(&barrier);
        threads.push(thread::spawn(move || {
            let graph = GraphStore::new(NamespaceId::from_u128(1).unwrap());
            let mut txn = GraphTxn::begin(&storage).unwrap();
            barrier.wait();
            graph
                .set_property(&mut txn, node, email, &Value::from("a@example.com"))
                .unwrap();
            txn.commit()
        }));
    }
    let mut committed = 0;
    let mut rejected = 0;
    for thread in threads {
        match thread.join().unwrap() {
            Ok(_) => committed += 1,
            Err(GraphError::Transaction(TxnError::Conflict { .. }))
            | Err(GraphError::UniqueConflict) => rejected += 1,
            Err(err) => panic!("{err}"),
        }
    }
    assert_eq!(committed, 1);
    assert_eq!(rejected, 1);
    let view = storage.snapshot().unwrap();
    let hits = graph
        .index_lookup(&view, index, &Value::from("a@example.com"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(hits.len(), 1);
}

#[test]
fn concurrent_edge_and_delete_do_not_dangle() {
    let (storage, graph) = setup();
    let mut txn = GraphTxn::begin(&storage).unwrap();
    let knows = graph.intern(&mut txn, "KNOWS").unwrap();
    let left = graph.create_node(&mut txn).unwrap();
    let right = graph.create_node(&mut txn).unwrap();
    txn.commit().unwrap();

    let barrier = Arc::new(Barrier::new(2));
    let storage = Arc::new(storage);
    let delete_storage = Arc::clone(&storage);
    let edge_storage = Arc::clone(&storage);
    let delete_barrier = Arc::clone(&barrier);
    let edge_barrier = Arc::clone(&barrier);
    let deleter = thread::spawn(move || {
        let graph = GraphStore::new(NamespaceId::from_u128(1).unwrap());
        let mut txn = GraphTxn::begin(&delete_storage).unwrap();
        delete_barrier.wait();
        graph.delete_node(&mut txn, right).unwrap();
        txn.commit()
    });
    let linker = thread::spawn(move || {
        let graph = GraphStore::new(NamespaceId::from_u128(1).unwrap());
        let mut txn = GraphTxn::begin(&edge_storage).unwrap();
        edge_barrier.wait();
        let created = graph.create_edge(&mut txn, left, right, knows);
        match created {
            Ok(_) => txn.commit().map(|_| true),
            Err(GraphError::EndpointNotLive) => Ok(false),
            Err(err) => Err(err),
        }
    });
    let _deleted = deleter.join().unwrap();
    let _linked = linker.join().unwrap();
    let view = storage.snapshot().unwrap();
    let edges = graph
        .neighbors(&view, left, pizzlydb_graph::Direction::Out)
        .unwrap()
        .count();
    if graph.get_node(&view, right).is_err() {
        assert_eq!(edges, 0);
    }
}
