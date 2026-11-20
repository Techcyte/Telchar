//! Fuzzes primitive worker-protocol framing to ensure hostile bytes cannot panic or exceed limits.

#![no_main]

use libfuzzer_sys::fuzz_target;
use nix_worker_protocol::{
    LATEST_WORKER_VERSION, ProtocolSessionLimits, WorkerReader, read_worker_byte_string,
    read_worker_integer,
};

fuzz_target!(|data: &[u8]| {
    let mut integer_input = data;
    let mut byte_string_input = data;

    let _ = read_worker_integer(&mut integer_input);
    let _ = read_worker_byte_string(&mut byte_string_input, 1024 * 1024);
    for missing in [false, true] {
        let mut reader = WorkerReader::new(
            data,
            ProtocolSessionLimits::new(64 * 1024, std::time::Duration::from_secs(1)),
        );
        if missing {
            let _ = reader.complete_query_missing();
        } else {
            let _ = reader.complete_query_valid_paths(LATEST_WORKER_VERSION);
        }
        assert_eq!(reader.retained_metadata_bytes(), 0);
    }
});
