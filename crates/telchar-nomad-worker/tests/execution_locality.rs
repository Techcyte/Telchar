//! Exercises assigned builds against a real daemon with an observable declining remote hook.

use std::collections::BTreeMap;
use std::fs;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use nix_worker_protocol::{
    BuildDerivationClientRequest, BuildDerivationOutputRequest, WorkerClient,
};
use telchar::backend::local::GatewayStoreExecutor;
use telchar::backend::static_ssh::StaticSshBackend;
use telchar::backend::{BackendKind, BackendTarget, BuildBackend, BuildExecution};
use telchar::build::BuildRequest;
use telchar::nomad::protocol::{
    BuildSpecification, Frame, FrameKind, InputManifest, NamedOutput, PathManifestEntry,
    ProtocolLimits, encode_metadata, write_frame,
};
use telchar::service::config::ServiceConfig;
use telchar::store::daemon::GatewayStoreEndpoint;
use telchar_nomad_worker::{WorkerConfig, receive_manifest};

struct Daemon(Child, PathBuf);

impl Drop for Daemon {
    fn drop(&mut self) {
        self.0.kill().expect("fixture daemon terminates");
        self.0.wait().expect("fixture daemon reaps");
        let diagnostic = fs::read_to_string(&self.1).unwrap();
        assert!(!diagnostic.contains("error:"), "{diagnostic}");
    }
}

