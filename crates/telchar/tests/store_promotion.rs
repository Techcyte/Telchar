use std::io::{self, Cursor};
use std::path::{Path, PathBuf};

use telchar::store_promotion::{
    DeclaredPathInfo, PromotionRequest, RegisteredPathInfo, StorePromotionBackend,
    validate_and_promote_nar,
};

const CONTENT: &[u8] = b"telchar-classic-fixture";
const NAR_HASH: [u8; 32] = [
    0x6c, 0x2b, 0xe2, 0xf1, 0x2a, 0x16, 0x86, 0x05, 0xeb, 0xcb, 0xa3, 0x78, 0x22, 0x86, 0xc3, 0x8e,
    0xaf, 0x3f, 0x5d, 0x78, 0x7b, 0x7a, 0x8b, 0xb2, 0x4a, 0x54, 0x0d, 0x22, 0x67, 0xff, 0x68, 0xe1,
];
const STORE_DIRECTORY: &str = "/nix/store";
const PATH: &str = "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-telchar-fixture";

#[test]
fn promotes_matching_staged_nar_and_verifies_registered_metadata() {
    let staging = TestDirectory::create();
    let declared = declared_path_info();
    let mut backend = RecordingBackend::accepting(registered_path_info());

    let registered = validate_and_promote_nar(
        Cursor::new(regular_nar(CONTENT)),
        staging.path(),
        Path::new(STORE_DIRECTORY),
        &declared,
        &mut backend,
    )
    .expect("matching NAR promotes");

    assert_eq!(registered, registered_path_info());
    let request = backend.request.expect("helper invoked once");
    assert_eq!(request.version, 1);
    assert_eq!(request.path, declared.path);
    assert_eq!(request.nar_hash_hex, hex_hash(NAR_HASH));
    assert_eq!(request.nar_size, declared.nar_size);
    assert!(request.nar_path.starts_with(staging.path()));
    assert!(
        !request.nar_path.exists(),
        "staging file leaked after success"
    );
    assert_eq!(backend.queried_paths, vec![declared.path]);
    assert_directory_empty(staging.path());
}

#[test]
fn rejects_content_hash_mismatch_before_helper_invocation() {
    let staging = TestDirectory::create();
    let mut nar = regular_nar(CONTENT);
    nar[96] ^= 0xff;
    let declared = declared_path_info();
    let mut backend = RecordingBackend::accepting(registered_path_info());

    let error = validate_and_promote_nar(
        Cursor::new(nar),
        staging.path(),
        Path::new(STORE_DIRECTORY),
        &declared,
        &mut backend,
    )
    .expect_err("mutated content must fail");

    assert!(error.to_string().contains("NAR hash mismatch"));
    assert!(
        backend.request.is_none(),
        "helper invoked for hash mismatch"
    );
    assert_eq!(backend.queried_paths, vec![declared.path]);
    assert_directory_empty(staging.path());
}

#[test]
fn rejects_declared_size_mismatch_before_helper_invocation() {
    let staging = TestDirectory::create();
    let mut declared = declared_path_info();
    declared.nar_size += 1;
    let mut backend = RecordingBackend::accepting(registered_path_info());

    let error = validate_and_promote_nar(
        Cursor::new(regular_nar(CONTENT)),
        staging.path(),
        Path::new(STORE_DIRECTORY),
        &declared,
        &mut backend,
    )
    .expect_err("size mismatch must fail");

    assert!(error.to_string().contains("NAR size mismatch"));
    assert!(
        backend.request.is_none(),
        "helper invoked for size mismatch"
    );
    assert_eq!(backend.queried_paths, vec![declared.path]);
    assert_directory_empty(staging.path());
}

#[test]
fn rejects_unsupported_classic_metadata_before_staging() {
    let cases: Vec<(&str, Box<dyn Fn(&mut DeclaredPathInfo)>)> = vec![
        (
            "content address",
            Box::new(|info| info.content_address = Some("fixed:r:sha256:00".into())),
        ),
        (
            "signature",
            Box::new(|info| info.signatures.push("sig".into())),
        ),
        ("ultimate", Box::new(|info| info.ultimate = true)),
    ];
    for (label, mutate) in cases {
        let staging = TestDirectory::create();
        let mut declared = declared_path_info();
        mutate(&mut declared);
        let mut backend = RecordingBackend::accepting(registered_path_info());

        let error = validate_and_promote_nar(
            Cursor::new(regular_nar(CONTENT)),
            staging.path(),
            Path::new(STORE_DIRECTORY),
            &declared,
            &mut backend,
        )
        .unwrap_err();

        assert!(
            error.to_string().contains("unsupported"),
            "{label}: {error}"
        );
        assert!(backend.request.is_none(), "{label} invoked helper");
        assert!(backend.queried_paths.is_empty(), "{label} queried store");
        assert_directory_empty(staging.path());
    }
}

#[test]
fn rejects_invalid_and_duplicate_path_metadata_before_staging() {
    let cases: Vec<(&str, Box<dyn Fn(&mut DeclaredPathInfo)>)> = vec![
        (
            "path outside store",
            Box::new(|info| info.path = PathBuf::from("/tmp/not-store")),
        ),
        (
            "invalid path hash",
            Box::new(|info| {
                info.path = PathBuf::from("/nix/store/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-bad")
            }),
        ),
        (
            "invalid reference",
            Box::new(|info| info.references.push(PathBuf::from("relative"))),
        ),
        (
            "duplicate reference",
            Box::new(|info| info.references.push(info.references[0].clone())),
        ),
        (
            "invalid deriver",
            Box::new(|info| info.deriver = Some(PathBuf::from(PATH))),
        ),
    ];

    for (label, mutate) in cases {
        let staging = TestDirectory::create();
        let mut declared = declared_path_info();
        mutate(&mut declared);
        let mut backend = RecordingBackend::accepting(registered_path_info());

        let error = validate_and_promote_nar(
            Cursor::new(regular_nar(CONTENT)),
            staging.path(),
            Path::new(STORE_DIRECTORY),
            &declared,
            &mut backend,
        )
        .unwrap_err();

        assert!(error.to_string().contains("invalid"), "{label}: {error}");
        assert!(backend.request.is_none(), "{label} invoked helper");
        assert!(backend.queried_paths.is_empty(), "{label} queried store");
        assert_directory_empty(staging.path());
    }
}

