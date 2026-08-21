//! Tests store export contracts and failure boundaries, including real store streams raw nar with registered hash and size.

use std::io::{self, Write};

use sha2::Digest;
use std::path::{Path, PathBuf};

use telchar::fixture::nix::{NixFixture, TrustMode};
use telchar::service::transfer_limits::{TransferBudget, TransferLimits};
use telchar::store::export::{
    export_verified_nar, export_verified_nar_with_limits, load_stored_derivation,
    validate_store_output, GatewayStoreExportBackend, StoreExportBackend, StoreExportRequest,
    VerifiedStoreExport,
};
use telchar::store::promotion::RegisteredPathInfo;

const CONTENT: &[u8] = b"telchar-classic-fixture";
const PATH: &str = "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-telchar-fixture";
const NAR_HASH: [u8; 32] = [
    0x6c, 0x2b, 0xe2, 0xf1, 0x2a, 0x16, 0x86, 0x05, 0xeb, 0xcb, 0xa3, 0x78, 0x22, 0x86, 0xc3, 0x8e,
    0xaf, 0x3f, 0x5d, 0x78, 0x7b, 0x7a, 0x8b, 0xb2, 0x4a, 0x54, 0x0d, 0x22, 0x67, 0xff, 0x68, 0xe1,
];

#[test]
#[ignore = "private fixture paths are outside the production /nix/store namespace"]
fn real_store_streams_raw_nar_with_registered_hash_and_size() {
    let fixture = NixFixture::create().expect("fixture creates");
    let mut daemon = fixture
        .start_daemon(TrustMode::Trusted)
        .expect("daemon starts");
    let path = daemon
        .build_classic_derivation()
        .expect("fixture path builds");
    let expected = daemon
        .query_path_info(&path)
        .expect("registered metadata queries");
    let mut backend: GatewayStoreExportBackend = daemon
        .export_backend()
        .expect("gateway export backend creates");
    let mut output = Vec::new();

    let verified = export_verified_nar(&path, &mut output, &mut backend)
        .expect("real raw NAR export verifies");

    assert!(output.starts_with(&13_u64.to_le_bytes()));
    assert_eq!(verified.metadata.path, path);
    assert_eq!(verified.metadata.nar_hash, sri_sha256(&expected.nar_hash));
    assert_eq!(verified.metadata.nar_size, expected.nar_size);
    assert_eq!(verified.nar_hash, verified.metadata.nar_hash);
    assert_eq!(verified.nar_size, verified.metadata.nar_size);

    daemon.stop().expect("daemon stops");
    fixture.cleanup().expect("fixture cleans");
}

#[test]
#[ignore = "private fixture paths are outside the production /nix/store namespace"]
fn real_store_validates_registered_output_metadata() {
    let fixture = NixFixture::create().expect("fixture creates");
    let mut daemon = fixture
        .start_daemon(TrustMode::Trusted)
        .expect("daemon starts");
    let path = daemon
        .build_classic_derivation()
        .expect("fixture path builds");
    let expected = daemon
        .query_path_info(&path)
        .expect("registered metadata queries");
    let mut backend: GatewayStoreExportBackend = daemon
        .export_backend()
        .expect("gateway export backend creates");

    let registered = validate_store_output(&path, &mut backend)
        .expect("real raw NAR validates against registered metadata");

    assert_eq!(registered.path, path);
    assert_eq!(registered.nar_hash, sri_sha256(&expected.nar_hash));
    assert_eq!(registered.nar_size, expected.nar_size);

    daemon.stop().expect("daemon stops");
    fixture.cleanup().expect("fixture cleans");
}

