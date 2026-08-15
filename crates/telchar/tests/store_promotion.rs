//! Tests store promotion contracts and failure boundaries, including real store promotes matching nar with explicit metadata.

use std::io::{self, Cursor};
use std::path::{Path, PathBuf};

use telchar::fixture::nix::{NixFixture, TrustMode};
use telchar::service::transfer_limits::{LimitedReader, TransferBudget};
use telchar::store::promotion::{
    validate_and_promote_nar, DeclaredPathInfo, PromotionRequest, RegisteredPathInfo,
    StorePromotionBackend, MAXIMUM_PROMOTION_REFERENCES,
};

type DeclarationMutation = Box<dyn Fn(&mut DeclaredPathInfo)>;
type BeforePromote = Box<dyn FnMut(&PromotionRequest) -> io::Result<()>>;

const CONTENT: &[u8] = b"telchar-classic-fixture";
const NAR_HASH: [u8; 32] = [
    0x6c, 0x2b, 0xe2, 0xf1, 0x2a, 0x16, 0x86, 0x05, 0xeb, 0xcb, 0xa3, 0x78, 0x22, 0x86, 0xc3, 0x8e,
    0xaf, 0x3f, 0x5d, 0x78, 0x7b, 0x7a, 0x8b, 0xb2, 0x4a, 0x54, 0x0d, 0x22, 0x67, 0xff, 0x68, 0xe1,
];
const STORE_DIRECTORY: &str = "/nix/store";
const PATH: &str = "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-telchar-fixture";

#[path = "store_promotion/failure_cleanup.rs"]
mod failure_cleanup;
#[path = "store_promotion/real_store.rs"]
mod real_store;
#[path = "store_promotion/validation.rs"]
mod validation;

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
    before_promote: Option<BeforePromote>,
    promote_error: bool,
    valid_paths: Vec<bool>,
    queried_paths: Vec<PathBuf>,
}

impl RecordingBackend {
    fn accepting(registered: RegisteredPathInfo) -> Self {
        Self {
            request: None,
            registered: Some(registered),
            before_promote: None,
            promote_error: false,
            valid_paths: vec![true],
            queried_paths: Vec::new(),
        }
    }

    fn failing() -> Self {
        Self {
            request: None,
            registered: None,
            before_promote: None,
            promote_error: true,
            valid_paths: Vec::new(),
            queried_paths: Vec::new(),
        }
    }
}

impl StorePromotionBackend for RecordingBackend {
    fn store_uri(&self) -> &str {
        "unix:///run/nix-daemon.sock"
    }

    fn is_valid_path(&mut self, path: &Path) -> io::Result<bool> {
        self.queried_paths.push(path.to_path_buf());
        if self.valid_paths.is_empty() {
            return Ok(false);
        }
        Ok(self.valid_paths.remove(0))
    }

    fn before_promote(&mut self, request: &PromotionRequest) -> io::Result<()> {
        if let Some(before_promote) = &mut self.before_promote {
            before_promote(request)?;
        }
        Ok(())
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
        static DIRECTORY_SEQUENCE: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "telchar-promotion-contract-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_nanos(),
            DIRECTORY_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
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

fn sri_sha256(value: &str) -> [u8; 32] {
    use base64::Engine;

    let encoded = value.strip_prefix("sha256-").expect("SHA-256 SRI hash");
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .expect("valid SRI base64")
        .try_into()
        .expect("SHA-256 length")
}

fn raw_nar_from_export(exported: &[u8]) -> Vec<u8> {
    let mut offset = 8;
    parse_nar_node(exported, &mut offset);
    exported[8..offset].to_vec()
}

fn parse_nar_node(input: &[u8], offset: &mut usize) {
    assert_eq!(read_string(input, offset), b"nix-archive-1");
    parse_node(input, offset);
}

fn parse_node(input: &[u8], offset: &mut usize) {
    assert_eq!(read_string(input, offset), b"(");
    assert_eq!(read_string(input, offset), b"type");
    match read_string(input, offset).as_slice() {
        b"regular" => {
            let marker = read_string(input, offset);
            if marker == b"executable" {
                assert!(read_string(input, offset).is_empty());
                assert_eq!(read_string(input, offset), b"contents");
            } else {
                assert_eq!(marker, b"contents");
            }
            read_string(input, offset);
            assert_eq!(read_string(input, offset), b")");
        }
        other => panic!("unsupported fixture root node: {other:?}"),
    }
}

fn read_string(input: &[u8], offset: &mut usize) -> Vec<u8> {
    let length = u64::from_le_bytes(input[*offset..*offset + 8].try_into().expect("length"));
    *offset += 8;
    let end = *offset + length as usize;
    let value = input[*offset..end].to_vec();
    *offset = end.next_multiple_of(8);
    value
}
