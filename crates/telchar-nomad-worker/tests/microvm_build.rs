use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use telchar::microvm::session::{Limits, Session};
use telchar::nomad::protocol::{
    BuildOutcome, BuildSpecification, InputManifest, NamedOutput, PathManifestEntry,
};
use telchar::store::daemon::{GatewayStoreConnection, GatewayStoreEndpoint};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/microvm/canonical-store.sh"
);

#[test]
fn dedicated_worker_builds_and_returns_registered_output() {
    if std::env::var_os("MICROVM_TEST_STORE_URI").is_none() {
        let output = Command::new("bash")
            .arg(FIXTURE)
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "dedicated_worker_builds_and_returns_registered_output",
                "--test-threads=1",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let root = std::path::PathBuf::from(std::env::var_os("MICROVM_STORE_ROOT").unwrap());
    let endpoint =
        GatewayStoreEndpoint::parse(&std::env::var("MICROVM_TEST_STORE_URI").unwrap()).unwrap();
    let source = root.join("input");
    std::fs::write(&source, b"dedicated-worker-success\n").unwrap();
    let added = Command::new("nix-store")
        .args(["--store", "local", "--add"])
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    assert!(added.stderr.is_empty());
    let input = String::from_utf8(added.stdout).unwrap().trim().to_owned();
    let mut store = GatewayStoreConnection::connect(&endpoint).unwrap();
    let info = store.query_path_info(input.as_bytes()).unwrap().unwrap();
    let output = "/nix/store/ffffffffffffffffffffffffffffffff-dedicated-output";
    let drv = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-dedicated.drv";
    assert!(!store.is_valid_path(output.as_bytes()).unwrap());
    let manifest = InputManifest {
        derivation_path: drv.into(),
        build: BuildSpecification {
            derivation_path: drv.as_bytes().to_vec(),
            outputs: vec![NamedOutput {
                name: b"out".to_vec(),
                path: output.as_bytes().to_vec(),
                hash_algorithm: vec![],
                hash: vec![],
            }],
            input_sources: vec![input.as_bytes().to_vec()],
            system: "x86_64-linux".into(),
            required_system_features: vec![],
            builder: b"/bin/sh".to_vec(),
            arguments: vec![
                b"-c".to_vec(),
                format!("read -r value < {input}; printf '%s\\n' \"$value\" > \"$out\"")
                    .into_bytes(),
            ],
            environment: vec![(b"out".to_vec(), output.as_bytes().to_vec())],
        },
        paths: vec![PathManifestEntry {
            path: input,
            nar_hash: info.nar_hash_hex().into(),
            nar_size: info.nar_size(),
            references: vec![],
            deriver: None,
            content_address: None,
        }],
        outputs: vec![output.into()],
    };
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let binding = root.join("binding.json");
    let trust = root.join("trust.der");
    std::fs::write(&trust, include_bytes!("fixtures/microvm-cert.der")).unwrap();
    std::fs::write(&binding, serde_json::to_vec(&serde_json::json!({
        "endpoint": format!("wss://localhost:{}/microvm", address.port()), "address": address.to_string(),
        "intent_id": "build-intent", "provider_id": "microvm-build", "bearer": "s".repeat(43), "lifetime_seconds": 15
    })).unwrap()).unwrap();
    std::fs::set_permissions(&binding, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut worker = Command::new("bash")
        .arg(FIXTURE)
        .arg("/bin/sh")
        .args([
            "-c",
            "exec \"$1\" \"$2\" \"$3\" \"$MICROVM_TEST_STORE_URI\"",
            "worker",
        ])
        .arg(env!("CARGO_BIN_EXE_telchar-microvm-worker"))
        .arg(&binding)
        .arg(&trust)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    listener.set_nonblocking(true).unwrap();
    let started = std::time::Instant::now();
    let stream = loop {
        if let Ok((stream, _)) = listener.accept() {
            break stream;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "worker did not connect"
        );
        if let Some(status) = worker.try_wait().unwrap() {
            panic!(
                "worker exited {status}: {:?}",
                worker.wait_with_output().unwrap()
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    let config = rustls::ServerConfig::builder_with_provider(
        rustls::crypto::ring::default_provider().into(),
    )
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![include_bytes!("fixtures/microvm-cert.der").to_vec().into()],
        rustls::pki_types::PrivateKeyDer::Pkcs8(
            include_bytes!("fixtures/microvm-key.der").to_vec().into(),
        ),
    )
    .unwrap();
    let tls = rustls::StreamOwned::new(
        rustls::ServerConnection::new(Arc::new(config)).unwrap(),
        stream,
    );
    #[allow(clippy::result_large_err)]
    let upgrade = |_: &tungstenite::handshake::server::Request,
                   mut response: tungstenite::handshake::server::Response| {
        response.headers_mut().insert(
            "sec-websocket-protocol",
            "telchar-microvm-v1".parse().unwrap(),
        );
        Ok(response)
    };
    let mut socket = tungstenite::accept_hdr(tls, upgrade).unwrap();
    let bootstrap: serde_json::Value =
        serde_json::from_slice(&socket.read().unwrap().into_data()).unwrap();
    assert_eq!(bootstrap["intent_id"], "build-intent");
    let limits = Limits {
        maximum_paths: 10,
        maximum_nar_bytes: 1024 * 1024,
        maximum_total_bytes: 2 * 1024 * 1024,
        metadata_bytes: 8192,
        chunk_bytes: 256 * 1024,
    };
    let result = Session::new(manifest, endpoint.clone(), root.clone(), limits)
        .unwrap()
        .run(socket, || Ok(()), |_| {});
    let worker = worker.wait_with_output().unwrap();
    assert!(
        worker.status.success(),
        "{result:?}: {}",
        String::from_utf8_lossy(&worker.stderr)
    );
    assert!(worker.stdout.is_empty());
    assert!(
        worker.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&worker.stderr)
    );
    assert_eq!(result.unwrap().outcome, BuildOutcome::Built);
    assert!(store.is_valid_path(output.as_bytes()).unwrap());
    assert_eq!(
        std::fs::read(output).unwrap(),
        b"dedicated-worker-success\n"
    );
}