#[test]
fn loads_one_bounded_registered_derivation() {
    let contents = b"Derive([],[],[],\"x86_64-linux\",\"/bin/sh\",[],[])";
    let metadata = RegisteredPathInfo {
        path: PathBuf::from("/nix/store/00000000000000000000000000000000-contract.drv"),
        nar_hash: sha2::Sha256::digest(regular_nar(contents)).into(),
        nar_size: regular_nar(contents).len() as u64,
        references: Vec::new(),
        deriver: None,
        content_address: None,
    };
    let mut backend = RecordingExportBackend::successful(metadata, regular_nar(contents));

    assert_eq!(
        load_stored_derivation(
            Path::new("/nix/store/00000000000000000000000000000000-contract.drv"),
            4096,
            &mut backend,
        )
        .unwrap(),
        contents
    );
}

#[test]
fn rejects_stored_derivation_hash_mismatch() {
    let contents = b"Derive([],[],[],\"x86_64-linux\",\"/bin/sh\",[],[])";
    let nar = regular_nar(contents);
    let metadata = RegisteredPathInfo {
        path: PathBuf::from("/nix/store/00000000000000000000000000000000-contract.drv"),
        nar_hash: [0; 32],
        nar_size: nar.len() as u64,
        references: Vec::new(),
        deriver: None,
        content_address: None,
    };
    let mut backend = RecordingExportBackend::successful(metadata, nar);

    assert!(load_stored_derivation(
        Path::new("/nix/store/00000000000000000000000000000000-contract.drv"),
        4096,
        &mut backend,
    )
    .is_err());
}

#[test]
fn validates_exact_output_path_against_registered_metadata() {
    let nar = regular_nar(CONTENT);
    let metadata = registered_path_info();
    let mut backend = RecordingExportBackend::successful(metadata.clone(), nar);

    let registered = validate_store_output(Path::new(PATH), &mut backend)
        .expect("matching registered output validates");

    assert_eq!(registered, metadata);
    assert_eq!(backend.queries, vec![PathBuf::from(PATH)]);
    assert_eq!(
        backend.requests,
        vec![StoreExportRequest {
            version: 1,
            store_uri: "unix:///run/nix-daemon.sock".into(),
            path: PATH.into(),
        }]
    );
}

#[test]
fn rejects_output_with_registered_hash_mismatch() {
    let mut metadata = registered_path_info();
    metadata.nar_hash[0] ^= 0xff;
    let mut backend = RecordingExportBackend::successful(metadata, regular_nar(CONTENT));

    let error = validate_store_output(Path::new(PATH), &mut backend)
        .expect_err("mismatched registered hash must fail");

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(error.to_string(), "exported NAR hash mismatch");
}

#[test]
fn rejects_output_with_registered_size_mismatch() {
    let mut metadata = registered_path_info();
    metadata.nar_size += 1;
    let mut backend = RecordingExportBackend::successful(metadata, regular_nar(CONTENT));

    let error = validate_store_output(Path::new(PATH), &mut backend)
        .expect_err("mismatched registered size must fail");

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(error.to_string(), "exported NAR size mismatch");
}

#[test]
fn rejects_invalid_serializations_while_validating_output() {
    let mut truncated = regular_nar(CONTENT);
    truncated.pop();
    let mut trailing = regular_nar(CONTENT);
    trailing.push(0xff);
    let cases = [
        ("malformed", b"not a NAR".to_vec()),
        ("truncated", truncated),
        ("trailing", trailing),
    ];

    for (label, nar) in cases {
        let mut backend = RecordingExportBackend::successful(registered_path_info(), nar);
        let error = validate_store_output(Path::new(PATH), &mut backend)
            .expect_err("invalid exported NAR must fail validation");

        match label {
            "malformed" => assert!(error.to_string().contains("NAR"), "{error}"),
            "truncated" => assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof),
            "trailing" => assert_eq!(error.to_string(), "trailing bytes after NAR"),
            _ => unreachable!(),
        }
    }
}

#[test]
fn rejects_backend_export_failure_while_validating_output() {
    let mut backend = FailingExportBackend;

    let error = validate_store_output(Path::new(PATH), &mut backend)
        .expect_err("backend export failure must fail validation");

    assert_eq!(error.to_string(), "backend export failed");
}

