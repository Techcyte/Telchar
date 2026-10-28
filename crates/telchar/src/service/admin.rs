//! Root-authenticated administrative requests, independent of frontend identity.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

const MAXIMUM_FRAME_BYTES: usize = 4096;
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

pub struct RequestScope {
    deadline: Instant,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl RequestScope {
    pub fn database(
        &self,
        database: &crate::persistence::Database,
    ) -> crate::persistence::Database {
        database.for_request(self.deadline, self.cancelled.clone())
    }

    pub fn remaining(&self) -> io::Result<Duration> {
        if self.cancelled.load(std::sync::atomic::Ordering::Acquire) {
            return Err(io::Error::other(
                "administrative request cancelled; mutation outcome may be unknown",
            ));
        }
        self.deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "administrative request timed out; mutation outcome may be unknown",
                )
            })
    }
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Request {
    VerifyTermination {
        intent_id: String,
        provider_id: String,
        reason: String,
        evidence_reference: String,
    },
    AttachIdentity {
        intent_id: String,
        provider_id: String,
        reason: String,
        evidence_reference: String,
    },
    RiskRelease {
        intent_id: String,
        provider_id: Option<String>,
        reason: String,
        evidence_reference: String,
        accept_remote_execution_risk: bool,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub released: bool,
    pub attached: bool,
    pub terminated: bool,
    pub rejected: bool,
}

pub fn authorize(stream: &UnixStream) -> io::Result<()> {
    super::ipc::authorize_peer(stream, 0)
}

pub fn read_request(stream: &mut UnixStream, timeout: Duration) -> io::Result<Request> {
    serde_json::from_slice(&read_frame(stream, timeout)?).map_err(invalid)
}

pub fn write_request(stream: &mut UnixStream, request: &Request) -> io::Result<()> {
    write_frame(stream, &serde_json::to_vec(request).map_err(invalid)?)
}

pub fn read_response(stream: &mut UnixStream) -> io::Result<Response> {
    serde_json::from_slice(&read_frame(stream, REQUEST_TIMEOUT)?).map_err(invalid)
}

pub fn serve(
    mut stream: UnixStream,
    database: &crate::persistence::Database,
    services: &[std::sync::Arc<crate::microvm::service::Service>],
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> io::Result<()> {
    authorize(&stream)?;
    let scope = RequestScope {
        deadline: Instant::now() + REQUEST_TIMEOUT,
        cancelled,
    };
    let database = &scope.database(database);
    scope.remaining()?;
    let request: Request =
        serde_json::from_slice(&read_frame_until(&mut stream, scope.deadline)?).map_err(invalid)?;
    let attachment = matches!(&request, Request::AttachIdentity { .. });
    let termination = matches!(&request, Request::VerifyTermination { .. });
    let result = match request {
        Request::VerifyTermination {
            intent_id,
            provider_id,
            reason,
            evidence_reference,
        } => verify_termination(
            &scope,
            database,
            services,
            &intent_id,
            &provider_id,
            &reason,
            &evidence_reference,
        ),
        Request::AttachIdentity {
            intent_id,
            provider_id,
            reason,
            evidence_reference,
        } => attach_identity(
            &scope,
            database,
            services,
            &intent_id,
            &provider_id,
            &reason,
            &evidence_reference,
        ),
        Request::RiskRelease {
            intent_id,
            provider_id,
            reason,
            evidence_reference,
            accept_remote_execution_risk,
        } => crate::persistence::release_microvm_capacity_risk(
            database,
            &crate::persistence::MicrovmRiskRelease {
                intent_id: &intent_id,
                provider_id: provider_id.as_deref(),
                actor_uid: 0,
                reason: &reason,
                evidence_reference: &evidence_reference,
                accept_remote_execution_risk,
            },
        )
        .map_err(io::Error::other),
    };
    let changed = result.as_ref().copied().unwrap_or(false);
    let response = Response {
        released: !attachment && !termination && changed,
        attached: attachment && changed,
        terminated: termination && changed,
        rejected: result.is_err(),
    };
    write_frame_until(
        &mut stream,
        &serde_json::to_vec(&response).map_err(invalid)?,
        scope.deadline,
    )
}

fn verify_termination(
    scope: &RequestScope,
    database: &crate::persistence::Database,
    services: &[std::sync::Arc<crate::microvm::service::Service>],
    intent_id: &str,
    provider_id: &str,
    reason: &str,
    evidence_reference: &str,
) -> io::Result<bool> {
    let identity =
        crate::persistence::microvm_termination_identity(database, intent_id, provider_id)
            .map_err(io::Error::other)?
            .ok_or_else(|| invalid("unresolved MicroVM termination unavailable"))?;
    let service = services
        .iter()
        .find(|service| {
            service.configuration().account_id() == identity.account_id
                && service.configuration().region() == identity.region
        })
        .ok_or_else(|| invalid("MicroVM authority unavailable"))?;
    service.verify_termination(
        scope,
        &crate::persistence::MicrovmTerminationObservation {
            intent_id,
            provider_id,
            account_id: &identity.account_id,
            region: &identity.region,
            actor_uid: 0,
            reason,
            evidence_reference,
        },
    )
}

fn attach_identity(
    scope: &RequestScope,
    database: &crate::persistence::Database,
    services: &[std::sync::Arc<crate::microvm::service::Service>],
    intent_id: &str,
    provider_id: &str,
    reason: &str,
    evidence_reference: &str,
) -> io::Result<bool> {
    let identity = crate::persistence::unresolved_microvm_identity(database, intent_id)
        .map_err(io::Error::other)?
        .ok_or_else(|| invalid("unresolved MicroVM intent unavailable"))?;
    let service = services
        .iter()
        .find(|service| {
            service.configuration().account_id() == identity.account_id
                && service.configuration().region() == identity.region
        })
        .ok_or_else(|| invalid("MicroVM authority unavailable"))?;
    service.attach_identity(
        scope,
        &crate::persistence::MicrovmIdentityAttachment {
            intent_id,
            provider_id,
            account_id: &identity.account_id,
            region: &identity.region,
            image_arn: &identity.image_arn,
            image_version: &identity.image_version,
            actor_uid: 0,
            reason,
            evidence_reference,
        },
    )
}

fn read_frame(stream: &mut UnixStream, timeout: Duration) -> io::Result<Vec<u8>> {
    read_frame_until(stream, Instant::now() + timeout)
}

fn read_frame_until(stream: &mut UnixStream, deadline: Instant) -> io::Result<Vec<u8>> {
    let mut length = [0; 4];
    read_until(stream, &mut length, deadline)?;
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > MAXIMUM_FRAME_BYTES {
        return Err(invalid("administrative frame exceeds size limit"));
    }
    let mut bytes = vec![0; length];
    read_until(stream, &mut bytes, deadline)?;
    Ok(bytes)
}

fn read_until(stream: &mut UnixStream, mut bytes: &mut [u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::TimedOut, "administrative request timed out")
            })?;
        stream.set_read_timeout(Some(remaining))?;
        let count = stream.read(bytes)?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "incomplete administrative frame",
            ));
        }
        bytes = &mut bytes[count..];
    }
    Ok(())
}

fn write_frame(stream: &mut UnixStream, bytes: &[u8]) -> io::Result<()> {
    write_frame_until(stream, bytes, Instant::now() + REQUEST_TIMEOUT)
}

fn write_frame_until(stream: &mut UnixStream, bytes: &[u8], deadline: Instant) -> io::Result<()> {
    if bytes.is_empty() || bytes.len() > MAXIMUM_FRAME_BYTES {
        return Err(invalid("administrative frame exceeds size limit"));
    }
    let frame = [&(bytes.len() as u32).to_le_bytes()[..], bytes].concat();
    let mut remaining = frame.as_slice();
    while !remaining.is_empty() {
        let timeout = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::TimedOut, "administrative response timed out")
            })?;
        stream.set_write_timeout(Some(timeout))?;
        let written = stream.write(remaining)?;
        if written == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        remaining = &remaining[written..];
    }
    Ok(())
}

fn invalid(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}
