//! Decoder fuzz target. A panic is a bug.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    pizzlydb_graph::fuzz::index_definition(data);
});
