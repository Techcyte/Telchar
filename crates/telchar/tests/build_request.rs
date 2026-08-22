//! Tests build request contracts and failure boundaries, including admits each system from the individual backend fleet.

use std::io;
use std::time::Duration;

use sha2::Digest;

use nix_worker_protocol::{
    write_worker_byte_string, write_worker_integer, ProtocolSessionLimits, WorkerReader,
};
use telchar::backend::{BackendKind, BackendTarget};
use telchar::build::BuildRequest;

#[test]
fn loads_classic_stored_derivation_into_build_request() {
    let derivation = br#"Derive([("out","/nix/store/11111111111111111111111111111111-telchar-gate-3-contract","","")],[],["/nix/store/22222222222222222222222222222222-input"],"x86_64-linux","/bin/sh",["-c","printf value > \"$out\""],[("builder","/bin/sh"),("name","telchar-gate-3-contract"),("out","/nix/store/11111111111111111111111111111111-telchar-gate-3-contract"),("system","x86_64-linux")])"#;
    let request = BuildRequest::from_stored_derivation(
        drv_path(),
        derivation,
        &backends("x86_64-linux", &[]),
    )
    .expect("stored derivation admits");

    assert_eq!(request.derivation_path(), drv_path());
    assert_eq!(
        request.expected_outputs(),
        &[(b"out".to_vec(), output_path().to_vec())]
    );
    assert_eq!(
        request.input_sources(),
        &[b"/nix/store/22222222222222222222222222222222-input".to_vec()]
    );
    assert_eq!(request.system(), "x86_64-linux");
    assert_eq!(request.builder(), b"/bin/sh");
}

#[test]
fn stored_derivation_inputs_include_direct_builder_dependencies() {
    let derivation = br#"Derive([("out","/nix/store/11111111111111111111111111111111-telchar-gate-3-contract","","")],[("/nix/store/33333333333333333333333333333333-dependency.drv",["out"])],["/nix/store/22222222222222222222222222222222-input"],"x86_64-linux","/nix/store/44444444444444444444444444444444-bash/bin/bash",[],[("builder","/nix/store/44444444444444444444444444444444-bash/bin/bash"),("name","telchar-gate-3-contract"),("out","/nix/store/11111111111111111111111111111111-telchar-gate-3-contract"),("system","x86_64-linux")])"#;
    let request = BuildRequest::from_stored_derivation(
        drv_path(),
        derivation,
        &backends("x86_64-linux", &[]),
    )
    .expect("stored derivation admits");

    assert_eq!(
        request.input_sources(),
        &[
            b"/nix/store/22222222222222222222222222222222-input".to_vec(),
            b"/nix/store/33333333333333333333333333333333-dependency.drv".to_vec(),
            b"/nix/store/44444444444444444444444444444444-bash".to_vec(),
        ]
    );
}

#[test]
fn loads_registered_derivation_through_store_export() {
    use std::path::{Path, PathBuf};
    use telchar::store::export::{StoreExportBackend, StoreExportRequest};
    use telchar::store::promotion::RegisteredPathInfo;

    struct Backend {
        nar: Vec<u8>,
        metadata: RegisteredPathInfo,
    }
    impl StoreExportBackend for Backend {
        fn store_uri(&self) -> &str {
            "fixture"
        }
        fn query_path_info(&mut self, _path: &Path) -> io::Result<RegisteredPathInfo> {
            Ok(self.metadata.clone())
        }
        fn export_nar(
            &mut self,
            _request: &StoreExportRequest,
            _nar_size: u64,
            sink: &mut dyn io::Write,
        ) -> io::Result<()> {
            sink.write_all(&self.nar)
        }
    }

    let contents = br#"Derive([("out","/nix/store/11111111111111111111111111111111-telchar-gate-3-contract","","")],[],[],"x86_64-linux","/bin/sh",[],[("builder","/bin/sh"),("name","telchar-gate-3-contract"),("out","/nix/store/11111111111111111111111111111111-telchar-gate-3-contract"),("system","x86_64-linux")])"#;
    let nar = regular_nar(contents);
    let mut backend = Backend {
        metadata: RegisteredPathInfo {
            path: PathBuf::from(std::str::from_utf8(drv_path()).unwrap()),
            nar_hash: sha2::Sha256::digest(&nar).into(),
            nar_size: nar.len() as u64,
            references: Vec::new(),
            deriver: None,
            content_address: None,
        },
        nar,
    };

    let request = BuildRequest::load_stored(
        Path::new(std::str::from_utf8(drv_path()).unwrap()),
        &mut backend,
        &backends("x86_64-linux", &[]),
    )
    .expect("registered derivation loads");
    assert_eq!(request.derivation_path(), drv_path());
}

