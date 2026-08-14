use proptest::prelude::*;
use proptest::test_runner::RngSeed;

use super::{
    protocol_name, read_client_worker_magic, read_fixture_client_handshake,
    read_fixture_client_post_handshake, read_fixture_server_handshake_info,
    read_fixture_set_options, read_fixture_stderr_frame, read_fixture_terminal_frame,
    read_worker_byte_string, read_worker_integer, read_worker_operation, write_server_worker_magic,
    write_stderr_frame, write_worker_byte_string, write_worker_integer, ActivityField,
    FixtureStderrFrame, ProtocolError, ProtocolSessionLimits, SessionAllocationBudget, StderrFrame,
    WorkerOperation, WorkerReader, WorkerVersion, CLIENT_WORKER_MAGIC, LATEST_WORKER_VERSION,
    MAXIMUM_STRUCTURED_FRAME_MESSAGE_BYTES, MINIMUM_WORKER_VERSION, SERVER_WORKER_MAGIC,
    STDERR_LAST, STDERR_NEXT, STDERR_RESULT, STDERR_START_ACTIVITY, STDERR_STOP_ACTIVITY,
};

#[test]
fn reports_protocol_name() {
    assert_eq!(protocol_name(), "Nix worker protocol");
}

#[test]
fn distinguishes_protocol_failure_classes() {
    let cases = [
        ProtocolError::CleanEof,
        ProtocolError::Truncated,
        ProtocolError::SizeLimit,
        ProtocolError::UnsupportedOperation,
        ProtocolError::VersionMismatch,
        ProtocolError::StoreFailure,
        ProtocolError::InternalFailure,
    ];

    assert_eq!(cases.len(), 7);
}

#[test]
fn session_allocation_budget_releases_metadata_charges() {
    let limits = ProtocolSessionLimits::new(8, std::time::Duration::from_secs(1));
    let budget = SessionAllocationBudget::new(limits);

    let Ok(first) = budget.charge(8) else {
        panic!("first charge fits");
    };
    assert_eq!(budget.retained_bytes(), 8);
    assert!(matches!(budget.charge(1), Err(ProtocolError::SizeLimit)));
    drop(first);
    assert_eq!(budget.retained_bytes(), 0);
    let Ok(second) = budget.charge(8) else {
        panic!("released capacity is reusable");
    };
    drop(second);
}

#[test]
fn session_limit_rejects_handshake_metadata_before_allocation() {
    let input = worker_handshake_with_feature(b"feature");
    let mut input = input.as_slice();
    let mut output = Vec::new();
    let limits = ProtocolSessionLimits::new(23, std::time::Duration::from_secs(1));
    let mut reader = WorkerReader::new(&mut input, limits);
    let error = reader
        .perform_server_handshake(&mut output, &[])
        .expect_err("metadata above the session limit is rejected");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(error.to_string(), "worker metadata exceeds session limit");
    assert_eq!(reader.retained_metadata_bytes(), 0);
}

#[test]
fn negotiated_handshake_metadata_releases_its_session_charge() {
    let input = worker_handshake_with_feature(b"feature");
    let mut input = input.as_slice();
    let mut output = Vec::new();
    let limits = ProtocolSessionLimits::new(64, std::time::Duration::from_secs(1));
    let mut reader = WorkerReader::new(&mut input, limits);
    let Ok(negotiated) = reader.perform_server_handshake(&mut output, &["feature".to_owned()])
    else {
        panic!("handshake metadata fits");
    };

    assert!(reader.retained_metadata_bytes() > 0);
    drop(negotiated);
    assert_eq!(reader.retained_metadata_bytes(), 0);
}

fn worker_handshake_with_feature(feature: &[u8]) -> Vec<u8> {
    let mut input = Vec::new();
    write_worker_integer(&mut input, CLIENT_WORKER_MAGIC);
    write_worker_integer(&mut input, LATEST_WORKER_VERSION.to_wire());
    write_worker_integer(&mut input, 1);
    write_worker_byte_string(&mut input, feature);
    input
}

#[test]
fn worker_reader_uses_one_budget_for_live_metadata() {
    let input = worker_handshake_with_feature(b"feature");
    let mut input = input.as_slice();
    let mut output = Vec::new();
    let limits = ProtocolSessionLimits::new(64, std::time::Duration::from_secs(1));
    let mut reader = WorkerReader::new(&mut input, limits);

    let Ok(negotiated) = reader.perform_server_handshake(&mut output, &["feature".to_owned()])
    else {
        panic!("handshake metadata fits");
    };

    assert!(reader.retained_metadata_bytes() > 0);
    drop(negotiated);
    assert_eq!(reader.retained_metadata_bytes(), 0);
}

