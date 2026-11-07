//! Graph decoders exercised by the `fuzz/` workspace.
//!
//! Enable this module with `--features fuzzing`.

use std::hint::black_box;

use pizzlydb_core::{EdgeId, NodeId};

use crate::index::IndexDefinition;
use crate::property::PropertyDeclaration;
use crate::record::{self, Stored};

/// Node and edge record decode, plus a round trip of a live node.
pub fn record(bytes: &[u8]) {
    let node = NodeId::from_u128(1).expect("nonzero id");
    let edge = EdgeId::from_u128(1).expect("nonzero id");
    let _ = black_box(record::decode_node(node, bytes));
    let _ = black_box(record::decode_edge(edge, bytes));

    let live = record::encode_node();
    match record::decode_node(node, &live) {
        Ok(Stored::Live(decoded)) => assert_eq!(decoded.id, node),
        Ok(Stored::Deleted) | Err(_) => panic!("live node round trip failed"),
    }

    let tombstone = record::encode_tombstone();
    assert!(matches!(
        record::decode_node(node, &tombstone),
        Ok(Stored::Deleted)
    ));
}

/// Property declaration decode, and `decode(encode(x))` when `x` decodes.
pub fn declaration(bytes: &[u8]) {
    let Ok(decoded) = PropertyDeclaration::decode(bytes) else {
        return;
    };
    let again = PropertyDeclaration::decode(&decoded.encode()).expect("encoded declaration");
    assert_eq!(again, decoded);
}

/// Index definition decode, and `decode(encode(x))` when `x` decodes.
pub fn index_definition(bytes: &[u8]) {
    let Ok(decoded) = IndexDefinition::decode(bytes) else {
        return;
    };
    let again = IndexDefinition::decode(&decoded.encode()).expect("encoded index");
    assert_eq!(again, decoded);
}
