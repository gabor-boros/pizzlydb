//! Meta-target. The smoke job expects this process to exit non-zero.
//!
//! It is not a decoder. Do not add it to the default target list.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    assert!(data.first().copied() != Some(0xA5), "injected decoder panic");
});