#[test]
fn reads_little_endian_worker_integers() {
    let mut zero = &b"\0\0\0\0\0\0\0\0"[..];
    let mut maximum = &b"\xff\xff\xff\xff\xff\xff\xff\xff"[..];
    let mut ordinary = &b"\x08\x07\x06\x05\x04\x03\x02\x01"[..];

    assert_eq!(read_worker_integer(&mut zero), Ok(0));
    assert_eq!(read_worker_integer(&mut maximum), Ok(u64::MAX));
    assert_eq!(
        read_worker_integer(&mut ordinary),
        Ok(0x0102_0304_0506_0708)
    );
}

#[test]
fn rejects_truncated_worker_integers() {
    let mut empty = &b""[..];
    let mut partial = &b"\0\0\0\0\0\0\0"[..];

    assert_eq!(
        read_worker_integer(&mut empty),
        Err(ProtocolError::CleanEof)
    );
    assert_eq!(
        read_worker_integer(&mut partial),
        Err(ProtocolError::Truncated)
    );
}

#[test]
fn reads_bounded_worker_byte_strings() {
    let mut empty = &b"\0\0\0\0\0\0\0\0"[..];
    let mut ordinary = &b"\x03\0\0\0\0\0\0\0abc\0\0\0\0\0"[..];
    let mut padded = &b"\x09\0\0\0\0\0\0\0abcdefghi\0\0\0\0\0\0\0"[..];

    assert_eq!(read_worker_byte_string(&mut empty, 9), Ok(Vec::new()));
    assert_eq!(
        read_worker_byte_string(&mut ordinary, 9),
        Ok(b"abc".to_vec())
    );
    assert_eq!(
        read_worker_byte_string(&mut padded, 9),
        Ok(b"abcdefghi".to_vec())
    );
    assert!(empty.is_empty());
    assert!(ordinary.is_empty());
    assert!(padded.is_empty());
}

#[test]
fn rejects_oversized_worker_byte_strings_before_allocation() {
    let mut input = &b"\x05\0\0\0\0\0\0\0"[..];

    assert_eq!(
        read_worker_byte_string(&mut input, 4),
        Err(ProtocolError::SizeLimit)
    );
    assert!(input.is_empty());
}

#[test]
fn rejects_truncated_worker_byte_string_payload_or_padding() {
    let mut payload = &b"\x03\0\0\0\0\0\0\0ab"[..];
    let mut padding = &b"\x03\0\0\0\0\0\0\0abc\0\0\0\0"[..];

    assert_eq!(
        read_worker_byte_string(&mut payload, 3),
        Err(ProtocolError::Truncated)
    );
    assert_eq!(
        read_worker_byte_string(&mut padding, 3),
        Err(ProtocolError::Truncated)
    );
}

#[test]
fn writes_worker_primitives_matching_golden_bytes() {
    let mut integer = Vec::new();
    let mut empty = Vec::new();
    let mut ordinary = Vec::new();
    let mut padded = Vec::new();

    write_worker_integer(&mut integer, 0x0102_0304_0506_0708);
    write_worker_byte_string(&mut empty, b"");
    write_worker_byte_string(&mut ordinary, b"abc");
    write_worker_byte_string(&mut padded, b"abcdefghi");

    assert_eq!(integer, b"\x08\x07\x06\x05\x04\x03\x02\x01");
    assert_eq!(empty, b"\0\0\0\0\0\0\0\0");
    assert_eq!(ordinary, b"\x03\0\0\0\0\0\0\0abc\0\0\0\0\0");
    assert_eq!(padded, b"\x09\0\0\0\0\0\0\0abcdefghi\0\0\0\0\0\0\0");
}

#[test]
fn accepts_only_the_pinned_client_worker_magic() {
    let accepted_bytes = CLIENT_WORKER_MAGIC.to_le_bytes();
    let rejected_bytes = 0_u64.to_le_bytes();
    let mut accepted = accepted_bytes.as_slice();
    let mut rejected = rejected_bytes.as_slice();

    assert_eq!(read_client_worker_magic(&mut accepted), Ok(()));
    assert_eq!(
        read_client_worker_magic(&mut rejected),
        Err(ProtocolError::VersionMismatch)
    );
}

#[test]
fn writes_the_pinned_server_worker_magic() {
    let mut output = Vec::new();

    write_server_worker_magic(&mut output);

    assert_eq!(SERVER_WORKER_MAGIC, 0x6478_696f);
    assert_eq!(output, b"oixd\0\0\0\0");
}