#[test]
fn loads_selected_input_derivation_outputs() {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use telchar::store::export::{StoreExportBackend, StoreExportRequest};
    use telchar::store::promotion::RegisteredPathInfo;

    struct Backend {
        nars: BTreeMap<PathBuf, Vec<u8>>,
    }
    impl StoreExportBackend for Backend {
        fn store_uri(&self) -> &str {
            "fixture"
        }
        fn query_path_info(&mut self, path: &Path) -> io::Result<RegisteredPathInfo> {
            let nar = self
                .nars
                .get(path)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing fixture path"))?;
            Ok(RegisteredPathInfo {
                path: path.to_path_buf(),
                nar_hash: sha2::Sha256::digest(nar).into(),
                nar_size: nar.len() as u64,
                references: Vec::new(),
                deriver: None,
                content_address: None,
            })
        }
        fn export_nar(
            &mut self,
            request: &StoreExportRequest,
            _nar_size: u64,
            sink: &mut dyn io::Write,
        ) -> io::Result<()> {
            sink.write_all(
                self.nars.get(&request.path).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "missing fixture NAR")
                })?,
            )
        }
    }

    let dependency_path =
        PathBuf::from("/nix/store/33333333333333333333333333333333-dependency.drv");
    let dependency_output = b"/nix/store/55555555555555555555555555555555-dependency";
    let root = br#"Derive([("out","/nix/store/11111111111111111111111111111111-telchar-gate-3-contract","","")],[("/nix/store/33333333333333333333333333333333-dependency.drv",["out"])],[],"x86_64-linux","/bin/sh",[],[("builder","/bin/sh"),("name","telchar-gate-3-contract"),("out","/nix/store/11111111111111111111111111111111-telchar-gate-3-contract"),("system","x86_64-linux")])"#;
    let dependency = br#"Derive([("out","/nix/store/55555555555555555555555555555555-dependency","","")],[],[],"x86_64-linux","/bin/sh",[],[("builder","/bin/sh"),("name","dependency"),("out","/nix/store/55555555555555555555555555555555-dependency"),("system","x86_64-linux")])"#;
    let root_path = PathBuf::from(std::str::from_utf8(drv_path()).unwrap());
    let mut backend = Backend {
        nars: BTreeMap::from([
            (root_path.clone(), regular_nar(root)),
            (dependency_path, regular_nar(dependency)),
        ]),
    };

    let request =
        BuildRequest::load_stored(&root_path, &mut backend, &backends("x86_64-linux", &[]))
            .expect("stored derivation and selected dependency output load");

    assert!(request
        .input_sources()
        .iter()
        .any(|path| path.as_slice() == dependency_output));
}

#[test]
fn rejects_malformed_or_dynamic_stored_derivations() {
    for derivation in [
        b"Derive([)".as_slice(),
        br#"DrvWithVersion("xp-dyn-drv",Derive([],[],[],"x86_64-linux","/bin/sh",[],[]))"#,
    ] {
        assert!(BuildRequest::from_stored_derivation(
            drv_path(),
            derivation,
            &backends("x86_64-linux", &[]),
        )
        .is_err());
    }
}