#[test]
fn rejects_missing_reference_before_staging() {
    let staging = TestDirectory::create();
    let declared = declared_path_info();
    let mut backend = RecordingBackend::accepting(registered_path_info());
    backend.valid_paths = vec![false];

    let error = validate_and_promote_nar(
        Cursor::new(regular_nar(CONTENT)),
        staging.path(),
        Path::new(STORE_DIRECTORY),
        &declared,
        &mut backend,
    )
    .expect_err("missing reference must fail");

    assert!(error.to_string().contains("reference is not valid"));
    assert!(backend.request.is_none());
    assert_directory_empty(staging.path());
}

#[test]
fn helper_failure_is_followed_by_authoritative_validity_query_and_cleanup() {
    let staging = TestDirectory::create();
    let declared = declared_path_info();
    let mut backend = RecordingBackend::failing();
    backend.valid_paths = vec![true, false];

    let error = validate_and_promote_nar(
        Cursor::new(regular_nar(CONTENT)),
        staging.path(),
        Path::new(STORE_DIRECTORY),
        &declared,
        &mut backend,
    )
    .expect_err("helper failure must surface");

    assert!(error.to_string().contains("promotion failed"));
    assert!(backend.request.is_some(), "helper was not invoked");
    assert_eq!(backend.queried_paths.last(), Some(&declared.path));
    assert_directory_empty(staging.path());
}

#[test]
fn rejects_registered_metadata_mismatch() {
    let staging = TestDirectory::create();
    let declared = declared_path_info();
    let mut wrong = registered_path_info();
    wrong.nar_size += 1;
    let mut backend = RecordingBackend::accepting(wrong);

    let error = validate_and_promote_nar(
        Cursor::new(regular_nar(CONTENT)),
        staging.path(),
        Path::new(STORE_DIRECTORY),
        &declared,
        &mut backend,
    )
    .expect_err("registered metadata mismatch must fail");

    assert!(error.to_string().contains("registered metadata mismatch"));
    assert_directory_empty(staging.path());
}

fn declared_path_info() -> DeclaredPathInfo {
    DeclaredPathInfo {
        path: PathBuf::from(PATH),
        nar_hash: NAR_HASH,
        nar_size: 136,
        references: vec![PathBuf::from(
            "/nix/store/11111111111111111111111111111111-reference",
        )],
        deriver: Some(PathBuf::from(
            "/nix/store/22222222222222222222222222222222-builder.drv",
        )),
        content_address: None,
        signatures: Vec::new(),
        ultimate: false,
    }
}

fn registered_path_info() -> RegisteredPathInfo {
    let declared = declared_path_info();
    RegisteredPathInfo {
        path: declared.path,
        nar_hash: declared.nar_hash,
        nar_size: declared.nar_size,
        references: declared.references,
        deriver: declared.deriver,
        content_address: declared.content_address,
    }
}

struct RecordingBackend {
    request: Option<PromotionRequest>,
    registered: Option<RegisteredPathInfo>,
    promote_error: bool,
    valid_paths: Vec<bool>,
    queried_paths: Vec<PathBuf>,
}

impl RecordingBackend {
    fn accepting(registered: RegisteredPathInfo) -> Self {
        Self {
            request: None,
            registered: Some(registered),
            promote_error: false,
            valid_paths: vec![true],
            queried_paths: Vec::new(),
        }
    }

    fn failing() -> Self {
        Self {
            request: None,
            registered: None,
            promote_error: true,
            valid_paths: Vec::new(),
            queried_paths: Vec::new(),
        }
    }
}

impl StorePromotionBackend for RecordingBackend {
    fn is_valid_path(&mut self, path: &Path) -> io::Result<bool> {
        self.queried_paths.push(path.to_path_buf());
        if self.valid_paths.is_empty() {
            return Ok(false);
        }
        Ok(self.valid_paths.remove(0))
    }

    fn promote(&mut self, request: &PromotionRequest) -> io::Result<()> {
        self.request = Some(request.clone());
        if self.promote_error {
            return Err(io::Error::other("helper rejected promotion"));
        }
        Ok(())
    }

    fn query_path_info(&mut self, path: &Path) -> io::Result<RegisteredPathInfo> {
        self.queried_paths.push(path.to_path_buf());
        self.registered
            .clone()
            .ok_or_else(|| io::Error::other("no registered path"))
    }
}

struct TestDirectory {
    path: PathBuf,
}

impl TestDirectory {
    fn create() -> Self {
        let path = std::env::temp_dir().join(format!(
            "telchar-promotion-contract-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ));
        std::fs::create_dir(&path).expect("staging directory creates");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn assert_directory_empty(path: &Path) {
    assert_eq!(
        std::fs::read_dir(path)
            .expect("staging directory reads")
            .count(),
        0,
        "staging directory is not empty"
    );
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

fn hex_hash(hash: [u8; 32]) -> String {
    hash.into_iter().map(|byte| format!("{byte:02x}")).collect()
}