#[test]
fn parses_typed_fixture_handshake_messages_without_retaining_strings() {
    let mut client = Vec::new();
    write_worker_integer(&mut client, CLIENT_WORKER_MAGIC);
    write_worker_integer(&mut client, LATEST_WORKER_VERSION.to_wire());
    write_worker_integer(&mut client, 1);
    write_worker_byte_string(&mut client, b"secret-feature");
    let mut client = client.as_slice();

    let handshake = read_fixture_client_handshake(&mut client).unwrap();
    assert_eq!(handshake.version, LATEST_WORKER_VERSION);
    assert_eq!(handshake.feature_lengths, vec![14]);
    assert!(client.is_empty());

    let mut server = Vec::new();
    write_worker_integer(&mut server, SERVER_WORKER_MAGIC);
    write_worker_integer(&mut server, LATEST_WORKER_VERSION.to_wire());
    write_worker_integer(&mut server, 1);
    write_worker_byte_string(&mut server, b"secret-server-feature");
    let mut server = server.as_slice();
    let handshake = super::read_fixture_server_handshake(&mut server).unwrap();
    assert_eq!(handshake.version, LATEST_WORKER_VERSION);
    assert_eq!(handshake.feature_lengths, vec![21]);
    assert!(server.is_empty());

    let mut server_info = Vec::new();
    write_worker_byte_string(&mut server_info, b"secret-daemon-version");
    write_worker_integer(&mut server_info, 0);
    write_worker_integer(&mut server_info, STDERR_LAST);
    let mut server_info = server_info.as_slice();

    assert_eq!(
        read_fixture_server_handshake_info(&mut server_info, LATEST_WORKER_VERSION)
            .unwrap()
            .daemon_version_length,
        Some(21)
    );
    read_fixture_terminal_frame(&mut server_info).unwrap();
    assert!(server_info.is_empty());

    let mut post_handshake = Vec::new();
    write_worker_integer(&mut post_handshake, 0);
    write_worker_integer(&mut post_handshake, 0);
    let mut post_handshake = post_handshake.as_slice();
    read_fixture_client_post_handshake(&mut post_handshake, LATEST_WORKER_VERSION).unwrap();
    assert!(post_handshake.is_empty());
}

#[test]
fn parses_typed_fixture_set_options_without_retaining_strings() {
    let mut input = Vec::new();
    write_worker_integer(&mut input, 19);
    for value in 0..12 {
        write_worker_integer(&mut input, value);
    }
    write_worker_integer(&mut input, 1);
    write_worker_byte_string(&mut input, b"secret-name");
    write_worker_byte_string(&mut input, b"secret-value");
    let mut input = input.as_slice();

    assert_eq!(
        read_fixture_set_options(&mut input),
        Ok(super::FixtureSetOptions {
            override_count: 1,
            string_lengths: vec![11, 12],
        })
    );
    assert!(input.is_empty());
}

#[test]
fn parses_pinned_worker_operation_codes_at_typed_boundaries() {
    let mut set_options = &19_u64.to_le_bytes()[..];
    let mut build_paths_with_results = &46_u64.to_le_bytes()[..];

    assert_eq!(
        read_worker_operation(&mut set_options),
        Ok(WorkerOperation::SetOptions)
    );
    assert_eq!(
        read_worker_operation(&mut build_paths_with_results),
        Ok(WorkerOperation::BuildPathsWithResults)
    );
}

#[test]
fn parses_a_fixture_bounded_store_path_request_without_retaining_its_body() {
    let mut input = Vec::new();
    write_worker_integer(&mut input, 11);
    write_worker_byte_string(&mut input, &[b'x'; 153]);
    let mut input = input.as_slice();

    assert_eq!(
        super::read_fixture_store_path_request(&mut input, WorkerOperation::AddTempRoot),
        Ok(super::FixtureStorePathRequest {
            operation: WorkerOperation::AddTempRoot,
            path_length: 153,
        })
    );
    assert!(input.is_empty());
}

#[test]
fn rejects_oversized_truncated_or_malformed_fixture_store_path_requests() {
    let mut oversized = Vec::new();
    write_worker_integer(&mut oversized, 1);
    write_worker_integer(&mut oversized, 154);
    let mut oversized = oversized.as_slice();
    assert_eq!(
        super::read_fixture_store_path_request(&mut oversized, WorkerOperation::IsValidPath),
        Err(ProtocolError::SizeLimit)
    );

    let mut truncated = Vec::new();
    write_worker_integer(&mut truncated, 11);
    write_worker_integer(&mut truncated, 153);
    truncated.extend([0; 152]);
    let mut truncated = truncated.as_slice();
    assert_eq!(
        super::read_fixture_store_path_request(&mut truncated, WorkerOperation::AddTempRoot),
        Err(ProtocolError::Truncated)
    );

    let mut padded = Vec::new();
    write_worker_integer(&mut padded, 1);
    write_worker_byte_string(&mut padded, b"x");
    *padded.last_mut().expect("worker string padding") = 1;
    let mut padded = padded.as_slice();
    assert_eq!(
        super::read_fixture_store_path_request(&mut padded, WorkerOperation::IsValidPath),
        Err(ProtocolError::InternalFailure)
    );
}

