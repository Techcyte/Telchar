use std::io::{self, Write};
use std::path::{Path, PathBuf};

use telchar::store_export::{
    export_verified_nar, StoreExportBackend, StoreExportRequest, VerifiedStoreExport,
};
use telchar::store_promotion::RegisteredPathInfo;

const CONTENT: &[u8] = b"telchar-classic-fixture";
const PATH: &str = "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-telchar-fixture";
const NAR_HASH: [u8; 32] = [
    0x6c, 0x2b, 0xe2, 0xf1, 0x2a, 0x16, 0x86, 0x05, 0xeb, 0xcb, 0xa3, 0x78, 0x22, 0x86, 0xc3, 0x8e,
    0xaf, 0x3f, 0x5d, 0x78, 0x7b, 0x7a, 0x8b, 0xb2, 0x4a, 0x54, 0x0d, 0x22, 0x67, 0xff, 0x68, 0xe1,
];

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
        vec![StoreExportRequest { path: PATH.into() }]
    );
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

impl StoreExportBackend for RecordingExportBackend {
    fn query_path_info(&mut self, path: &Path) -> io::Result<RegisteredPathInfo> {
        self.queries.push(path.to_path_buf());
        self.metadata
            .clone()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "path info missing"))
    }

    fn export_nar(&mut self, request: &StoreExportRequest, sink: &mut dyn Write) -> io::Result<()> {
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
