//! Tests query valid paths contract contracts and failure boundaries, including decodes bounded query valid paths request.

use std::io;
use std::time::Duration;

use nix_worker_protocol::{
    LATEST_WORKER_VERSION, MAXIMUM_QUERY_VALID_PATHS, MAXIMUM_WORKER_STORE_PATH_BYTES,
    ProtocolSessionLimits, WorkerOperation, WorkerReader, WorkerVersion, write_worker_byte_string,
    write_worker_integer,
};

const VALID_PATH: &[u8] = b"/nix/store/0123456789abcdfghijklmnpqrsvwxyz-valid";
const INVALID_PATH: &[u8] = b"/nix/store/11111111111111111111111111111111-missing";

proptest::proptest! {
    #[test]
    fn path_query_membership_matches_distinct_input(values in proptest::collection::vec(0_u16..1000, 0..128)) {
        let paths = values.iter().map(|value| {
            format!("/nix/store/0123456789abcdfghijklmnpqrsvwxyz-property-{value}").into_bytes()
        }).collect::<Vec<_>>();
        let distinct = values.iter().collect::<std::collections::BTreeSet<_>>().len() == values.len();
        for missing in [false, true] {
            let mut wire = Vec::new();
            write_worker_integer(&mut wire, paths.len() as u64);
            for path in &paths { write_worker_byte_string(&mut wire, path); }
            if !missing { write_worker_integer(&mut wire, 0); }
            let mut reader = WorkerReader::new(wire.as_slice(), ProtocolSessionLimits::DEFAULT);
            let result = if missing {
                reader.complete_query_missing().map(|request| request.targets().to_vec())
            } else {
                reader.complete_query_valid_paths(LATEST_WORKER_VERSION).map(|request| request.paths().to_vec())
            };
            proptest::prop_assert_eq!(result.is_ok(), distinct);
            if let Ok(decoded) = result { proptest::prop_assert_eq!(decoded, paths.clone()); }
            proptest::prop_assert_eq!(reader.retained_metadata_bytes(), 0);
        }
    }
}

#[test]
fn maximum_path_queries_preserve_order_and_reject_late_duplicates() {
    for missing in [false, true] {
        for duplicate in [false, true] {
            let mut wire = Vec::new();
            write_worker_integer(&mut wire, MAXIMUM_QUERY_VALID_PATHS as u64);
            for index in 0..MAXIMUM_QUERY_VALID_PATHS {
                let index = if duplicate && index + 1 == MAXIMUM_QUERY_VALID_PATHS {
                    0
                } else {
                    index
                };
                write_worker_byte_string(
                    &mut wire,
                    format!("/nix/store/0123456789abcdfghijklmnpqrsvwxyz-query-{index:05}")
                        .as_bytes(),
                );
            }
            if !missing {
                write_worker_integer(&mut wire, 0);
            }
            let mut reader = WorkerReader::new(
                wire.as_slice(),
                ProtocolSessionLimits::new(32 * 1024 * 1024, Duration::from_secs(60)),
            );
            let started = std::time::Instant::now();
            let result = if missing {
                reader
                    .complete_query_missing()
                    .map(|request| request.targets().to_vec())
            } else {
                reader
                    .complete_query_valid_paths(LATEST_WORKER_VERSION)
                    .map(|request| request.paths().to_vec())
            };
            eprintln!(
                "maximum query missing={missing} duplicate={duplicate}: {:?}",
                started.elapsed()
            );
            if duplicate {
                assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
            } else {
                let paths = result.unwrap();
                assert_eq!(paths.len(), MAXIMUM_QUERY_VALID_PATHS);
                assert!(paths.first().unwrap().ends_with(b"query-00000"));
                assert!(paths.last().unwrap().ends_with(b"query-65535"));
            }
        }
    }
}

#[test]
fn path_queries_reject_early_duplicates_and_insufficient_membership_budget() {
    for missing in [false, true] {
        let mut duplicate = Vec::new();
        write_worker_integer(&mut duplicate, 2);
        write_worker_byte_string(&mut duplicate, VALID_PATH);
        write_worker_byte_string(&mut duplicate, VALID_PATH);
        if !missing {
            write_worker_integer(&mut duplicate, 0);
        }
        let mut reader = WorkerReader::new(duplicate.as_slice(), ProtocolSessionLimits::DEFAULT);
        let result = if missing {
            reader.complete_query_missing().map(|_| ())
        } else {
            reader
                .complete_query_valid_paths(LATEST_WORKER_VERSION)
                .map(|_| ())
        };
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);

        let mut wire = Vec::new();
        write_worker_integer(&mut wire, 1);
        write_worker_byte_string(&mut wire, VALID_PATH);
        if !missing {
            write_worker_integer(&mut wire, 0);
        }
        let budget = std::mem::size_of::<Vec<u8>>() + VALID_PATH.len();
        let mut reader = WorkerReader::new(
            wire.as_slice(),
            ProtocolSessionLimits::new(budget, Duration::from_secs(1)),
        );
        let result = if missing {
            reader.complete_query_missing().map(|_| ())
        } else {
            reader
                .complete_query_valid_paths(LATEST_WORKER_VERSION)
                .map(|_| ())
        };
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert_eq!(reader.retained_metadata_bytes(), 0);
    }
}

