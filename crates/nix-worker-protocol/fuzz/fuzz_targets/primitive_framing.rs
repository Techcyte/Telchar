#![no_main]

use libfuzzer_sys::fuzz_target;
use nix_worker_protocol::{read_worker_byte_string, read_worker_integer};

fuzz_target!(|data: &[u8]| {
    let mut integer_input = data;
    let mut byte_string_input = data;

    let _ = read_worker_integer(&mut integer_input);
    let _ = read_worker_byte_string(&mut byte_string_input, 1024 * 1024);
});