#[test]
fn admits_each_system_from_the_individual_backend_fleet() {
    let backends = [
        BackendTarget::new("ssh-amd64", BackendKind::StaticSsh, "x86_64-linux", ["kvm"])
            .expect("amd64 backend parses"),
        BackendTarget::new(
            "ssh-arm64",
            BackendKind::StaticSsh,
            "aarch64-linux",
            ["big-parallel"],
        )
        .expect("arm64 backend parses"),
    ];

    let amd64 = BuildRequest::from_worker_request(
        &decode_request_with_system_and_features("x86_64-linux", "kvm"),
        &backends,
    )
    .expect("amd64 request admits");
    let arm64 = BuildRequest::from_worker_request(
        &decode_request_with_system_and_features("aarch64-linux", "big-parallel"),
        &backends,
    )
    .expect("arm64 request admits");

    assert_eq!(amd64.system(), "x86_64-linux");
    assert_eq!(arm64.system(), "aarch64-linux");
}

#[test]
fn rejects_features_that_exist_only_as_a_cross_backend_union() {
    let backends = [
        BackendTarget::new("first", BackendKind::StaticSsh, "x86_64-linux", ["kvm"])
            .expect("first backend parses"),
        BackendTarget::new(
            "second",
            BackendKind::Nomad,
            "x86_64-linux",
            ["big-parallel"],
        )
        .expect("second backend parses"),
    ];
    let worker = decode_request_with_system_and_features("x86_64-linux", "kvm big-parallel");

    assert_eq!(
        BuildRequest::from_worker_request(&worker, &backends)
            .expect_err("cross-backend feature union rejects")
            .kind(),
        io::ErrorKind::InvalidInput
    );
}

#[test]
fn normalizes_gate_3_request_without_backend_objects() {
    let worker = decode_gate_3_request("x86_64-linux", 0);
    let backends = backends("x86_64-linux", &[]);

    let request =
        BuildRequest::from_worker_request(&worker, &backends).expect("Gate 3 request is admitted");

    assert!(request
        .derivation_path()
        .ends_with(b"-telchar-gate-3-contract.drv"));
    assert_eq!(request.expected_outputs().len(), 1);
    assert_eq!(request.expected_outputs()[0].0, b"out");
    assert_eq!(request.system(), "x86_64-linux");
    assert_eq!(request.builder(), b"/bin/sh");
    assert_eq!(request.arguments().len(), 2);
    assert_eq!(request.environment().len(), 4);
    assert!(request.required_system_features().is_empty());
    assert!(request.input_sources().is_empty());
}

#[test]
fn preserves_bounded_required_system_features() {
    let worker = decode_request_with_features("kvm big-parallel");
    let backends = backends("x86_64-linux", &["kvm", "big-parallel"]);

    let request = BuildRequest::from_worker_request(&worker, &backends)
        .expect("required features are admitted");

    assert_eq!(request.required_system_features(), ["big-parallel", "kvm"]);
}