#[test]
fn rejects_backend_panic_while_validating_output() {
    let mut backend = PanickingExportBackend;

    let error = validate_store_output(Path::new(PATH), &mut backend)
        .expect_err("backend panic must fail validation");

    assert_eq!(error.to_string(), "export backend thread panicked");
}

#[test]
fn streams_raw_nar_and_verifies_registered_metadata() {
    let nar = regular_nar(CONTENT);
    let metadata = registered_path_info();
    let mut backend = RecordingExportBackend::successful(metadata.clone(), nar.clone());
    let mut output = Vec::new();

    let verified = export_verified_nar(Path::new(PATH), &mut output, &mut backend)
        .expect("matching export verifies");

    assert_eq!(output, nar);
    assert_eq!(
        verified,
        VerifiedStoreExport {
            metadata,
            nar_hash: NAR_HASH,
            nar_size: 136,
        }
    );
    assert_eq!(backend.queries, vec![PathBuf::from(PATH)]);
    assert_eq!(
        backend.requests,
        vec![StoreExportRequest {
            version: 1,
            store_uri: "unix:///run/nix-daemon.sock".into(),
            path: PATH.into(),
        }]
    );
}

#[test]
fn object_limit_stops_export_producer_at_exact_boundary() {
    let nar = regular_nar(CONTENT);
    let mut backend = RecordingExportBackend::successful(registered_path_info(), nar);
    let limits = TransferLimits {
        maximum_object_bytes: 135,
        maximum_inbound_session_bytes: 1,
        maximum_outbound_session_bytes: 1_000,
        maximum_inbound_session_objects: 256,
        maximum_outbound_session_objects: 256,
        maximum_active_inbound_objects: 256,
        maximum_active_outbound_objects: 256,
        ..TransferLimits::default()
    };
    let mut session = TransferBudget::new(limits.maximum_outbound_session_bytes);
    let mut output = Vec::new();

    let error = export_verified_nar_with_limits(
        Path::new(PATH),
        &mut output,
        &mut backend,
        &limits,
        &mut session,
    )
    .expect_err("export above object limit must fail");

    assert_eq!(error.to_string(), "NAR object byte limit exceeded");
    assert_eq!(output.len(), 135, "excess byte reached worker boundary");
    assert_eq!(session.charged(), 135);
    assert!(backend.writer_failed, "export producer did not stop");
}

#[test]
fn session_limit_is_shared_across_verified_exports() {
    let nar = regular_nar(CONTENT);
    let limits = TransferLimits {
        maximum_object_bytes: 200,
        maximum_inbound_session_bytes: 1,
        maximum_outbound_session_bytes: 200,
        maximum_inbound_session_objects: 256,
        maximum_outbound_session_objects: 256,
        maximum_active_inbound_objects: 256,
        maximum_active_outbound_objects: 256,
        ..TransferLimits::default()
    };
    let mut session = TransferBudget::new(limits.maximum_outbound_session_bytes);
    let mut first_backend = RecordingExportBackend::successful(registered_path_info(), nar.clone());
    let mut first_output = Vec::new();

    export_verified_nar_with_limits(
        Path::new(PATH),
        &mut first_output,
        &mut first_backend,
        &limits,
        &mut session,
    )
    .expect("first export fits session limit");

    let mut second_backend = RecordingExportBackend::successful(registered_path_info(), nar);
    let mut second_output = Vec::new();
    let error = export_verified_nar_with_limits(
        Path::new(PATH),
        &mut second_output,
        &mut second_backend,
        &limits,
        &mut session,
    )
    .expect_err("second export exceeds remaining session limit");

    assert_eq!(error.to_string(), "transfer session byte limit exceeded");
    assert_eq!(first_output.len(), 136);
    assert_eq!(
        second_output.len(),
        64,
        "excess byte reached worker boundary"
    );
    assert_eq!(session.charged(), 200);
    assert!(second_backend.writer_failed, "export producer did not stop");
}