#[test]
fn decodes_bounded_query_valid_paths_request() {
    let mut wire = Vec::new();
    write_worker_integer(&mut wire, 31);
    write_worker_integer(&mut wire, 2);
    write_worker_byte_string(&mut wire, VALID_PATH);
    write_worker_byte_string(&mut wire, INVALID_PATH);
    write_worker_integer(&mut wire, 0);
    let mut reader = WorkerReader::new(
        wire.as_slice(),
        ProtocolSessionLimits::new(16 * 1024 * 1024, Duration::from_secs(1)),
    );

    assert_eq!(
        reader.read_operation().expect("operation decodes"),
        WorkerOperation::QueryValidPaths
    );
    let request = reader
        .complete_query_valid_paths(LATEST_WORKER_VERSION)
        .expect("bounded request decodes");

    assert_eq!(request.paths(), [VALID_PATH, INVALID_PATH]);
    assert!(!request.substitute());
    assert!(reader.retained_metadata_bytes() > 0);
}

#[test]
fn decodes_realistic_package_closure_query() {
    let count = 2_530;
    let mut wire = Vec::new();
    write_worker_integer(&mut wire, 31);
    write_worker_integer(&mut wire, count);
    for index in 0..count {
        write_worker_byte_string(
            &mut wire,
            format!("/nix/store/0123456789abcdfghijklmnpqrsvwxyz-closure-{index}").as_bytes(),
        );
    }
    write_worker_integer(&mut wire, 1);
    let mut reader = WorkerReader::new(
        wire.as_slice(),
        ProtocolSessionLimits::new(16 * 1024 * 1024, Duration::from_secs(1)),
    );

    assert_eq!(
        reader.read_operation().expect("operation decodes"),
        WorkerOperation::QueryValidPaths
    );
    let request = reader
        .complete_query_valid_paths(LATEST_WORKER_VERSION)
        .expect("real package closure query decodes");

    assert_eq!(request.paths().len(), count as usize);
    assert!(request.substitute());
    assert!(reader.retained_metadata_bytes() < 16 * 1024 * 1024);
}

#[test]
fn rejects_query_valid_paths_count_before_path_allocation() {
    let mut wire = Vec::new();
    write_worker_integer(&mut wire, 31);
    write_worker_integer(&mut wire, MAXIMUM_QUERY_VALID_PATHS as u64 + 1);
    let mut reader = WorkerReader::new(
        wire.as_slice(),
        ProtocolSessionLimits::new(16 * 1024 * 1024, Duration::from_secs(1)),
    );

    assert_eq!(
        reader.read_operation().expect("operation decodes"),
        WorkerOperation::QueryValidPaths
    );
    let error = reader
        .complete_query_valid_paths(LATEST_WORKER_VERSION)
        .expect_err("oversized count must fail");

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(reader.retained_metadata_bytes(), 0);
}

#[test]
fn rejects_oversized_query_valid_paths_path_before_allocation() {
    let mut wire = Vec::new();
    write_worker_integer(&mut wire, 31);
    write_worker_integer(&mut wire, 1);
    write_worker_integer(&mut wire, MAXIMUM_WORKER_STORE_PATH_BYTES as u64 + 1);
    let mut reader = WorkerReader::new(
        wire.as_slice(),
        ProtocolSessionLimits::new(16 * 1024 * 1024, Duration::from_secs(1)),
    );
    assert_eq!(
        reader.read_operation().expect("operation decodes"),
        WorkerOperation::QueryValidPaths
    );

    let error = reader
        .complete_query_valid_paths(LATEST_WORKER_VERSION)
        .expect_err("oversized path must fail");

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn protocol_before_1_27_has_no_substitute_word() {
    let mut wire = Vec::new();
    write_worker_integer(&mut wire, 31);
    write_worker_integer(&mut wire, 1);
    write_worker_byte_string(&mut wire, VALID_PATH);
    let mut reader = WorkerReader::new(
        wire.as_slice(),
        ProtocolSessionLimits::new(16 * 1024 * 1024, Duration::from_secs(1)),
    );
    assert_eq!(
        reader.read_operation().expect("operation decodes"),
        WorkerOperation::QueryValidPaths
    );

    let request = reader
        .complete_query_valid_paths(WorkerVersion::new(1, 26))
        .expect("pre-1.27 request decodes without substitute word");

    assert_eq!(request.paths(), [VALID_PATH]);
    assert!(!request.substitute());
}

#[test]
fn decodes_query_valid_paths_substitution_request() {
    let mut wire = Vec::new();
    write_worker_integer(&mut wire, 31);
    write_worker_integer(&mut wire, 1);
    write_worker_byte_string(&mut wire, VALID_PATH);
    write_worker_integer(&mut wire, 1);
    let mut reader = WorkerReader::new(
        wire.as_slice(),
        ProtocolSessionLimits::new(16 * 1024 * 1024, Duration::from_secs(1)),
    );

    assert_eq!(
        reader.read_operation().expect("operation decodes"),
        WorkerOperation::QueryValidPaths
    );
    let request = reader
        .complete_query_valid_paths(LATEST_WORKER_VERSION)
        .expect("stock Nix substitution request decodes");

    assert_eq!(request.paths(), [VALID_PATH]);
    assert!(request.substitute());
}

#[test]
fn encodes_query_valid_paths_response_in_store_path_order() {
    let mut response = Vec::new();
    nix_worker_protocol::write_query_valid_paths_response(
        &mut response,
        [INVALID_PATH, VALID_PATH],
    )
    .expect("valid response encodes");
    let mut expected = Vec::new();
    write_worker_integer(&mut expected, 2);
    write_worker_byte_string(&mut expected, VALID_PATH);
    write_worker_byte_string(&mut expected, INVALID_PATH);

    assert_eq!(response, expected);
}