fn command(arguments: &[&str]) -> String {
    let output = Command::new(arguments[0])
        .args(&arguments[1..])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn instantiate(name: &str) -> BuildSpecification {
    let expression = format!(
        "derivation {{ name = \"{name}\"; system = builtins.currentSystem; builder = \"/bin/sh\"; args = [ \"-c\" \"printf terminal > $out\" ]; }}"
    );
    let link = PathBuf::from(std::env::var_os("TELCHAR_EXECUTION_ROOT").unwrap()).join(name);
    let drv = command(&[
        "nix-instantiate",
        "--store",
        "local",
        "--add-root",
        link.to_str().unwrap(),
        "--expr",
        &expression,
    ]);
    let drv = fs::canonicalize(drv).unwrap().to_str().unwrap().to_owned();
    let metadata: serde_json::Value = serde_json::from_str(&command(&[
        "nix",
        "--extra-experimental-features",
        "nix-command",
        "derivation",
        "show",
        "--store",
        "local",
        &drv,
    ]))
    .unwrap();
    let derivation =
        &metadata["derivations"][Path::new(&drv).file_name().unwrap().to_str().unwrap()];
    let output = derivation["outputs"]["out"]["path"].as_str().unwrap();
    let output = if output.starts_with('/') {
        output.to_owned()
    } else {
        format!("/nix/store/{output}")
    };
    BuildSpecification {
        derivation_path: drv.into_bytes(),
        outputs: vec![NamedOutput {
            name: b"out".to_vec(),
            path: output.into_bytes(),
            hash_algorithm: vec![],
            hash: vec![],
        }],
        input_sources: vec![],
        system: derivation["system"].as_str().unwrap().to_owned(),
        required_system_features: vec![],
        builder: derivation["builder"].as_str().unwrap().as_bytes().to_vec(),
        arguments: derivation["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|arg| arg.as_str().unwrap().as_bytes().to_vec())
            .collect(),
        environment: derivation["env"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(key, value)| {
                (
                    key.as_bytes().to_vec(),
                    value.as_str().unwrap().as_bytes().to_vec(),
                )
            })
            .collect(),
    }
}

fn wire_request(build: &BuildSpecification) -> Vec<u8> {
    fn integer(wire: &mut Vec<u8>, value: u64) {
        wire.extend_from_slice(&value.to_le_bytes());
    }
    fn string(wire: &mut Vec<u8>, value: &[u8]) {
        integer(wire, value.len() as u64);
        wire.extend_from_slice(value);
        wire.resize(wire.len() + (8 - value.len() % 8) % 8, 0);
    }
    let mut wire = Vec::new();
    string(&mut wire, &build.derivation_path);
    integer(&mut wire, build.outputs.len() as u64);
    for output in &build.outputs {
        for value in [
            &output.name,
            &output.path,
            &output.hash_algorithm,
            &output.hash,
        ] {
            string(&mut wire, value);
        }
    }
    integer(&mut wire, 0);
    string(&mut wire, build.system.as_bytes());
    string(&mut wire, &build.builder);
    integer(&mut wire, build.arguments.len() as u64);
    for argument in &build.arguments {
        string(&mut wire, argument);
    }
    integer(&mut wire, build.environment.len() as u64);
    for (key, value) in &build.environment {
        string(&mut wire, key);
        string(&mut wire, value);
    }
    integer(&mut wire, 0);
    wire
}

fn admit(build: &BuildSpecification, kind: BackendKind) -> BuildRequest {
    let wire = wire_request(build);
    let mut reader = nix_worker_protocol::WorkerReader::new(
        wire.as_slice(),
        nix_worker_protocol::ProtocolSessionLimits::DEFAULT,
    );
    let request = reader.complete_build_derivation().unwrap();
    BuildRequest::from_worker_request(
        &request,
        &[BackendTarget::new("fixture", kind, &build.system, [] as [&str; 0]).unwrap()],
    )
    .unwrap()
}

fn ordinary_build(build: &BuildSpecification, socket: &Path) {
    let stream = UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut client = WorkerClient::connect(stream).unwrap();
    assert!(
        client.is_valid_path(&build.derivation_path).unwrap(),
        "drv must be registered"
    );
    let outputs = [BuildDerivationOutputRequest {
        name: b"out",
        path: &build.outputs[0].path,
        hash_algorithm: b"",
        hash: b"",
    }];
    client
        .build_derivation(
            &BuildDerivationClientRequest {
                drv_path: &build.derivation_path,
                outputs: &outputs,
                input_sources: &[],
                platform: build.system.as_bytes(),
                builder: &build.builder,
                arguments: &build.arguments,
                environment: &build.environment,
            },
            &mut |_| Ok(()),
        )
        .unwrap();
}

#[allow(clippy::result_large_err)]
fn select_protocol(
    _: &tungstenite::handshake::server::Request,
    mut response: tungstenite::handshake::server::Response,
) -> Result<tungstenite::handshake::server::Response, tungstenite::handshake::server::ErrorResponse>
{
    response.headers_mut().insert(
        "sec-websocket-protocol",
        "telchar-nomad-transfer-v1".parse().unwrap(),
    );
    Ok(response)
}

fn worker_build(build: BuildSpecification, uri: &str) {
    let endpoint = GatewayStoreEndpoint::parse(uri).unwrap();
    let info = telchar::store::daemon::GatewayStoreConnection::connect(&endpoint)
        .unwrap()
        .query_path_info(&build.derivation_path)
        .unwrap()
        .unwrap();
    let entry = PathManifestEntry {
        path: String::from_utf8(build.derivation_path.clone()).unwrap(),
        nar_hash: info.nar_hash_hex().to_owned(),
        nar_size: info.nar_size(),
        references: vec![],
        deriver: None,
        content_address: info
            .content_address()
            .map(|value| String::from_utf8(value.to_vec()).unwrap()),
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("ws://{}/callback", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut socket = tungstenite::accept_hdr(stream, select_protocol).unwrap();
        socket.read().unwrap();
        let manifest = InputManifest {
            derivation_path: String::from_utf8(build.derivation_path.clone()).unwrap(),
            outputs: vec![String::from_utf8(build.outputs[0].path.clone()).unwrap()],
            build,
            paths: vec![entry],
        };
        let mut bytes = Vec::new();
        write_frame(
            &mut bytes,
            &Frame::new(
                FrameKind::InputManifest,
                encode_metadata(&manifest, 8 * 1024 * 1024).unwrap(),
                vec![],
            ),
            ProtocolLimits::new(8 * 1024 * 1024, 0),
        )
        .unwrap();
        socket
            .send(tungstenite::Message::Binary(bytes.into()))
            .unwrap();
        while socket.read().is_ok() {}
    });
    let values = BTreeMap::from([
        ("TELCHAR_TRANSFER_ENDPOINT", endpoint.as_str()),
        ("TELCHAR_NIX_STORE_URI", uri),
        ("TELCHAR_TRANSFER_CHUNK_BYTES", "262144"),
        ("TELCHAR_MAXIMUM_MANIFEST_BYTES", "8388608"),
        ("TELCHAR_TRANSFER_IDLE_TIMEOUT_SECONDS", "30"),
        ("TELCHAR_SETUP_TIMEOUT_SECONDS", "30"),
        ("TELCHAR_OUTPUT_COLLECTION_TIMEOUT_SECONDS", "30"),
        ("TELCHAR_MAXIMUM_CONNECTION_LIFETIME_SECONDS", "60"),
        ("TELCHAR_MAXIMUM_DIAGNOSTIC_BYTES", "65536"),
        ("TELCHAR_TRANSFER_AUTHENTICATION", "workload-identity"),
        ("TELCHAR_BACKEND", "nomad-primary"),
        ("TELCHAR_NAMESPACE", "telchar"),
        ("TELCHAR_JOB_ID", "job-1"),
        ("TELCHAR_SHARED_BUILD_DIGEST", "digest-1"),
        ("TELCHAR_TASK", "build"),
        ("NOMAD_NAMESPACE", "telchar"),
        ("NOMAD_JOB_ID", "job-1"),
        ("NOMAD_ALLOC_ID", "allocation-1"),
        ("NOMAD_TASK_NAME", "build"),
        ("NOMAD_TOKEN_telchar_transfer", "fixture-token"),
    ]);
    let config =
        WorkerConfig::from_lookup(|name| values.get(name).map(|value| value.to_string())).unwrap();
    let mut session = receive_manifest(&config).unwrap();
    session.resolve_inputs(uri).unwrap();
    session.build(uri).unwrap();
    drop(session);
    server.join().unwrap();
}

#[test]
fn assigned_builds_do_not_delegate_and_ordinary_connections_retain_builders() {
    let Some(root) = std::env::var_os("TELCHAR_EXECUTION_ROOT") else {
        let output = Command::new("bash")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../scripts/test-execution-store.sh"
            ))
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "assigned_builds_do_not_delegate_and_ordinary_connections_retain_builders",
                "--test-threads=1",
            ])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        return;
    };
    let root = PathBuf::from(root);
    let marker = root.join("remote-attempts");
    let hook = root.join("build-hook");
    fs::write(
        &hook,
        format!(
            r#"#!/usr/bin/env python3
import struct, sys
from pathlib import Path
stream = sys.stdin.buffer
def number():
    return struct.unpack('<Q', stream.read(8))[0]
def string():
    size = number()
    data = stream.read(size)
    stream.read((-size) % 8)
    return data.decode()
settings = {{}}
while number():
    key = string()
    settings[key] = string()
request = string()
assert request == 'try'
if settings['builders']:
    with Path({marker:?}).open('a') as output:
        output.write('attempt\n')
print('# decline-permanently', file=sys.stderr, flush=True)
"#,
            marker = marker.to_str().unwrap()
        ),
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o700)).unwrap();
    let config = format!(
        "{}\nmax-jobs = 1\nbuilders = ssh-ng://fixture x86_64-linux\nbuild-hook = {}\n",
        std::env::var("NIX_CONFIG").unwrap(),
        hook.display()
    );
    command(&["nix-store", "--store", "local", "--init"]);
    let log = root.join("daemon.log");
    let child = Command::new("nix-daemon")
        .env("NIX_CONFIG", &config)
        .stdout(Stdio::null())
        .stderr(fs::File::create(&log).unwrap())
        .spawn()
        .unwrap();
    let _daemon = Daemon(child, log);
    let socket = PathBuf::from(std::env::var_os("NIX_DAEMON_SOCKET_PATH").unwrap());
    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket.exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    let uri = format!("unix://{}", socket.display());
    let endpoint = GatewayStoreEndpoint::parse(&uri).unwrap();
    ordinary_build(&instantiate("ordinary-before"), &socket);
    assert_eq!(fs::read_to_string(&marker).unwrap(), "attempt\n");
    for backend in ["local", "ssh", "worker"] {
        let build = instantiate(backend);
        let output = PathBuf::from(String::from_utf8(build.outputs[0].path.clone()).unwrap());
        assert!(!output.exists());
        match backend {
            "local" => {
                let request = admit(&build, BackendKind::Local);
                GatewayStoreExecutor::new(endpoint.clone())
                    .execute(
                        &BuildExecution::new("local", &request, Duration::from_secs(10)).unwrap(),
                    )
                    .unwrap();
            }
            "ssh" => {
                let identity = root.join("identity");
                fs::write(&identity, "fixture identity").unwrap();
                fs::set_permissions(&identity, fs::Permissions::from_mode(0o600)).unwrap();
                let hosts = root.join("known-hosts");
                fs::write(&hosts, "fixture ssh-ed25519 AAAA\n").unwrap();
                let transport = root.join("ssh-transport");
                fs::write(
                    &transport,
                    format!(
                        "#!/bin/sh\nexport NIX_REMOTE=local\nexport NIX_CONFIG='{}'\nexec nix-daemon --stdio\n", config
                    ),
                )
                .unwrap();
                fs::set_permissions(&transport, fs::Permissions::from_mode(0o700)).unwrap();
                let path = root.join("telchar.toml");
                fs::write(&path, format!("[[backends.ssh]]\nssh_user = \"builder\"\nidentity_file = \"{}\"\nknown_hosts_file = \"{}\"\nssh_program = \"{}\"\n[backends.ssh.pool]\nsource = \"static\"\n[backends.ssh.pool.fixture]\naddress = \"fixture\"\n", identity.display(), hosts.display(), transport.display())).unwrap();
                unsafe {
                    std::env::set_var("TELCHAR_CONFIG", path);
                }
                let config = ServiceConfig::load().unwrap();
                let request = admit(&build, BackendKind::StaticSsh);
                let mut logs = Vec::new();
                let result = StaticSshBackend::new(
                    config.static_ssh_backends()[0].clone(),
                    endpoint.clone(),
                )
                .execute_with_logs(
                    &BuildExecution::new("ssh", &request, Duration::from_secs(10)).unwrap(),
                    &mut |chunk| {
                        logs.extend_from_slice(chunk);
                        Ok(())
                    },
                    &mut || Ok(false),
                );
                assert!(
                    result.is_ok(),
                    "{result:?}; {}",
                    String::from_utf8_lossy(&logs)
                );
            }
            "worker" => worker_build(build, &uri),
            _ => unreachable!(),
        }
        assert_eq!(fs::read(output).unwrap(), b"terminal");
        assert_eq!(
            fs::read_to_string(&marker).unwrap(),
            "attempt\n",
            "{backend} delegated"
        );
    }
    ordinary_build(&instantiate("ordinary-after"), &socket);
    assert_eq!(fs::read_to_string(&marker).unwrap(), "attempt\nattempt\n");
}