#[test]
fn rejects_export_hash_mismatch_after_streaming() {
    let mut metadata = registered_path_info();
    metadata.nar_hash[0] ^= 0xff;
    let mut backend = RecordingExportBackend::successful(metadata, regular_nar(CONTENT));
    let mut output = Vec::new();

    let error = export_verified_nar(Path::new(PATH), &mut output, &mut backend)
        .expect_err("hash mismatch must fail");

    assert!(error.to_string().contains("exported NAR hash mismatch"));
    assert!(
        !output.is_empty(),
        "hostile bytes did not reach streaming boundary"
    );
}

#[test]
fn rejects_export_size_mismatch_after_streaming() {
    let mut metadata = registered_path_info();
    metadata.nar_size += 1;
    let mut backend = RecordingExportBackend::successful(metadata, regular_nar(CONTENT));
    let mut output = Vec::new();

    let error = export_verified_nar(Path::new(PATH), &mut output, &mut backend)
        .expect_err("size mismatch must fail");

    assert!(error.to_string().contains("exported NAR size mismatch"));
    assert!(
        !output.is_empty(),
        "hostile bytes did not reach streaming boundary"
    );
}

#[test]
fn rejects_truncated_raw_nar_from_backend() {
    let mut nar = regular_nar(CONTENT);
    nar.pop();
    let mut backend = RecordingExportBackend::successful(registered_path_info(), nar);
    let mut output = Vec::new();

    let error = export_verified_nar(Path::new(PATH), &mut output, &mut backend)
        .expect_err("truncated NAR must fail");

    assert!(
        error.kind() == io::ErrorKind::UnexpectedEof || error.to_string().contains("truncated"),
        "unexpected truncation error: {error}"
    );
    assert!(!output.is_empty(), "truncated bytes did not reach parser");
}

#[test]
fn rejects_trailing_bytes_after_raw_nar() {
    let mut nar = regular_nar(CONTENT);
    nar.push(0xff);
    let mut backend = RecordingExportBackend::successful(registered_path_info(), nar);
    let mut output = Vec::new();

    let error = export_verified_nar(Path::new(PATH), &mut output, &mut backend)
        .expect_err("trailing bytes must fail");

    assert!(error.to_string().contains("trailing bytes"));
    assert_eq!(output.len(), 136, "trailing byte reached caller");
}

#[test]
fn rejects_malformed_raw_nar_from_backend() {
    let mut backend =
        RecordingExportBackend::successful(registered_path_info(), b"not a NAR".to_vec());
    let mut output = Vec::new();

    let error = export_verified_nar(Path::new(PATH), &mut output, &mut backend)
        .expect_err("malformed NAR must fail");

    assert!(error.to_string().contains("NAR"));
}

#[test]
fn missing_path_fails_before_export() {
    let mut backend = RecordingExportBackend::missing();
    let mut output = Vec::new();

    let error = export_verified_nar(Path::new(PATH), &mut output, &mut backend)
        .expect_err("missing path must fail");

    assert!(error.to_string().contains("path info"));
    assert!(backend.requests.is_empty());
    assert!(output.is_empty());
}

#[test]
fn writer_failure_stops_export_and_surfaces_error() {
    let mut backend =
        RecordingExportBackend::successful(registered_path_info(), regular_nar(CONTENT));
    let mut writer = FailingWriter { remaining: 32 };

    let error = export_verified_nar(Path::new(PATH), &mut writer, &mut backend)
        .expect_err("writer failure must surface");

    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    assert!(
        backend.writer_failed,
        "backend did not observe writer failure"
    );
}

#[test]
fn slow_writer_receives_complete_nar_without_export_buffering() {
    let nar = regular_nar(CONTENT);
    let mut backend = RecordingExportBackend::successful(registered_path_info(), nar.clone());
    backend.chunk_size = 7;
    let mut writer = SlowWriter::default();

    export_verified_nar(Path::new(PATH), &mut writer, &mut backend)
        .expect("slow writer export verifies");

    assert_eq!(writer.output, nar);
    assert!(writer.writes > 1);
}

