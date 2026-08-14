use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixtureStorePathRequest {
    pub operation: WorkerOperation,
    pub path_length: u64,
}

pub fn read_fixture_store_path_request(
    input: &mut &[u8],
    expected_operation: WorkerOperation,
) -> Result<FixtureStorePathRequest, ProtocolError> {
    match expected_operation {
        WorkerOperation::AddTempRoot | WorkerOperation::IsValidPath => {}
        _ => return Err(ProtocolError::UnsupportedOperation),
    }
    let operation = read_worker_operation(input)?;
    if operation != expected_operation {
        return Err(ProtocolError::UnsupportedOperation);
    }
    Ok(FixtureStorePathRequest {
        operation,
        path_length: read_fixture_string_length(input, 153)?,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixtureAddToStoreRequest {
    pub name_length: u64,
    pub content_address_length: u64,
    pub reference_count: u64,
    pub upload_chunk_lengths: Vec<u64>,
    pub upload_length: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixtureDerivedPathsRequest {
    pub operation: WorkerOperation,
    pub path_count: u64,
    pub path_lengths: Vec<u64>,
    pub build_mode: Option<u64>,
}

pub fn read_fixture_derived_paths_request(
    input: &mut &[u8],
    expected_operation: WorkerOperation,
) -> Result<FixtureDerivedPathsRequest, ProtocolError> {
    match expected_operation {
        WorkerOperation::QueryMissing | WorkerOperation::BuildPathsWithResults => {}
        _ => return Err(ProtocolError::UnsupportedOperation),
    }
    let operation = read_worker_operation(input)?;
    if operation != expected_operation {
        return Err(ProtocolError::UnsupportedOperation);
    }
    let path_count = read_fixture_count(input, 1)?;
    let path_lengths = (0..path_count)
        .map(|_| read_fixture_string_length(input, 157))
        .collect::<Result<Vec<_>, _>>()?;
    let build_mode = if operation == WorkerOperation::BuildPathsWithResults {
        let mode = read_worker_integer(input)?;
        if mode > 2 {
            return Err(ProtocolError::InternalFailure);
        }
        Some(mode)
    } else {
        None
    };
    Ok(FixtureDerivedPathsRequest {
        operation,
        path_count,
        path_lengths,
        build_mode,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixtureQueryMissingResponse {
    pub will_build_count: u64,
    pub will_substitute_count: u64,
    pub unknown_count: u64,
    pub path_lengths: Vec<u64>,
}

pub fn read_fixture_query_missing_response(
    input: &mut &[u8],
) -> Result<FixtureQueryMissingResponse, ProtocolError> {
    let will_build_count = read_fixture_count(input, 1)?;
    let mut path_lengths = Vec::with_capacity(will_build_count as usize);
    for _ in 0..will_build_count {
        path_lengths.push(read_fixture_string_length(input, 153)?);
    }
    let will_substitute_count = read_fixture_count(input, 0)?;
    let unknown_count = read_fixture_count(input, 0)?;
    read_worker_integer(input)?;
    read_worker_integer(input)?;
    Ok(FixtureQueryMissingResponse {
        will_build_count,
        will_substitute_count,
        unknown_count,
        path_lengths,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixtureUnkeyedPathInfo {
    pub deriver_length: u64,
    pub nar_hash_length: u64,
    pub reference_count: u64,
    pub signature_count: u64,
    pub content_address_length: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixtureValidPathInfo {
    pub path_length: u64,
    pub deriver_length: u64,
    pub nar_hash_length: u64,
    pub reference_count: u64,
    pub signature_count: u64,
    pub content_address_length: u64,
}

pub fn read_fixture_valid_path_info(
    input: &mut &[u8],
) -> Result<FixtureValidPathInfo, ProtocolError> {
    let path_length = read_fixture_string_length(input, 153)?;
    let info = read_fixture_unkeyed_path_info(input)?;
    Ok(FixtureValidPathInfo {
        path_length,
        deriver_length: info.deriver_length,
        nar_hash_length: info.nar_hash_length,
        reference_count: info.reference_count,
        signature_count: info.signature_count,
        content_address_length: info.content_address_length,
    })
}

pub fn read_fixture_query_path_info_response(
    input: &mut &[u8],
) -> Result<Option<FixtureUnkeyedPathInfo>, ProtocolError> {
    if read_fixture_boolean(input)? {
        Ok(Some(read_fixture_unkeyed_path_info(input)?))
    } else {
        Ok(None)
    }
}

fn read_fixture_unkeyed_path_info(
    input: &mut &[u8],
) -> Result<FixtureUnkeyedPathInfo, ProtocolError> {
    let deriver_length = read_fixture_string_length(input, 0)?;
    let nar_hash_length = read_fixture_string_length(input, 64)?;
    let reference_count = read_fixture_count(input, 0)?;
    read_worker_integer(input)?;
    read_worker_integer(input)?;
    read_fixture_boolean(input)?;
    let signature_count = read_fixture_count(input, 0)?;
    let content_address_length = read_fixture_string_length(input, 64)?;
    Ok(FixtureUnkeyedPathInfo {
        deriver_length,
        nar_hash_length,
        reference_count,
        signature_count,
        content_address_length,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixtureBuildPathsWithResultsResponse {
    pub result_count: u64,
    pub path_length: u64,
    pub status: u64,
    pub error_length: u64,
    pub output_count: u64,
    pub output_id_length: u64,
    pub output_realisation_length: u64,
}

pub fn read_fixture_build_paths_with_results_response(
    input: &mut &[u8],
) -> Result<FixtureBuildPathsWithResultsResponse, ProtocolError> {
    let result_count = read_fixture_count(input, 1)?;
    if result_count == 0 {
        return Err(ProtocolError::InternalFailure);
    }
    let path_length = read_fixture_string_length(input, 157)?;
    let status = read_worker_integer(input)?;
    if status > 14 {
        return Err(ProtocolError::InternalFailure);
    }
    let error_length = read_fixture_string_length(input, 0)?;
    read_worker_integer(input)?;
    read_fixture_boolean(input)?;
    read_worker_integer(input)?;
    read_worker_integer(input)?;
    read_fixture_optional_duration(input)?;
    read_fixture_optional_duration(input)?;
    let output_count = read_fixture_count(input, 1)?;
    if output_count == 0 {
        return Err(ProtocolError::InternalFailure);
    }
    let output_id_length = read_fixture_string_length(input, 75)?;
    let output_realisation_length = read_fixture_string_length(input, 196)?;
    Ok(FixtureBuildPathsWithResultsResponse {
        result_count,
        path_length,
        status,
        error_length,
        output_count,
        output_id_length,
        output_realisation_length,
    })
}

fn read_fixture_optional_duration(input: &mut &[u8]) -> Result<(), ProtocolError> {
    match read_worker_integer(input)? {
        0 => Ok(()),
        1 => {
            read_worker_integer(input)?;
            Ok(())
        }
        _ => Err(ProtocolError::InternalFailure),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FixtureStderrFrame {
    Next {
        message_length: u64,
    },
    StartActivity {
        message_length: u64,
        field_count: u64,
        field_string_lengths: Vec<u64>,
    },
    StopActivity,
    Result {
        field_count: u64,
        field_string_lengths: Vec<u64>,
    },
    Last,
}

pub fn read_fixture_stderr_frame(input: &mut &[u8]) -> Result<FixtureStderrFrame, ProtocolError> {
    match read_worker_integer(input)? {
        STDERR_NEXT => Ok(FixtureStderrFrame::Next {
            message_length: read_fixture_string_length(input, 145)?,
        }),
        STDERR_START_ACTIVITY => {
            read_worker_integer(input)?;
            read_worker_integer(input)?;
            read_worker_integer(input)?;
            let message_length = read_fixture_string_length(input, 164)?;
            let (field_count, field_string_lengths) = read_fixture_activity_fields(input)?;
            read_worker_integer(input)?;
            Ok(FixtureStderrFrame::StartActivity {
                message_length,
                field_count,
                field_string_lengths,
            })
        }
        STDERR_STOP_ACTIVITY => {
            read_worker_integer(input)?;
            Ok(FixtureStderrFrame::StopActivity)
        }
        STDERR_RESULT => {
            read_worker_integer(input)?;
            read_worker_integer(input)?;
            let (field_count, field_string_lengths) = read_fixture_activity_fields(input)?;
            Ok(FixtureStderrFrame::Result {
                field_count,
                field_string_lengths,
            })
        }
        STDERR_LAST => Ok(FixtureStderrFrame::Last),
        _ => Err(ProtocolError::UnsupportedOperation),
    }
}

fn read_fixture_activity_fields(input: &mut &[u8]) -> Result<(u64, Vec<u64>), ProtocolError> {
    let field_count = read_fixture_count(input, 4)?;
    let mut field_string_lengths = Vec::new();
    for _ in 0..field_count {
        match read_worker_integer(input)? {
            0 => {
                read_worker_integer(input)?;
            }
            1 => field_string_lengths.push(read_fixture_string_length(input, 153)?),
            _ => return Err(ProtocolError::InternalFailure),
        }
    }
    Ok((field_count, field_string_lengths))
}

pub fn read_fixture_add_to_store_request(
    input: &mut &[u8],
) -> Result<FixtureAddToStoreRequest, ProtocolError> {
    if read_worker_operation(input)? != WorkerOperation::AddToStore {
        return Err(ProtocolError::UnsupportedOperation);
    }
    let name_length = read_fixture_string_length(input, 27)?;
    let content_address_length = read_fixture_string_length(input, 11)?;
    let reference_count = read_fixture_count(input, 0)?;
    for _ in 0..reference_count {
        read_fixture_string_length(input, 153)?;
    }
    read_fixture_boolean(input)?;

    let mut upload_chunk_lengths = Vec::new();
    let mut upload_length = 0_u64;
    loop {
        let length = read_worker_integer(input)?;
        if length == 0 {
            break;
        }
        if !upload_chunk_lengths.is_empty() || length > 502 {
            return Err(ProtocolError::SizeLimit);
        }
        upload_length = upload_length
            .checked_add(length)
            .filter(|length| *length <= 502)
            .ok_or(ProtocolError::SizeLimit)?;
        let length = usize::try_from(length).map_err(|_| ProtocolError::SizeLimit)?;
        if input.len() < length {
            return Err(ProtocolError::Truncated);
        }
        let (_, remaining) = input.split_at(length);
        *input = remaining;
        upload_chunk_lengths.push(length as u64);
    }
    Ok(FixtureAddToStoreRequest {
        name_length,
        content_address_length,
        reference_count,
        upload_chunk_lengths,
        upload_length,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixtureHandshake {
    pub version: WorkerVersion,
    pub feature_lengths: Vec<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixtureServerHandshakeInfo {
    pub daemon_version_length: Option<u64>,
    pub trust_status: Option<u64>,
}

pub fn read_fixture_client_handshake(input: &mut &[u8]) -> Result<FixtureHandshake, ProtocolError> {
    if read_worker_integer(input)? != CLIENT_WORKER_MAGIC {
        return Err(ProtocolError::VersionMismatch);
    }
    read_fixture_handshake_version(input)
}

pub fn read_fixture_server_handshake(input: &mut &[u8]) -> Result<FixtureHandshake, ProtocolError> {
    if read_worker_integer(input)? != SERVER_WORKER_MAGIC {
        return Err(ProtocolError::VersionMismatch);
    }
    read_fixture_handshake_version(input)
}

fn read_fixture_handshake_version(input: &mut &[u8]) -> Result<FixtureHandshake, ProtocolError> {
    let version = WorkerVersion::from_wire(read_worker_integer(input)?);
    let feature_lengths = if version >= FEATURE_NEGOTIATION_VERSION {
        read_fixture_string_lengths(input, 64, 1_024)?
    } else {
        Vec::new()
    };
    Ok(FixtureHandshake {
        version,
        feature_lengths,
    })
}

pub fn read_fixture_client_post_handshake(
    input: &mut &[u8],
    version: WorkerVersion,
) -> Result<(), ProtocolError> {
    if version >= WorkerVersion::new(1, 14) && read_worker_integer(input)? != 0 {
        read_worker_integer(input)?;
    }
    if version >= WorkerVersion::new(1, 11) {
        read_worker_integer(input)?;
    }
    Ok(())
}

pub fn read_fixture_server_handshake_info(
    input: &mut &[u8],
    version: WorkerVersion,
) -> Result<FixtureServerHandshakeInfo, ProtocolError> {
    let daemon_version_length = if version >= WorkerVersion::new(1, 33) {
        Some(read_fixture_string_length(input, 1_024)?)
    } else {
        None
    };
    let trust_status = if version >= WorkerVersion::new(1, 35) {
        let status = read_worker_integer(input)?;
        if status > 2 {
            return Err(ProtocolError::InternalFailure);
        }
        Some(status)
    } else {
        None
    };
    Ok(FixtureServerHandshakeInfo {
        daemon_version_length,
        trust_status,
    })
}

pub fn read_fixture_terminal_frame(input: &mut &[u8]) -> Result<(), ProtocolError> {
    if read_worker_integer(input)? == STDERR_LAST {
        Ok(())
    } else {
        Err(ProtocolError::UnsupportedOperation)
    }
}

pub fn read_fixture_set_options(input: &mut &[u8]) -> Result<FixtureSetOptions, ProtocolError> {
    if read_worker_operation(input)? != WorkerOperation::SetOptions {
        return Err(ProtocolError::UnsupportedOperation);
    }

    for _ in 0..12 {
        read_worker_integer(input)?;
    }

    let override_count = read_worker_integer(input)?;
    if override_count > 256 {
        return Err(ProtocolError::SizeLimit);
    }

    let mut string_lengths = Vec::with_capacity((override_count * 2) as usize);
    for _ in 0..override_count {
        for _ in 0..2 {
            string_lengths.push(read_fixture_string_length(input, 16_384)?);
        }
    }

    Ok(FixtureSetOptions {
        override_count,
        string_lengths,
    })
}

fn read_fixture_string_lengths(
    input: &mut &[u8],
    maximum_count: u64,
    maximum_length: u64,
) -> Result<Vec<u64>, ProtocolError> {
    let count = read_fixture_count(input, maximum_count)?;
    (0..count)
        .map(|_| read_fixture_string_length(input, maximum_length))
        .collect()
}

fn read_fixture_count(input: &mut &[u8], maximum: u64) -> Result<u64, ProtocolError> {
    let count = read_worker_integer(input)?;
    if count > maximum {
        return Err(ProtocolError::SizeLimit);
    }
    Ok(count)
}

fn read_fixture_boolean(input: &mut &[u8]) -> Result<bool, ProtocolError> {
    match read_worker_integer(input)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(ProtocolError::InternalFailure),
    }
}

fn read_fixture_string_length(
    input: &mut &[u8],
    maximum_length: u64,
) -> Result<u64, ProtocolError> {
    let length = read_worker_integer(input)?;
    if length > maximum_length {
        return Err(ProtocolError::SizeLimit);
    }
    let length = usize::try_from(length).map_err(|_| ProtocolError::SizeLimit)?;
    let padding_length = (8 - length % 8) % 8;
    let framed_length = length
        .checked_add(padding_length)
        .ok_or(ProtocolError::SizeLimit)?;
    if input.len() < framed_length {
        return Err(ProtocolError::Truncated);
    }
    let (framed, remaining) = input.split_at(framed_length);
    if framed[length..].iter().any(|byte| *byte != 0) {
        return Err(ProtocolError::InternalFailure);
    }
    *input = remaining;
    Ok(length as u64)
}

pub fn read_worker_integer(input: &mut &[u8]) -> Result<u64, ProtocolError> {
    if input.is_empty() {
        return Err(ProtocolError::CleanEof);
    }

    if input.len() < 8 {
        return Err(ProtocolError::Truncated);
    }

    let (encoded, remaining) = input.split_at(8);
    *input = remaining;
    let mut bytes = [0; 8];
    bytes.copy_from_slice(encoded);
    Ok(u64::from_le_bytes(bytes))
}

pub fn read_client_worker_magic(input: &mut &[u8]) -> Result<(), ProtocolError> {
    if read_worker_integer(input)? == CLIENT_WORKER_MAGIC {
        Ok(())
    } else {
        Err(ProtocolError::VersionMismatch)
    }
}

pub fn negotiate_worker_version(
    client_version: WorkerVersion,
    client_features: &[String],
    server_features: &[String],
) -> Result<NegotiatedWorkerVersion, ProtocolError> {
    let version = client_version.min(LATEST_WORKER_VERSION);
    if version < MINIMUM_WORKER_VERSION {
        return Err(ProtocolError::VersionMismatch);
    }

    let features = if version >= WorkerVersion::new(1, 38) {
        client_features
            .iter()
            .filter(|feature| server_features.contains(feature))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };

    Ok(NegotiatedWorkerVersion {
        version,
        features,
        _feature_charge: None,
    })
}
