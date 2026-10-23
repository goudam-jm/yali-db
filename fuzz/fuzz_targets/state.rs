#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| yalidb_core::fuzz_support::state(data));