#[test]
fn parses_a_fixture_bounded_add_to_store_request_and_upload_without_retaining_bodies() {
    let mut input = Vec::new();
    write_worker_integer(&mut input, 7);
    write_worker_byte_string(&mut input, &[b'n'; 27]);
    write_worker_byte_string(&mut input, &[b'c'; 11]);
    write_worker_integer(&mut input, 0);
    write_worker_integer(&mut input, 0);
    write_worker_integer(&mut input, 502);
    input.extend(vec![b'x'; 502]);
    write_worker_integer(&mut input, 0);
    let mut input = input.as_slice();

    assert_eq!(
        super::read_fixture_add_to_store_request(&mut input),
        Ok(super::FixtureAddToStoreRequest {
            name_length: 27,
            content_address_length: 11,
            reference_count: 0,
            upload_chunk_lengths: vec![502],
            upload_length: 502,
        })
    );
    assert!(input.is_empty());
}

#[test]
fn parses_fixture_bounded_query_missing_and_build_requests_without_retaining_paths() {
    let mut query_missing = Vec::new();
    write_worker_integer(&mut query_missing, 40);
    write_worker_integer(&mut query_missing, 1);
    write_worker_byte_string(&mut query_missing, &[b'd'; 157]);
    let mut query_missing = query_missing.as_slice();
    assert_eq!(
        super::read_fixture_derived_paths_request(
            &mut query_missing,
            WorkerOperation::QueryMissing
        ),
        Ok(super::FixtureDerivedPathsRequest {
            operation: WorkerOperation::QueryMissing,
            path_count: 1,
            path_lengths: vec![157],
            build_mode: None,
        })
    );

    let mut build = Vec::new();
    write_worker_integer(&mut build, 46);
    write_worker_integer(&mut build, 1);
    write_worker_byte_string(&mut build, b"fixture-derived-path");
    write_worker_integer(&mut build, 0);
    let mut build = build.as_slice();
    assert_eq!(
        super::read_fixture_derived_paths_request(
            &mut build,
            WorkerOperation::BuildPathsWithResults
        ),
        Ok(super::FixtureDerivedPathsRequest {
            operation: WorkerOperation::BuildPathsWithResults,
            path_count: 1,
            path_lengths: vec![20],
            build_mode: Some(0),
        })
    );
}

#[test]
fn rejects_fixture_derived_path_count_length_and_build_mode_outside_the_envelope() {
    let mut too_many = Vec::new();
    write_worker_integer(&mut too_many, 40);
    write_worker_integer(&mut too_many, 2);
    let mut too_many = too_many.as_slice();
    assert_eq!(
        super::read_fixture_derived_paths_request(&mut too_many, WorkerOperation::QueryMissing),
        Err(ProtocolError::SizeLimit)
    );

    let mut too_long = Vec::new();
    write_worker_integer(&mut too_long, 46);
    write_worker_integer(&mut too_long, 1);
    write_worker_integer(&mut too_long, 158);
    let mut too_long = too_long.as_slice();
    assert_eq!(
        super::read_fixture_derived_paths_request(
            &mut too_long,
            WorkerOperation::BuildPathsWithResults
        ),
        Err(ProtocolError::SizeLimit)
    );

    let mut bad_mode = Vec::new();
    write_worker_integer(&mut bad_mode, 46);
    write_worker_integer(&mut bad_mode, 1);
    write_worker_byte_string(&mut bad_mode, b"path");
    write_worker_integer(&mut bad_mode, 3);
    let mut bad_mode = bad_mode.as_slice();
    assert_eq!(
        super::read_fixture_derived_paths_request(
            &mut bad_mode,
            WorkerOperation::BuildPathsWithResults
        ),
        Err(ProtocolError::InternalFailure)
    );
}

#[test]
fn parses_fixture_bounded_query_missing_response_without_retaining_paths() {
    let mut input = Vec::new();
    write_worker_integer(&mut input, 1);
    write_worker_byte_string(&mut input, &[b'p'; 153]);
    write_worker_integer(&mut input, 0);
    write_worker_integer(&mut input, 0);
    write_worker_integer(&mut input, 502);
    write_worker_integer(&mut input, 502);
    let mut input = input.as_slice();

    assert_eq!(
        super::read_fixture_query_missing_response(&mut input),
        Ok(super::FixtureQueryMissingResponse {
            will_build_count: 1,
            will_substitute_count: 0,
            unknown_count: 0,
            path_lengths: vec![153],
        })
    );
    assert!(input.is_empty());
}

