//! Timestamp cell decode must not panic, and a successful decode round-trips.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pizzlydb_core::{decode_value, decode_value_key, encode_value, encode_value_key, index_cmp};

fuzz_target!(|data: &[u8]| {
    let mut bytes = vec![8];
    bytes.extend_from_slice(data);
    if let Ok(value) = decode_value(&bytes) {
        let encoded = encode_value(&value).expect("encode");
        assert_eq!(decode_value(&encoded).expect("round trip"), value);
        let mut key = Vec::new();
        encode_value_key(&value, &mut key).expect("key");
        let (decoded, used) = decode_value_key(&key).expect("key decode");
        assert_eq!(used, key.len());
        assert_eq!(index_cmp(&decoded, &value), std::cmp::Ordering::Equal);
    }
});
