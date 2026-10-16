//! Runs one execution using a protected runtime binding and image-owned TLS trust.

use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Execution {
    endpoint: String,
    address: std::net::SocketAddr,
    intent_id: String,
    provider_id: String,
    bearer: String,
    lifetime_seconds: u64,
}

fn invalid() -> io::Error {
    io::Error::other("invalid MicroVM execution")
}

fn run() -> io::Result<()> {
    let started = Instant::now();
    let mut arguments = std::env::args_os().skip(1);
    let binding = arguments.next().ok_or_else(invalid)?;
    let trust = arguments.next().ok_or_else(invalid)?;
    let store = arguments.next().ok_or_else(invalid)?;
    if arguments.next().is_some() {
        return Err(invalid());
    }
    let before = std::fs::symlink_metadata(&binding)?;
    if !before.is_file() || before.mode() & 0o777 != 0o600 || before.nlink() != 1 {
        return Err(invalid());
    }
    let file = std::fs::File::open(&binding)?;
    let after = file.metadata()?;
    if before.dev() != after.dev() || before.ino() != after.ino() || after.mode() & 0o777 != 0o600 {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    file.take(16385).read_to_end(&mut bytes)?;
    if bytes.len() > 16384 {
        return Err(invalid());
    }
    let execution: Execution = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if !(1..=28800).contains(&execution.lifetime_seconds)
        || execution.intent_id.is_empty()
        || execution.intent_id.len() > 128
        || execution.provider_id.is_empty()
        || execution.provider_id.len() > 256
        || execution.bearer.len() != 43
    {
        return Err(invalid());
    }
    let deadline = started
        .checked_add(Duration::from_secs(execution.lifetime_seconds))
        .ok_or_else(invalid)?;
    let mut certificate = Vec::new();
    std::fs::File::open(trust)?
        .take(65537)
        .read_to_end(&mut certificate)?;
    if certificate.len() > 65536 {
        return Err(invalid());
    }
    let mut roots = rustls::RootCertStore::empty();
    roots.add(certificate.into()).map_err(|_| invalid())?;
    let tls = rustls::ClientConfig::builder_with_provider(
        rustls::crypto::ring::default_provider().into(),
    )
    .with_safe_default_protocol_versions()
    .map_err(|_| invalid())?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let store = store.to_str().ok_or_else(invalid)?;
    telchar::store::daemon::GatewayStoreEndpoint::parse(store).map_err(|_| invalid())?;
    let socket = telchar_nomad_worker::microvm::connect(
        &execution.endpoint,
        execution.address,
        Arc::new(tls),
        deadline,
        &execution.intent_id,
        &execution.provider_id,
        &execution.bearer,
    )?;
    let mut session = telchar_nomad_worker::microvm::receive_session(
        socket,
        8 * 1024 * 1024,
        256 * 1024,
        deadline,
    )?;
    let requested = session.resolve_inputs(store)?;
    session.import_requested_inputs(store, &requested)?;
    let result = match session.build(store) {
        Ok(result) => result,
        Err(error) => {
            let _ = session.report_failure(&error, 4096);
            return Err(error);
        }
    };
    session.return_outputs(store, &result)
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(_) => {
            eprintln!("MicroVM worker failed");
            std::process::ExitCode::FAILURE
        }
    }
}