#[test]
fn rejects_fixture_query_missing_response_outside_the_envelope() {
    let mut too_many = &2_u64.to_le_bytes()[..];
    assert_eq!(
        super::read_fixture_query_missing_response(&mut too_many),
        Err(ProtocolError::SizeLimit)
    );

    let mut truncated = Vec::new();
    write_worker_integer(&mut truncated, 1);
    write_worker_integer(&mut truncated, 153);
    let mut truncated = truncated.as_slice();
    assert_eq!(
        super::read_fixture_query_missing_response(&mut truncated),
        Err(ProtocolError::Truncated)
    );
}

#[test]
fn parses_fixture_bounded_valid_path_info_replies_without_retaining_bodies() {
    let mut valid = Vec::new();
    write_worker_byte_string(&mut valid, &[b'p'; 153]);
    append_fixture_unkeyed_path_info(&mut valid);
    let mut valid = valid.as_slice();
    assert_eq!(
        super::read_fixture_valid_path_info(&mut valid),
        Ok(super::FixtureValidPathInfo {
            path_length: 153,
            deriver_length: 0,
            nar_hash_length: 64,
            reference_count: 0,
            signature_count: 0,
            content_address_length: 64,
        })
    );
    assert!(valid.is_empty());

    let mut query_path_info = Vec::new();
    write_worker_integer(&mut query_path_info, 1);
    append_fixture_unkeyed_path_info(&mut query_path_info);
    let mut query_path_info = query_path_info.as_slice();
    assert_eq!(
        super::read_fixture_query_path_info_response(&mut query_path_info),
        Ok(Some(super::FixtureUnkeyedPathInfo {
            deriver_length: 0,
            nar_hash_length: 64,
            reference_count: 0,
            signature_count: 0,
            content_address_length: 64,
        }))
    );
    assert!(query_path_info.is_empty());
}

#[test]
fn rejects_fixture_path_info_outside_the_envelope() {
    let mut oversized = Vec::new();
    write_worker_integer(&mut oversized, 154);
    let mut oversized = oversized.as_slice();
    assert_eq!(
        super::read_fixture_valid_path_info(&mut oversized),
        Err(ProtocolError::SizeLimit)
    );

    let mut invalid_flag = &2_u64.to_le_bytes()[..];
    assert_eq!(
        super::read_fixture_query_path_info_response(&mut invalid_flag),
        Err(ProtocolError::InternalFailure)
    );
}

#[test]
fn parses_fixture_bounded_build_paths_with_results_response_without_retaining_bodies() {
    let mut input = Vec::new();
    write_worker_integer(&mut input, 1);
    write_worker_byte_string(&mut input, &[b'p'; 157]);
    write_worker_integer(&mut input, 0);
    write_worker_byte_string(&mut input, b"");
    write_worker_integer(&mut input, 1);
    write_worker_integer(&mut input, 0);
    write_worker_integer(&mut input, 0);
    write_worker_integer(&mut input, 0);
    write_worker_integer(&mut input, 0);
    write_worker_integer(&mut input, 0);
    write_worker_integer(&mut input, 1);
    write_worker_byte_string(&mut input, &[b'o'; 75]);
    write_worker_byte_string(&mut input, &[b'r'; 196]);
    let mut input = input.as_slice();

    assert_eq!(
        super::read_fixture_build_paths_with_results_response(&mut input),
        Ok(super::FixtureBuildPathsWithResultsResponse {
            result_count: 1,
            path_length: 157,
            status: 0,
            error_length: 0,
            output_count: 1,
            output_id_length: 75,
            output_realisation_length: 196,
        })
    );
    assert!(input.is_empty());
}

#[test]
fn rejects_fixture_build_results_outside_the_envelope() {
    let mut too_many = &2_u64.to_le_bytes()[..];
    assert_eq!(
        super::read_fixture_build_paths_with_results_response(&mut too_many),
        Err(ProtocolError::SizeLimit)
    );

    let mut bad_status = Vec::new();
    write_worker_integer(&mut bad_status, 1);
    write_worker_byte_string(&mut bad_status, b"p");
    write_worker_integer(&mut bad_status, 15);
    let mut bad_status = bad_status.as_slice();
    assert_eq!(
        super::read_fixture_build_paths_with_results_response(&mut bad_status),
        Err(ProtocolError::InternalFailure)
    );
}

