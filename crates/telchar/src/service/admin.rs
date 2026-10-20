//! Root-authenticated administrative requests, independent of frontend identity.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

const MAXIMUM_FRAME_BYTES: usize = 4096;
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

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
) -> io::Result<()> {
    authorize(&stream)?;
    let request = read_request(&mut stream, REQUEST_TIMEOUT)?;
    let attachment = matches!(&request, Request::AttachIdentity { .. });
    let termination = matches!(&request, Request::VerifyTermination { .. });
    let result = match request {
        Request::VerifyTermination {
            intent_id,
            provider_id,
            reason,
            evidence_reference,
        } => verify_termination(
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
    write_frame(
        &mut stream,
        &serde_json::to_vec(&response).map_err(invalid)?,
    )
}

fn verify_termination(
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
    service.verify_termination(&crate::persistence::MicrovmTerminationObservation {
        intent_id,
        provider_id,
        account_id: &identity.account_id,
        region: &identity.region,
        actor_uid: 0,
        reason,
        evidence_reference,
    })
}

fn attach_identity(
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
    service.attach_identity(&crate::persistence::MicrovmIdentityAttachment {
        intent_id,
        provider_id,
        account_id: &identity.account_id,
        region: &identity.region,
        image_arn: &identity.image_arn,
        image_version: &identity.image_version,
        actor_uid: 0,
        reason,
        evidence_reference,
    })
}

fn read_frame(stream: &mut UnixStream, timeout: Duration) -> io::Result<Vec<u8>> {
    let deadline = Instant::now() + timeout;
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
    if bytes.is_empty() || bytes.len() > MAXIMUM_FRAME_BYTES {
        return Err(invalid("administrative frame exceeds size limit"));
    }
    stream.set_write_timeout(Some(REQUEST_TIMEOUT))?;
    stream.write_all(&(bytes.len() as u32).to_le_bytes())?;
    stream.write_all(bytes)
}

fn invalid(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}
