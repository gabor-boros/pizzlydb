//! Logical mutation decode must not panic, and a successful decode round-trips.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pizzlydb_core::{decode_mutation_list, encode_mutation_list};

fuzz_target!(|data: &[u8]| {
    if let Ok(mutations) = decode_mutation_list(data) {
        let encoded = encode_mutation_list(&mutations).expect("decoded mutations encode");
        assert_eq!(
            decode_mutation_list(&encoded).expect("round trip"),
            mutations
        );
    }
});