#[test]
fn parses_fixture_bounded_stderr_activity_frames_without_retaining_bodies() {
    let mut start = Vec::new();
    write_worker_integer(&mut start, super::STDERR_START_ACTIVITY);
    write_worker_integer(&mut start, 1);
    write_worker_integer(&mut start, 2);
    write_worker_integer(&mut start, 3);
    write_worker_byte_string(&mut start, &[b'm'; 164]);
    write_worker_integer(&mut start, 2);
    write_worker_integer(&mut start, 0);
    write_worker_integer(&mut start, 42);
    write_worker_integer(&mut start, 1);
    write_worker_byte_string(&mut start, &[b'f'; 153]);
    write_worker_integer(&mut start, 0);
    let mut start = start.as_slice();
    assert_eq!(
        super::read_fixture_stderr_frame(&mut start),
        Ok(super::FixtureStderrFrame::StartActivity {
            message_length: 164,
            field_count: 2,
            field_string_lengths: vec![153],
        })
    );
    assert!(start.is_empty());

    let mut next = Vec::new();
    write_worker_integer(&mut next, super::STDERR_NEXT);
    write_worker_byte_string(&mut next, &[b'n'; 145]);
    let mut next = next.as_slice();
    assert_eq!(
        super::read_fixture_stderr_frame(&mut next),
        Ok(super::FixtureStderrFrame::Next {
            message_length: 145
        })
    );
}

#[test]
fn rejects_fixture_stderr_activity_frames_outside_the_envelope() {
    let mut too_many_fields = Vec::new();
    write_worker_integer(&mut too_many_fields, super::STDERR_RESULT);
    write_worker_integer(&mut too_many_fields, 1);
    write_worker_integer(&mut too_many_fields, 1);
    write_worker_integer(&mut too_many_fields, 5);
    let mut too_many_fields = too_many_fields.as_slice();
    assert_eq!(
        super::read_fixture_stderr_frame(&mut too_many_fields),
        Err(ProtocolError::SizeLimit)
    );

    let mut unknown = &0_u64.to_le_bytes()[..];
    assert_eq!(
        super::read_fixture_stderr_frame(&mut unknown),
        Err(ProtocolError::UnsupportedOperation)
    );
}

#[test]
fn rejects_fixture_add_to_store_overflow_truncation_and_extra_upload_chunks() {
    let mut oversized = Vec::new();
    write_worker_integer(&mut oversized, 7);
    write_worker_integer(&mut oversized, 28);
    let mut oversized = oversized.as_slice();
    assert_eq!(
        super::read_fixture_add_to_store_request(&mut oversized),
        Err(ProtocolError::SizeLimit)
    );

    let mut truncated = Vec::new();
    write_worker_integer(&mut truncated, 7);
    write_worker_byte_string(&mut truncated, b"n");
    write_worker_byte_string(&mut truncated, b"c");
    write_worker_integer(&mut truncated, 0);
    write_worker_integer(&mut truncated, 0);
    write_worker_integer(&mut truncated, 1);
    let mut truncated = truncated.as_slice();
    assert_eq!(
        super::read_fixture_add_to_store_request(&mut truncated),
        Err(ProtocolError::Truncated)
    );

    let mut extra_chunk = Vec::new();
    write_worker_integer(&mut extra_chunk, 7);
    write_worker_byte_string(&mut extra_chunk, b"n");
    write_worker_byte_string(&mut extra_chunk, b"c");
    write_worker_integer(&mut extra_chunk, 0);
    write_worker_integer(&mut extra_chunk, 0);
    write_worker_integer(&mut extra_chunk, 1);
    extra_chunk.push(b'x');
    write_worker_integer(&mut extra_chunk, 1);
    extra_chunk.push(b'y');
    let mut extra_chunk = extra_chunk.as_slice();
    assert_eq!(
        super::read_fixture_add_to_store_request(&mut extra_chunk),
        Err(ProtocolError::SizeLimit)
    );

    let mut wrong_operation = &39_u64.to_le_bytes()[..];
    assert_eq!(
        super::read_fixture_add_to_store_request(&mut wrong_operation),
        Err(ProtocolError::UnsupportedOperation)
    );
}

fn append_fixture_unkeyed_path_info(output: &mut Vec<u8>) {
    write_worker_byte_string(output, b"");
    write_worker_byte_string(output, &[b'h'; 64]);
    write_worker_integer(output, 0);
    write_worker_integer(output, 0);
    write_worker_integer(output, 502);
    write_worker_integer(output, 1);
    write_worker_integer(output, 0);
    write_worker_byte_string(output, &[b'c'; 64]);
}