#[test]
fn rejects_unsupported_or_malformed_required_system_features() {
    let backends = backends("x86_64-linux", &["kvm"]);

    for features in ["benchmark", "kvm kvm", "kvm feature/unsafe"] {
        let worker = decode_request_with_features(features);
        assert_eq!(
            BuildRequest::from_worker_request(&worker, &backends)
                .expect_err("invalid required features must fail")
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
}

#[test]
fn preserves_fixed_output_authority_in_shared_build_identity() {
    let backends = backends("x86_64-linux", &[]);
    let first = BuildRequest::from_worker_request(
        &decode_request_with_hash(
            b"sha256",
            b"0000000000000000000000000000000000000000000000000000000000000000",
        ),
        &backends,
    )
    .expect("flat fixed-output request admits");
    let second = BuildRequest::from_worker_request(
        &decode_request_with_hash(
            b"r:sha256",
            b"0000000000000000000000000000000000000000000000000000000000000000",
        ),
        &backends,
    )
    .expect("recursive fixed-output request admits");

    assert_eq!(first.output_authorities().len(), 1);
    assert_eq!(first.output_authorities()[0].hash_algorithm(), b"sha256");
    assert_eq!(
        first.output_authorities()[0]
            .expected_content_address()
            .as_deref(),
        Some("fixed:sha256:0000000000000000000000000000000000000000000000000000")
    );
    assert_eq!(
        second.output_authorities()[0]
            .expected_content_address()
            .as_deref(),
        Some("fixed:r:sha256:0000000000000000000000000000000000000000000000000000")
    );
    assert_ne!(first.shared_build_key(), second.shared_build_key());
}

#[test]
fn equivalent_requests_have_the_same_shared_build_key() {
    let backends = backends("x86_64-linux", &[]);
    let first =
        BuildRequest::from_worker_request(&decode_gate_3_request("x86_64-linux", 0), &backends)
            .expect("first request admits");
    let second =
        BuildRequest::from_worker_request(&decode_gate_3_request("x86_64-linux", 0), &backends)
            .expect("second request admits");

    assert_eq!(first.shared_build_key(), second.shared_build_key());
    assert_eq!(first.shared_build_key().len(), drv_path().len() + 1 + 64);
}

#[test]
fn admitted_semantic_difference_changes_shared_build_key() {
    let backends = backends("x86_64-linux", &[]);
    let first =
        BuildRequest::from_worker_request(&decode_gate_3_request("x86_64-linux", 0), &backends)
            .expect("first request admits");
    let second = BuildRequest::from_worker_request(
        &decode_request_with_command("printf different > $out"),
        &backends,
    )
    .expect("second request admits");

    assert_ne!(first.shared_build_key(), second.shared_build_key());
}

#[test]
fn rejects_system_mismatch_before_execution() {
    let worker = decode_gate_3_request("aarch64-linux", 0);
    let backends = backends("x86_64-linux", &[]);

    let error = BuildRequest::from_worker_request(&worker, &backends)
        .expect_err("mismatched system must fail admission");

    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn rejects_output_environment_mismatch() {
    let worker = decode_request(
        "x86_64-linux",
        b"/nix/store/22222222222222222222222222222222-different-output",
        0,
    );
    let backends = backends("x86_64-linux", &[]);

    assert_eq!(
        BuildRequest::from_worker_request(&worker, &backends)
            .expect_err("output environment mismatch must fail")
            .kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn rejects_non_normal_build_modes() {
    for mode in [1, 2] {
        let wire = build_request_wire("x86_64-linux", output_path(), mode);
        let mut reader = WorkerReader::new(
            wire.as_slice(),
            ProtocolSessionLimits::new(16 * 1024 * 1024, Duration::from_secs(1)),
        );
        assert_eq!(
            reader
                .read_operation()
                .expect("BuildDerivation operation reads"),
            nix_worker_protocol::WorkerOperation::BuildDerivation
        );
        let error = reader
            .complete_build_derivation()
            .expect_err("non-normal build mode must fail before admission");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}

fn decode_gate_3_request(system: &str, mode: u64) -> nix_worker_protocol::BuildDerivationRequest {
    decode_request(system, output_path(), mode)
}

fn backends(system: &str, features: &[&str]) -> Vec<BackendTarget> {
    vec![
        BackendTarget::new("fixture", BackendKind::Local, system, features)
            .expect("backend parses"),
    ]
}

fn decode_request_with_system_and_features(
    system: &str,
    features: &str,
) -> nix_worker_protocol::BuildDerivationRequest {
    let wire = build_request_wire_with_environment(
        system,
        output_path(),
        0,
        "printf telchar-remote-build > $out",
        Some(features),
    );
    decode_wire(wire)
}

fn decode_request(
    system: &str,
    environment_output: &[u8],
    mode: u64,
) -> nix_worker_protocol::BuildDerivationRequest {
    let wire = build_request_wire(system, environment_output, mode);
    let mut reader = WorkerReader::new(
        wire.as_slice(),
        ProtocolSessionLimits::new(16 * 1024 * 1024, Duration::from_secs(1)),
    );
    let operation = reader.read_operation().expect("operation reads");
    assert_eq!(
        operation,
        nix_worker_protocol::WorkerOperation::BuildDerivation
    );
    reader
        .complete_build_derivation()
        .expect("worker request decodes for admission test")
}

fn decode_request_with_hash(
    hash_algorithm: &[u8],
    hash: &[u8],
) -> nix_worker_protocol::BuildDerivationRequest {
    decode_wire(build_request_wire_with_output_authority(
        "x86_64-linux",
        output_path(),
        0,
        "printf telchar-remote-build > $out",
        None,
        hash_algorithm,
        hash,
    ))
}

fn decode_request_with_features(features: &str) -> nix_worker_protocol::BuildDerivationRequest {
    let wire = build_request_wire_with_environment(
        "x86_64-linux",
        output_path(),
        0,
        "printf telchar-remote-build > $out",
        Some(features),
    );
    decode_wire(wire)
}

fn decode_request_with_command(command: &str) -> nix_worker_protocol::BuildDerivationRequest {
    let wire = build_request_wire_with_command("x86_64-linux", output_path(), 0, command);
    decode_wire(wire)
}

fn decode_wire(wire: Vec<u8>) -> nix_worker_protocol::BuildDerivationRequest {
    let mut reader = WorkerReader::new(
        wire.as_slice(),
        ProtocolSessionLimits::new(16 * 1024 * 1024, Duration::from_secs(1)),
    );
    assert_eq!(
        reader.read_operation().expect("operation reads"),
        nix_worker_protocol::WorkerOperation::BuildDerivation
    );
    reader
        .complete_build_derivation()
        .expect("worker request decodes")
}

fn build_request_wire(system: &str, environment_output: &[u8], mode: u64) -> Vec<u8> {
    build_request_wire_with_command(
        system,
        environment_output,
        mode,
        "printf telchar-remote-build > $out",
    )
}

fn build_request_wire_with_command(
    system: &str,
    environment_output: &[u8],
    mode: u64,
    command: &str,
) -> Vec<u8> {
    build_request_wire_with_environment(system, environment_output, mode, command, None)
}

fn build_request_wire_with_environment(
    system: &str,
    environment_output: &[u8],
    mode: u64,
    command: &str,
    required_system_features: Option<&str>,
) -> Vec<u8> {
    build_request_wire_with_output_authority(
        system,
        environment_output,
        mode,
        command,
        required_system_features,
        b"",
        b"",
    )
}

fn build_request_wire_with_output_authority(
    system: &str,
    environment_output: &[u8],
    mode: u64,
    command: &str,
    required_system_features: Option<&str>,
    hash_algorithm: &[u8],
    hash: &[u8],
) -> Vec<u8> {
    let mut wire = Vec::new();
    write_worker_integer(&mut wire, 36);
    write_worker_byte_string(&mut wire, drv_path());
    write_worker_integer(&mut wire, 1);
    write_worker_byte_string(&mut wire, b"out");
    write_worker_byte_string(&mut wire, output_path());
    write_worker_byte_string(&mut wire, hash_algorithm);
    write_worker_byte_string(&mut wire, hash);
    write_worker_integer(&mut wire, 0);
    write_worker_byte_string(&mut wire, system.as_bytes());
    write_worker_byte_string(&mut wire, b"/bin/sh");
    write_worker_integer(&mut wire, 2);
    write_worker_byte_string(&mut wire, b"-c");
    write_worker_byte_string(&mut wire, command.as_bytes());
    write_worker_integer(&mut wire, 4 + u64::from(required_system_features.is_some()));
    for (key, value) in [
        (b"builder".as_slice(), b"/bin/sh".as_slice()),
        (b"name".as_slice(), b"telchar-gate-3-contract".as_slice()),
        (b"out".as_slice(), environment_output),
        (b"system".as_slice(), system.as_bytes()),
    ] {
        write_worker_byte_string(&mut wire, key);
        write_worker_byte_string(&mut wire, value);
    }
    if let Some(features) = required_system_features {
        write_worker_byte_string(&mut wire, b"requiredSystemFeatures");
        write_worker_byte_string(&mut wire, features.as_bytes());
    }
    write_worker_integer(&mut wire, mode);
    wire
}

fn drv_path() -> &'static [u8] {
    b"/nix/store/00000000000000000000000000000000-telchar-gate-3-contract.drv"
}

fn regular_nar(contents: &[u8]) -> Vec<u8> {
    let mut nar = Vec::new();
    for value in [
        b"nix-archive-1".as_slice(),
        b"(".as_slice(),
        b"type".as_slice(),
        b"regular".as_slice(),
        b"contents".as_slice(),
        contents,
        b")".as_slice(),
    ] {
        write_worker_byte_string(&mut nar, value);
    }
    nar
}

fn output_path() -> &'static [u8] {
    b"/nix/store/11111111111111111111111111111111-telchar-gate-3-contract"
}
