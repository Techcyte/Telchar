//! Sends explicit administrative actions to the active daemon's local socket.

use std::io;
use std::os::unix::net::UnixStream;

pub(crate) fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut arguments = std::env::args().skip(2);
    if arguments.next().as_deref() != Some("microvm") {
        return Err(invalid("expected admin microvm action").into());
    }
    let action = arguments
        .next()
        .ok_or_else(|| invalid("administrative action required"))?;
    if !matches!(
        action.as_str(),
        "risk-release" | "attach-identity" | "verify-termination"
    ) {
        return Err(invalid("unknown administrative action").into());
    }
    let mut values = std::collections::BTreeMap::new();
    let mut acknowledged = false;
    while let Some(argument) = arguments.next() {
        if argument == "--accept-remote-execution-risk" && action == "risk-release" && !acknowledged
        {
            acknowledged = true;
        } else if matches!(
            argument.as_str(),
            "--socket" | "--intent" | "--provider-id" | "--reason" | "--evidence"
        ) {
            let value = arguments
                .next()
                .ok_or_else(|| invalid("missing administrative argument"))?;
            if values.insert(argument, value).is_some() {
                return Err(invalid("duplicate administrative argument").into());
            }
        } else {
            return Err(invalid("unknown administrative argument").into());
        }
    }
    if action == "risk-release" && !acknowledged {
        return Err(invalid("explicit remote execution risk acknowledgment required").into());
    }
    let socket = values
        .remove("--socket")
        .ok_or_else(|| invalid("--socket required"))?;
    let intent_id = values
        .remove("--intent")
        .ok_or_else(|| invalid("--intent required"))?;
    let provider_id = values.remove("--provider-id");
    let reason = values
        .remove("--reason")
        .ok_or_else(|| invalid("--reason required"))?;
    let evidence_reference = values
        .remove("--evidence")
        .ok_or_else(|| invalid("--evidence required"))?;
    let request = if action == "verify-termination" {
        telchar::service::admin::Request::VerifyTermination {
            intent_id,
            provider_id: provider_id.ok_or_else(|| invalid("--provider-id required"))?,
            reason,
            evidence_reference,
        }
    } else if action == "attach-identity" {
        telchar::service::admin::Request::AttachIdentity {
            intent_id,
            provider_id: provider_id.ok_or_else(|| invalid("--provider-id required"))?,
            reason,
            evidence_reference,
        }
    } else {
        telchar::service::admin::Request::RiskRelease {
            intent_id,
            provider_id,
            reason,
            evidence_reference,
            accept_remote_execution_risk: acknowledged,
        }
    };
    let mut stream = UnixStream::connect(socket)?;
    telchar::service::admin::write_request(&mut stream, &request)?;
    let response = telchar::service::admin::read_response(&mut stream)?;
    if response.rejected {
        return Err(invalid("administrative request rejected").into());
    }
    serde_json::to_writer(std::io::stdout().lock(), &response)?;
    println!();
    Ok(())
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