#[test]
fn negotiates_supported_worker_versions_and_features() {
    let client_features = vec!["one".to_owned(), "two".to_owned()];
    let server_features = vec!["two".to_owned(), "three".to_owned()];

    let below_minimum = super::negotiate_worker_version(WorkerVersion::new(1, 34), &[], &[]);
    let minimum = super::negotiate_worker_version(MINIMUM_WORKER_VERSION, &[], &[]).unwrap();
    let supported = super::negotiate_worker_version(WorkerVersion::new(1, 37), &[], &[]).unwrap();
    let maximum =
        super::negotiate_worker_version(LATEST_WORKER_VERSION, &client_features, &server_features)
            .unwrap();
    let newer_same_major = super::negotiate_worker_version(
        WorkerVersion::new(1, 39),
        &client_features,
        &server_features,
    )
    .unwrap();
    let newer_major = super::negotiate_worker_version(
        WorkerVersion::new(2, 18),
        &client_features,
        &server_features,
    )
    .unwrap();

    assert_eq!(below_minimum, Err(ProtocolError::VersionMismatch));
    assert_eq!(minimum.version, MINIMUM_WORKER_VERSION);
    assert_eq!(supported.version, WorkerVersion::new(1, 37));
    assert_eq!(maximum.version, LATEST_WORKER_VERSION);
    assert_eq!(maximum.features, vec!["two"]);
    assert_eq!(newer_same_major.version, LATEST_WORKER_VERSION);
    assert_eq!(newer_major.version, LATEST_WORKER_VERSION);
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, rng_seed: RngSeed::Fixed(0x5445_4c43_4841_5230), .. ProptestConfig::default() })]

    #[test]
    fn primitive_parsers_never_panic_and_respect_limits(input in proptest::collection::vec(any::<u8>(), 0..1024), maximum_length in 0usize..128) {
        let mut integer_input = input.as_slice();
        let mut byte_string_input = input.as_slice();

        let _ = read_worker_integer(&mut integer_input);
        let result = read_worker_byte_string(&mut byte_string_input, maximum_length);

        if let Ok(value) = result {
            prop_assert!(value.len() <= maximum_length);
        }
    }
}

#[test]
fn writes_stderr_next_frame_with_message() {
    let mut output = Vec::new();
    let _ = write_stderr_frame(
        &mut output,
        StderrFrame::Next {
            message: b"building derivation\0".to_vec(),
        },
    );

    // STDERR_NEXT tag
    assert_eq!(&output[0..8], &STDERR_NEXT.to_le_bytes());
    // message length (19 bytes + 7 padding = 26)
    let msg_len = u64::from_le_bytes(output[8..16].try_into().unwrap());
    assert_eq!(msg_len, 20);
    // message content
    assert_eq!(&output[16..36], b"building derivation\0");
}

#[test]
fn writes_stderr_last_frame() {
    let mut output = Vec::new();
    let _ = write_stderr_frame(&mut output, StderrFrame::Last);

    assert_eq!(&output[..8], &STDERR_LAST.to_le_bytes());
    assert_eq!(output.len(), 8);
}

#[test]
fn writes_stderr_stop_activity_frame() {
    let mut output = Vec::new();
    let _ = write_stderr_frame(&mut output, StderrFrame::StopActivity { activity_id: 1 });

    let mut expected = Vec::new();
    write_worker_integer(&mut expected, STDERR_STOP_ACTIVITY);
    write_worker_integer(&mut expected, 1);
    assert_eq!(output, expected);
}

#[test]
fn writes_unsigned_activity_integer_fields() {
    let mut output = Vec::new();
    let _ = write_stderr_frame(
        &mut output,
        StderrFrame::Result {
            activity_id: 1,
            result_type: 2,
            fields: vec![ActivityField::Integer(u64::MAX)],
        },
    );

    let mut expected = Vec::new();
    write_worker_integer(&mut expected, STDERR_RESULT);
    write_worker_integer(&mut expected, 1);
    write_worker_integer(&mut expected, 2);
    write_worker_integer(&mut expected, 1);
    write_worker_integer(&mut expected, 0);
    write_worker_integer(&mut expected, u64::MAX);
    assert_eq!(output, expected);
}

#[test]
fn writes_stderr_start_activity_frame_with_fields() {
    let mut output = Vec::new();
    let fields = vec![
        ActivityField::String(b"copying\0".to_vec()),
        ActivityField::Integer(42),
    ];
    let _ = write_stderr_frame(
        &mut output,
        StderrFrame::StartActivity {
            activity_id: 1,
            verbosity: 2,
            activity_type: 3,
            message: b"starting build\0".to_vec(),
            fields,
            parent_activity_id: 0,
        },
    );

    let mut expected = Vec::new();
    write_worker_integer(&mut expected, STDERR_START_ACTIVITY);
    write_worker_integer(&mut expected, 1);
    write_worker_integer(&mut expected, 2);
    write_worker_integer(&mut expected, 3);
    write_worker_byte_string(&mut expected, b"starting build\0");
    write_worker_integer(&mut expected, 2);
    write_worker_integer(&mut expected, 1);
    write_worker_byte_string(&mut expected, b"copying\0");
    write_worker_integer(&mut expected, 0);
    write_worker_integer(&mut expected, 42);
    write_worker_integer(&mut expected, 0);
    assert_eq!(output, expected);
}

