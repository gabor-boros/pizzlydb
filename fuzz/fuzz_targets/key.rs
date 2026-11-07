//! Ordered value-key decode must not panic, and a successful decode round-trips.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pizzlydb_core::{decode_value_key, encode_value_key};

fuzz_target!(|data: &[u8]| {
    if let Ok((value, _)) = decode_value_key(data) {
        let mut encoded = Vec::new();
        encode_value_key(&value, &mut encoded).expect("a decoded key encodes");
        let (again, used) = decode_value_key(&encoded).expect("round trip");
        assert_eq!(again, value);
        assert_eq!(used, encoded.len());
    }
});