fn sri_sha256(value: &str) -> [u8; 32] {
    use base64::Engine;

    let encoded = value.strip_prefix("sha256-").expect("SHA-256 SRI hash");
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .expect("valid SRI base64")
        .try_into()
        .expect("SHA-256 length")
}

fn registered_path_info() -> RegisteredPathInfo {
    RegisteredPathInfo {
        path: PathBuf::from(PATH),
        nar_hash: NAR_HASH,
        nar_size: 136,
        references: Vec::new(),
        deriver: None,
        content_address: None,
    }
}

struct RecordingExportBackend {
    metadata: Option<RegisteredPathInfo>,
    nar: Vec<u8>,
    chunk_size: usize,
    queries: Vec<PathBuf>,
    requests: Vec<StoreExportRequest>,
    writer_failed: bool,
}

impl RecordingExportBackend {
    fn successful(metadata: RegisteredPathInfo, nar: Vec<u8>) -> Self {
        Self {
            metadata: Some(metadata),
            nar,
            chunk_size: 8192,
            queries: Vec::new(),
            requests: Vec::new(),
            writer_failed: false,
        }
    }

    fn missing() -> Self {
        Self {
            metadata: None,
            nar: Vec::new(),
            chunk_size: 8192,
            queries: Vec::new(),
            requests: Vec::new(),
            writer_failed: false,
        }
    }
}

struct FailingExportBackend;

impl StoreExportBackend for FailingExportBackend {
    fn store_uri(&self) -> &str {
        "unix:///run/nix-daemon.sock"
    }

    fn query_path_info(&mut self, _path: &Path) -> io::Result<RegisteredPathInfo> {
        Ok(registered_path_info())
    }

    fn export_nar(
        &mut self,
        _request: &StoreExportRequest,
        _nar_size: u64,
        sink: &mut dyn Write,
    ) -> io::Result<()> {
        sink.write_all(&regular_nar(CONTENT))?;
        Err(io::Error::other("backend export failed"))
    }
}

struct PanickingExportBackend;

impl StoreExportBackend for PanickingExportBackend {
    fn store_uri(&self) -> &str {
        "unix:///run/nix-daemon.sock"
    }

    fn query_path_info(&mut self, _path: &Path) -> io::Result<RegisteredPathInfo> {
        Ok(registered_path_info())
    }

    fn export_nar(
        &mut self,
        _request: &StoreExportRequest,
        _nar_size: u64,
        sink: &mut dyn Write,
    ) -> io::Result<()> {
        sink.write_all(&regular_nar(CONTENT))
            .expect("test NAR writes before panic");
        panic!("backend export panicked")
    }
}

impl StoreExportBackend for RecordingExportBackend {
    fn store_uri(&self) -> &str {
        "unix:///run/nix-daemon.sock"
    }

    fn query_path_info(&mut self, path: &Path) -> io::Result<RegisteredPathInfo> {
        self.queries.push(path.to_path_buf());
        self.metadata
            .clone()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "path info missing"))
    }

    fn export_nar(
        &mut self,
        request: &StoreExportRequest,
        _nar_size: u64,
        sink: &mut dyn Write,
    ) -> io::Result<()> {
        self.requests.push(request.clone());
        for chunk in self.nar.chunks(self.chunk_size) {
            if let Err(error) = sink.write_all(chunk) {
                self.writer_failed = true;
                return Err(error);
            }
        }
        Ok(())
    }
}

struct FailingWriter {
    remaining: usize,
}

impl Write for FailingWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "writer rejected export",
            ));
        }
        let written = buffer.len().min(self.remaining);
        self.remaining -= written;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct SlowWriter {
    output: Vec<u8>,
    writes: usize,
}

impl Write for SlowWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.writes += 1;
        let written = buffer.len().min(3);
        self.output.extend_from_slice(&buffer[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
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
        push_string(&mut nar, value);
    }
    nar
}

fn push_string(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&(value.len() as u64).to_le_bytes());
    output.extend_from_slice(value);
    output.resize(output.len().next_multiple_of(8), 0);
}