#[test]
fn writes_stderr_result_frame_with_fields() {
    let mut output = Vec::new();
    let fields = vec![
        ActivityField::String(b"output\0".to_vec()),
        ActivityField::String(b"result\0".to_vec()),
    ];
    let _ = write_stderr_frame(
        &mut output,
        StderrFrame::Result {
            activity_id: 1,
            result_type: 2,
            fields,
        },
    );

    let mut expected = Vec::new();
    write_worker_integer(&mut expected, STDERR_RESULT);
    write_worker_integer(&mut expected, 1);
    write_worker_integer(&mut expected, 2);
    write_worker_integer(&mut expected, 2);
    write_worker_integer(&mut expected, 1);
    write_worker_byte_string(&mut expected, b"output\0");
    write_worker_integer(&mut expected, 1);
    write_worker_byte_string(&mut expected, b"result\0");
    assert_eq!(output, expected);
}

#[test]
fn round_trip_stderr_next_frame() {
    let mut encoded = Vec::new();
    let _ = write_stderr_frame(
        &mut encoded,
        StderrFrame::Next {
            message: b"round-trip test\0".to_vec(),
        },
    );

    let mut decoded = encoded.as_slice();
    let frame = read_fixture_stderr_frame(&mut decoded).unwrap();

    match frame {
        FixtureStderrFrame::Next { message_length } => {
            assert_eq!(message_length, 16);
        }
        other => panic!("expected Next, got {:?}", other),
    }
}

#[test]
fn rejects_oversized_worker_error_before_writing() {
    let mut output = Vec::new();
    let error = super::write_worker_error(
        &mut output,
        &"e".repeat(MAXIMUM_STRUCTURED_FRAME_MESSAGE_BYTES + 1),
    )
    .expect_err("oversized worker error is rejected");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert!(output.is_empty());
}

#[test]
fn rejects_oversized_structured_stderr_frames() {
    let mut output = Vec::new();
    let message = vec![b'm'; 16 * 1024 + 1];

    let error = write_stderr_frame(&mut output, StderrFrame::Next { message })
        .expect_err("oversized structured frame is rejected");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert!(output.is_empty());
}

#[test]
fn rejects_oversized_structured_activity_metadata() {
    let mut output = Vec::new();
    let fields = vec![ActivityField::String(vec![b'n'; 4 * 1024 + 1])];

    let error = write_stderr_frame(
        &mut output,
        StderrFrame::Result {
            activity_id: 1,
            result_type: 2,
            fields,
        },
    )
    .expect_err("oversized activity metadata is rejected");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert!(output.is_empty());
}

#[test]
fn round_trip_stderr_last_frame() {
    let mut encoded = Vec::new();
    let _ = write_stderr_frame(&mut encoded, StderrFrame::Last);

    let mut decoded = encoded.as_slice();
    let frame = read_fixture_stderr_frame(&mut decoded).unwrap();

    assert!(matches!(frame, FixtureStderrFrame::Last));
}

#[test]
fn round_trip_stderr_stop_activity_frame() {
    let mut encoded = Vec::new();
    let _ = write_stderr_frame(&mut encoded, StderrFrame::StopActivity { activity_id: 1 });

    let mut decoded = encoded.as_slice();
    let frame = read_fixture_stderr_frame(&mut decoded).unwrap();

    assert!(matches!(frame, FixtureStderrFrame::StopActivity));
}

#[test]
fn round_trip_stderr_start_activity_frame() {
    let mut encoded = Vec::new();
    let fields = vec![
        ActivityField::String(b"copying\0".to_vec()),
        ActivityField::Integer(42),
    ];
    let _ = write_stderr_frame(
        &mut encoded,
        StderrFrame::StartActivity {
            activity_id: 1,
            verbosity: 2,
            activity_type: 3,
            message: b"build start\0".to_vec(),
            fields,
            parent_activity_id: 0,
        },
    );

    let mut decoded = encoded.as_slice();
    let frame = read_fixture_stderr_frame(&mut decoded).unwrap();

    match frame {
        FixtureStderrFrame::StartActivity { field_count, .. } => {
            assert_eq!(field_count, 2);
        }
        other => panic!("expected StartActivity, got {:?}", other),
    }
}

#[test]
fn round_trip_stderr_result_frame() {
    let mut encoded = Vec::new();
    let fields = vec![
        ActivityField::String(b"output\0".to_vec()),
        ActivityField::String(b"success\0".to_vec()),
    ];
    let _ = write_stderr_frame(
        &mut encoded,
        StderrFrame::Result {
            activity_id: 1,
            result_type: 2,
            fields,
        },
    );

    let mut decoded = encoded.as_slice();
    let frame = read_fixture_stderr_frame(&mut decoded).unwrap();

    match frame {
        FixtureStderrFrame::Result { field_count, .. } => {
            assert_eq!(field_count, 2);
        }
        other => panic!("expected Result, got {:?}", other),
    }
}
