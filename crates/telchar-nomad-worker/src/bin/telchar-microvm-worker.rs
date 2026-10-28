//! Runs one execution using a protected runtime binding and configured TLS roots.

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

fn phase<T>(
    name: &'static str,
    deadline: Instant,
    operation: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    let started = Instant::now();
    let result = operation();
    if result.is_err() && Instant::now() >= deadline && name != "report-failure" {
        opentelemetry::global::meter("telchar-microvm-worker")
            .u64_counter("telchar.microvm.worker.deadline")
            .build()
            .add(1, &[opentelemetry::KeyValue::new("outcome", "expired")]);
    }
    opentelemetry::global::meter("telchar-microvm-worker")
        .f64_histogram("telchar.microvm.worker.phase.duration")
        .with_unit("s")
        .build()
        .record(
            started.elapsed().as_secs_f64(),
            &[
                opentelemetry::KeyValue::new("phase", name),
                opentelemetry::KeyValue::new(
                    "outcome",
                    if result.is_ok() { "success" } else { "failure" },
                ),
            ],
        );
    result
}

fn trust_roots(path: impl AsRef<std::path::Path>) -> io::Result<rustls::RootCertStore> {
    use rustls::pki_types::pem::PemObject;

    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(1048577)
        .read_to_end(&mut bytes)?;
    if bytes.is_empty() || bytes.len() > 1048576 {
        return Err(invalid());
    }
    let mut roots = rustls::RootCertStore::empty();
    if bytes.first() != Some(&0x30) {
        for certificate in rustls::pki_types::CertificateDer::pem_slice_iter(&bytes) {
            roots
                .add(certificate.map_err(|_| invalid())?)
                .map_err(|_| invalid())?;
        }
    } else {
        roots.add(bytes.into()).map_err(|_| invalid())?;
    }
    if roots.is_empty() {
        return Err(invalid());
    }
    Ok(roots)
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
    let roots = trust_roots(trust)?;
    let tls = rustls::ClientConfig::builder_with_provider(
        rustls::crypto::ring::default_provider().into(),
    )
    .with_safe_default_protocol_versions()
    .map_err(|_| invalid())?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let store = store.to_str().ok_or_else(invalid)?;
    telchar::store::daemon::GatewayStoreEndpoint::parse(store).map_err(|_| invalid())?;
    let socket = phase("connect", deadline, || {
        telchar_nomad_worker::microvm::connect(
            &execution.endpoint,
            execution.address,
            Arc::new(tls),
            deadline,
            &execution.intent_id,
            &execution.provider_id,
            &execution.bearer,
        )
    })?;
    let mut session = phase("manifest", deadline, || {
        telchar_nomad_worker::microvm::receive_session(
            socket,
            8 * 1024 * 1024,
            256 * 1024,
            deadline,
        )
    })?;
    let requested = phase("resolve-inputs", deadline, || session.resolve_inputs(store))?;
    phase("import-inputs", deadline, || {
        session.import_requested_inputs(store, &requested)
    })?;
    let result = match phase("build", deadline, || session.build(store)) {
        Ok(result) => result,
        Err(error) => {
            let _ = phase("report-failure", deadline, || {
                session.report_failure(&error, 4096)
            });
            return Err(error);
        }
    };
    phase("return-outputs", deadline, || {
        session.return_outputs(store, &result)
    })
}

fn main() -> std::process::ExitCode {
    let telemetry = match telchar_telemetry::Telemetry::initialize(
        "telchar-microvm-worker",
        env!("CARGO_PKG_VERSION"),
    ) {
        Ok(telemetry) => telemetry,
        Err(_) => {
            eprintln!("MicroVM worker failed");
            return std::process::ExitCode::FAILURE;
        }
    };
    let started = Instant::now();
    let result = {
        let execution = tracing::info_span!("microvm.worker.execution");
        let _entered = execution.enter();
        let result = run();
        if let Err(error) = &result {
            tracing::error!(event = "microvm.worker.failed", error_kind = ?error.kind());
        }
        opentelemetry::global::meter("telchar-microvm-worker")
            .f64_histogram("telchar.microvm.worker.duration")
            .with_unit("s")
            .build()
            .record(
                started.elapsed().as_secs_f64(),
                &[opentelemetry::KeyValue::new(
                    "outcome",
                    if result.is_ok() { "success" } else { "failure" },
                )],
            );
        result
    };
    telemetry.shutdown();
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(_) => {
            eprintln!("MicroVM worker failed");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::trust_roots;

    #[test]
    fn loads_standard_ca_bundle_and_private_der() {
        let bundle = std::env::var_os("SSL_CERT_FILE").expect("test CA bundle");
        let roots = trust_roots(bundle).expect("standard CA roots");
        assert!(roots.len() > 1);
        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/microvm-cert.der"
        );
        assert_eq!(trust_roots(fixture).unwrap().len(), 1);
    }

    #[test]
    fn rejects_invalid_or_missing_explicit_trust() {
        let path = std::env::temp_dir().join(format!("microvm-trust-{}", std::process::id()));
        for bytes in [
            Vec::new(),
            b"invalid certificate".to_vec(),
            vec![0; 1048577],
        ] {
            std::fs::write(&path, bytes).unwrap();
            assert!(trust_roots(&path).is_err());
        }
        std::fs::remove_file(&path).unwrap();
        assert!(trust_roots(&path).is_err());
    }

    #[test]
    fn loads_private_pem_bundle() {
        let fixture = include_bytes!("../../tests/fixtures/microvm-cert.der");
        use base64::Engine;
        let pem = format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
            base64::engine::general_purpose::STANDARD.encode(fixture)
        );
        let path = std::env::temp_dir().join(format!("microvm-pem-{}", std::process::id()));
        std::fs::write(&path, &pem).unwrap();
        let roots = trust_roots(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(roots.len(), 1);
    }
}
