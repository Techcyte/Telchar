//! Compares executor result metadata with an independent real Nix daemon.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use nix_worker_protocol::{
    BuildDerivationClientRequest, BuildDerivationOutputRequest, ProtocolSessionLimits,
    WorkerClient, WorkerOptions, WorkerReader,
};
use telchar::backend::{BackendKind, BackendTarget};
use telchar::build::BuildRequest;

#[test]
fn execution_output_realizations_match_real_nix() {
    let socket = std::env::var("NIX_DAEMON_SOCKET_PATH").expect("isolated canonical store socket");
    for (name, algorithm, hash, environment) in [
        ("telchar-realization-input", "", "", vec![]),
        (
            "telchar-realization-structured",
            "",
            "",
            vec![(
                "__json",
                "{ \"z\": [1, true, 1e-7, 1e-5, 1e15, 1e20, 1e23, -0.0], \"a\": \"quote\\\"\" }",
            )],
        ),
        (
            "telchar-realization-fixed",
            "sha256",
            "2d711642b726b04401627ca9fbac32f5c8530fb1903cc4db02258717921a4881",
            vec![],
        ),
    ] {
        let authority = if algorithm.is_empty() {
            String::new()
        } else {
            format!(
                "outputHashAlgo = \"sha256\"; outputHashMode = \"flat\"; outputHash = \"{hash}\";"
            )
        };
        let expression = format!(
            "derivation {{ name = \"{name}\"; system = builtins.currentSystem; builder = \"/bin/sh\"; args = [ \"-c\" \"printf x > $out\" ]; {authority} }}"
        );
        let result = std::process::Command::new("nix-instantiate")
            .args(["--expr", &expression])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let drv = String::from_utf8(result.stdout).unwrap();
        let drv = drv.trim();
        let result = std::process::Command::new("nix-store")
            .args(["-q", "--outputs", drv])
            .output()
            .unwrap();
        assert!(result.status.success());
        let output = String::from_utf8(result.stdout).unwrap();
        let output = output.trim();
        let system = std::process::Command::new("nix-instantiate")
            .args(["--eval", "--expr", "builtins.currentSystem"])
            .output()
            .unwrap();
        let system = String::from_utf8(system.stdout).unwrap();
        let system = system.trim().trim_matches('"');
        let mut env = vec![
            (b"name".to_vec(), name.as_bytes().to_vec()),
            (b"out".to_vec(), output.as_bytes().to_vec()),
        ];
        env.extend(
            environment
                .iter()
                .map(|(key, value)| (key.as_bytes().to_vec(), value.as_bytes().to_vec())),
        );
        let arguments = vec![b"-c".to_vec(), format!("printf x > {output}").into_bytes()];
        let outputs = [BuildDerivationOutputRequest {
            name: b"out",
            path: output.as_bytes(),
            hash_algorithm: algorithm.as_bytes(),
            hash: hash.as_bytes(),
        }];
        let request = BuildDerivationClientRequest {
            drv_path: drv.as_bytes(),
            outputs: &outputs,
            input_sources: &[],
            platform: system.as_bytes(),
            builder: b"/bin/sh",
            arguments: &arguments,
            environment: &env,
        };
        let stream = UnixStream::connect(&socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        let mut client = WorkerClient::connect(stream).unwrap();
        client
            .set_options(&WorkerOptions {
                overrides: vec![(b"builders".to_vec(), vec![])],
                ..WorkerOptions::default()
            })
            .unwrap();
        // Capture the real response independently of Telchar's success writer.
        let mut stream = client.into_inner();
        let wire = request_wire(&request);
        stream.write_all(&wire).unwrap();
        stream.flush().unwrap();
        let mut reader = WorkerReader::new(&wire[8..], ProtocolSessionLimits::DEFAULT);
        let decoded = reader.complete_build_derivation().unwrap();
        let admitted = BuildRequest::from_worker_request(
            &decoded,
            &[BackendTarget::new("local", BackendKind::Local, system, [] as [&str; 0]).unwrap()],
        )
        .unwrap();
        loop {
            match integer(&mut stream) {
                nix_worker_protocol::STDERR_LAST => break,
                nix_worker_protocol::STDERR_NEXT => {
                    string(&mut stream);
                }
                nix_worker_protocol::STDERR_START_ACTIVITY => {
                    for _ in 0..3 {
                        integer(&mut stream);
                    }
                    string(&mut stream);
                    fields(&mut stream);
                    integer(&mut stream);
                }
                nix_worker_protocol::STDERR_STOP_ACTIVITY => {
                    integer(&mut stream);
                }
                nix_worker_protocol::STDERR_RESULT => {
                    integer(&mut stream);
                    integer(&mut stream);
                    fields(&mut stream);
                }
                nix_worker_protocol::STDERR_ERROR => panic!("real Nix rejected {name}"),
                word => panic!("unexpected real Nix frame {word:x}"),
            }
        }
        let status = integer(&mut stream);
        let diagnostic = string(&mut stream);
        assert!(
            matches!(status, 0 | 2),
            "{name}: status {status}: {}",
            String::from_utf8_lossy(&diagnostic)
        );
        assert_eq!(diagnostic, b"");
        for _ in 0..4 {
            integer(&mut stream);
        }
        for _ in 0..2 {
            if integer(&mut stream) == 1 {
                integer(&mut stream);
            }
        }
        assert_eq!(integer(&mut stream), 1);
        let id = string(&mut stream);
        let real: serde_json::Value = serde_json::from_slice(&string(&mut stream)).unwrap();
        let mut response = Vec::new();
        nix_worker_protocol::write_build_derivation_success_response(
            &mut response,
            nix_worker_protocol::LATEST_WORKER_VERSION,
            false,
            &admitted.output_realizations().unwrap(),
        )
        .unwrap();
        let mut response = response.as_slice();
        integer(&mut response);
        integer(&mut response);
        string(&mut response);
        for _ in 0..6 {
            integer(&mut response);
        }
        assert_eq!(integer(&mut response), 1);
        assert_eq!(string(&mut response), id, "{name}: realization ID");
        let actual: serde_json::Value = serde_json::from_slice(&string(&mut response)).unwrap();
        assert_eq!(actual, real, "{name}: realization value");
        assert!(Path::new(output).exists());
    }
}

fn fields(input: &mut impl Read) {
    for _ in 0..integer(input) {
        match integer(input) {
            0 => {
                integer(input);
            }
            1 => {
                string(input);
            }
            kind => panic!("unexpected activity field {kind}"),
        }
    }
}

fn request_wire(request: &BuildDerivationClientRequest<'_>) -> Vec<u8> {
    let mut wire = Vec::new();
    word(&mut wire, 36);
    bytes(&mut wire, request.drv_path);
    word(&mut wire, request.outputs.len() as u64);
    for output in request.outputs {
        for value in [output.name, output.path, output.hash_algorithm, output.hash] {
            bytes(&mut wire, value);
        }
    }
    word(&mut wire, request.input_sources.len() as u64);
    for source in request.input_sources {
        bytes(&mut wire, source);
    }
    bytes(&mut wire, request.platform);
    bytes(&mut wire, request.builder);
    word(&mut wire, request.arguments.len() as u64);
    for argument in request.arguments {
        bytes(&mut wire, argument);
    }
    word(&mut wire, request.environment.len() as u64);
    for (key, value) in request.environment {
        bytes(&mut wire, key);
        bytes(&mut wire, value);
    }
    word(&mut wire, 0);
    wire
}

fn word(wire: &mut Vec<u8>, value: u64) {
    wire.extend_from_slice(&value.to_le_bytes());
}
fn bytes(wire: &mut Vec<u8>, value: &[u8]) {
    word(wire, value.len() as u64);
    wire.extend_from_slice(value);
    wire.resize(wire.len() + (8 - value.len() % 8) % 8, 0);
}
fn integer(input: &mut impl Read) -> u64 {
    let mut word = [0; 8];
    input.read_exact(&mut word).unwrap();
    u64::from_le_bytes(word)
}
fn string(input: &mut impl Read) -> Vec<u8> {
    let length = integer(input) as usize;
    let mut value = vec![0; length];
    input.read_exact(&mut value).unwrap();
    let mut padding = [0; 7];
    input
        .read_exact(&mut padding[..(8 - length % 8) % 8])
        .unwrap();
    value
}
