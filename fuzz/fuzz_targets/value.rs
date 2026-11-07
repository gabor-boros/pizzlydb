//! `Value` decode must not panic, and a successful decode round-trips.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pizzlydb_core::{decode_value, encode_value};

fuzz_target!(|data: &[u8]| {
    if let Ok(value) = decode_value(data) {
        let encoded = encode_value(&value).expect("a decoded value encodes");
        assert_eq!(decode_value(&encoded).expect("round trip"), value);
    }
});
