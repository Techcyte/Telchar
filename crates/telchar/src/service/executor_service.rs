//! Implements the bounded local executor IPC protocol and its idempotent execution registry.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::build::BuildRequest;
use crate::persistence::{
    self, LocalBackendExecution, LocalBackendExecutionFailure, LocalBackendExecutionState,
};

pub const EXECUTOR_PROTOCOL_VERSION: u32 = 1;
pub const MAXIMUM_EXECUTOR_FRAME_BYTES: usize = 1024 * 1024;
pub const EXECUTOR_IO_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ExecutorRequest {
    Submit {
        version: u32,
        backend_execution_id: String,
        idempotency_key: String,
        specification: Box<ExecutorSpecification>,
    },
    Status {
        version: u32,
        backend_execution_id: String,
    },
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ExecutorSpecification {
    pub request_id: String,
    pub timeout_seconds: u64,
    pub build: BuildRequest,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ExecutorResponse {
    pub version: u32,
    pub result: ExecutorResult,
    pub execution: Option<ExecutorExecution>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutorResult {
    Accepted,
    Found,
    NotFound,
    Conflict,
    Invalid,
    Failed,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ExecutorExecution {
    pub backend_execution_id: String,
    pub idempotency_key: String,
    pub state: ExecutorExecutionState,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutorExecutionState {
    Accepted,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

pub fn handle_connection(database_url: &str, mut stream: UnixStream) -> io::Result<()> {
    handle_connection_with_submit(database_url, &mut stream, &mut |_, _, _| Ok(()))
}

pub fn handle_connection_with_submit(
    database_url: &str,
    stream: &mut UnixStream,
    submit: &mut dyn FnMut(&str, &ExecutorSpecification, &LocalBackendExecution) -> io::Result<()>,
) -> io::Result<()> {
    stream.set_read_timeout(Some(EXECUTOR_IO_TIMEOUT))?;
    stream.set_write_timeout(Some(EXECUTOR_IO_TIMEOUT))?;
    let request: ExecutorRequest = read_frame(stream)?;
    let response = match request {
        ExecutorRequest::Submit {
            version,
            backend_execution_id,
            idempotency_key,
            specification,
        } => {
            let specification_bytes = serde_json::to_vec(&specification).unwrap_or_default();
            if version != EXECUTOR_PROTOCOL_VERSION
                || specification.request_id.is_empty()
                || specification.timeout_seconds == 0
                || specification_bytes.is_empty()
                || specification_bytes.len() > MAXIMUM_EXECUTOR_FRAME_BYTES
            {
                invalid_response()
            } else {
                let digest: [u8; 32] = Sha256::digest(&specification_bytes).into();
                match persistence::register_local_backend_execution(
                    database_url,
                    &backend_execution_id,
                    &idempotency_key,
                    &digest,
                ) {
                    Ok(execution) => {
                        match submit(&backend_execution_id, &specification, &execution) {
                            Ok(()) => execution_response(ExecutorResult::Accepted, execution),
                            Err(_) => result_response(ExecutorResult::Failed),
                        }
                    }
                    Err(error) if error.failure() == LocalBackendExecutionFailure::Conflict => {
                        result_response(ExecutorResult::Conflict)
                    }
                    Err(_) => result_response(ExecutorResult::Failed),
                }
            }
        }
        ExecutorRequest::Status {
            version,
            backend_execution_id,
        } => {
            if version != EXECUTOR_PROTOCOL_VERSION {
                invalid_response()
            } else {
                match persistence::read_local_backend_execution(database_url, &backend_execution_id)
                {
                    Ok(Some(execution)) => execution_response(ExecutorResult::Found, execution),
                    Ok(None) => result_response(ExecutorResult::NotFound),
                    Err(_) => result_response(ExecutorResult::Failed),
                }
            }
        }
    };
    write_frame(stream, &response)
}

pub fn send_request(
    stream: &mut UnixStream,
    request: &ExecutorRequest,
) -> io::Result<ExecutorResponse> {
    stream.set_read_timeout(Some(EXECUTOR_IO_TIMEOUT))?;
    stream.set_write_timeout(Some(EXECUTOR_IO_TIMEOUT))?;
    write_frame(stream, request)?;
    read_frame(stream)
}

fn execution_response(
    result: ExecutorResult,
    execution: LocalBackendExecution,
) -> ExecutorResponse {
    ExecutorResponse {
        version: EXECUTOR_PROTOCOL_VERSION,
        result,
        execution: Some(ExecutorExecution {
            backend_execution_id: execution.backend_execution_id,
            idempotency_key: execution.idempotency_key,
            state: match execution.state {
                LocalBackendExecutionState::Accepted => ExecutorExecutionState::Accepted,
                LocalBackendExecutionState::Running => ExecutorExecutionState::Running,
                LocalBackendExecutionState::Succeeded => ExecutorExecutionState::Succeeded,
                LocalBackendExecutionState::Failed => ExecutorExecutionState::Failed,
                LocalBackendExecutionState::Cancelled => ExecutorExecutionState::Cancelled,
            },
        }),
    }
}

fn invalid_response() -> ExecutorResponse {
    result_response(ExecutorResult::Invalid)
}

fn result_response(result: ExecutorResult) -> ExecutorResponse {
    ExecutorResponse {
        version: EXECUTOR_PROTOCOL_VERSION,
        result,
        execution: None,
    }
}

fn read_frame<T: for<'de> Deserialize<'de>>(stream: &mut UnixStream) -> io::Result<T> {
    let mut length = [0_u8; 4];
    stream.read_exact(&mut length)?;
    let length = usize::try_from(u32::from_le_bytes(length))
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "executor frame is invalid"))?;
    if length == 0 || length > MAXIMUM_EXECUTOR_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "executor frame is invalid",
        ));
    }
    let mut bytes = vec![0_u8; length];
    stream.read_exact(&mut bytes)?;
    serde_json::from_slice(&bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "executor frame is invalid"))
}

fn write_frame<T: Serialize>(stream: &mut UnixStream, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec(value)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "executor frame is invalid"))?;
    if bytes.is_empty() || bytes.len() > MAXIMUM_EXECUTOR_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "executor frame is invalid",
        ));
    }
    let length = u32::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "executor frame is invalid"))?;
    stream.write_all(&length.to_le_bytes())?;
    stream.write_all(&bytes)?;
    stream.flush()
}
