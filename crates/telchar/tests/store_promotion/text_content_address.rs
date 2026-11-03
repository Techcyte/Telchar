//! Real daemon contracts for Nix text objects, not floating CA derivations.

use super::*;
use nix_worker_protocol::{
    LATEST_WORKER_VERSION, ProtocolSessionLimits, WorkerOperation, WorkerReader,
    write_worker_byte_string, write_worker_integer,
};
use telchar::store::daemon::GatewayStoreEndpoint;
use telchar::store::import::{GatewayStoreImport, StoreImportBackend};
use telchar::store::promotion::GatewayStorePromotionBackend;

fn nix(program: &str, arguments: &[&str]) -> Vec<u8> {
    assert!(
        std::env::var_os("TELCHAR_STORE_ROOT").is_some(),
        "run text CA contracts under tests/canonical-store.sh"
    );
    let output = std::process::Command::new(program)
        .args([
            "--store",
            "daemon",
            "--option",
            "builders",
            "",
            "--option",
            "post-build-hook",
            "",
            "--option",
            "substituters",
            "",
        ])
        .args(arguments)
        .output()
        .expect("isolated Nix command runs");
    assert!(
        output.status.success(),
        "{program} {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn backend() -> GatewayStorePromotionBackend {
    GatewayStorePromotionBackend::new(
        GatewayStoreEndpoint::parse("unix:///nix/var/nix/daemon-socket/socket").unwrap(),
    )
}

fn text_paths() -> Vec<PathBuf> {
    let drv = nix(
        "nix-instantiate",
        &[
            "--expr",
            "derivation { name = \"telchar-text-ca\"; system = builtins.currentSystem; builder = \"/bin/sh\"; }",
        ],
    );
    let text = nix(
        "nix-instantiate",
        &[
            "--eval",
            "--json",
            "--read-write-mode",
            "--expr",
            "let ref = builtins.toFile \"telchar-text-reference\" \"reference contents\"; in builtins.toFile \"telchar-text-with-reference\" \"text referencing ${ref}\\n\"",
        ],
    );
    let text = serde_json::from_slice::<String>(&text)
        .unwrap()
        .into_bytes();
    [drv, text]
        .into_iter()
        .map(|value| PathBuf::from(String::from_utf8(value).unwrap().trim()))
        .collect()
}

fn export_and_delete(path: &Path) -> (DeclaredPathInfo, Vec<u8>, Vec<u8>) {
    let info = backend().query_path_info(path).unwrap();
    assert!(
        info.content_address
            .as_deref()
            .unwrap()
            .starts_with("text:sha256:")
    );
    let contents = std::fs::read(path).unwrap();
    let exported = nix("nix-store", &["--export", path.to_str().unwrap()]);
    let nar = raw_nar_from_export(&exported);
    nix("nix-store", &["--delete", path.to_str().unwrap()]);
    assert!(!backend().is_valid_path(path).unwrap());
    (
        DeclaredPathInfo {
            path: info.path,
            nar_hash: info.nar_hash,
            nar_size: info.nar_size,
            references: info.references,
            deriver: info.deriver,
            content_address: info.content_address,
            signatures: vec![],
            ultimate: false,
        },
        nar,
        contents,
    )
}

fn import(declared: &DeclaredPathInfo, nar: &[u8]) -> io::Result<()> {
    let mut logical = Vec::new();
    write_worker_integer(&mut logical, 1);
    write_worker_byte_string(&mut logical, declared.path.as_os_str().as_encoded_bytes());
    write_worker_byte_string(
        &mut logical,
        declared
            .deriver
            .as_ref()
            .map_or(b"", |path| path.as_os_str().as_encoded_bytes()),
    );
    write_worker_byte_string(&mut logical, hex_hash(declared.nar_hash).as_bytes());
    write_worker_integer(&mut logical, declared.references.len() as u64);
    for reference in &declared.references {
        write_worker_byte_string(&mut logical, reference.as_os_str().as_encoded_bytes());
    }
    for value in [0, declared.nar_size, 0, 0] {
        write_worker_integer(&mut logical, value);
    }
    write_worker_byte_string(
        &mut logical,
        declared.content_address.as_deref().unwrap().as_bytes(),
    );
    logical.extend_from_slice(nar);
    let mut wire = Vec::new();
    for value in [44, 0, 1, logical.len() as u64] {
        write_worker_integer(&mut wire, value);
    }
    wire.extend_from_slice(&logical);
    write_worker_integer(&mut wire, 0);
    let mut reader = WorkerReader::new(
        wire.as_slice(),
        ProtocolSessionLimits::new(1024 * 1024, std::time::Duration::from_secs(10)),
    );
    assert_eq!(
        reader.read_operation().unwrap(),
        WorkerOperation::AddMultipleToStore
    );
    let mut importer = GatewayStoreImport::new(
        GatewayStoreEndpoint::parse("unix:///nix/var/nix/daemon-socket/socket").unwrap(),
    )?;
    let result = reader
        .complete_add_multiple_to_store(LATEST_WORKER_VERSION, |info, source| {
            importer.import(info, source)
        })
        .map(|_| ())
        .map_err(|error| io::Error::new(error.kind(), error.to_string()));
    assert_directory_empty(importer.staging_directory().unwrap());
    result
}

fn assert_preserved(declared: &DeclaredPathInfo, contents: &[u8]) {
    let registered = backend().query_path_info(&declared.path).unwrap();
    assert_eq!(
        registered,
        RegisteredPathInfo {
            path: declared.path.clone(),
            nar_hash: declared.nar_hash,
            nar_size: declared.nar_size,
            references: declared.references.clone(),
            deriver: declared.deriver.clone(),
            content_address: declared.content_address.clone(),
        }
    );
    assert_eq!(std::fs::read(&declared.path).unwrap(), contents);
    nix(
        "nix-store",
        &["--verify-path", declared.path.to_str().unwrap()],
    );
}

fn round_trip_text_object(through_import: bool, index: usize) {
    // Run under tests/canonical-store.sh: no host-store registration or builders.
    let staging = TestDirectory::create();
    for path in text_paths().into_iter().skip(index).take(1) {
        let (declared, nar, contents) = export_and_delete(&path);
        if !path.to_str().unwrap().ends_with(".drv") {
            assert_eq!(declared.references.len(), 1);
            assert!(
                String::from_utf8_lossy(&contents)
                    .contains(declared.references[0].to_str().unwrap())
            );
        }
        if through_import {
            import(&declared, &nar).expect("real text CA imports through AddMultipleToStore");
        } else {
            validate_and_promote_nar(
                Cursor::new(&nar),
                staging.path(),
                Path::new(STORE_DIRECTORY),
                &declared,
                &mut backend(),
            )
            .expect("real text CA promotes");
        }
        assert_preserved(&declared, &contents);
        nix("nix-store", &["--delete", path.to_str().unwrap()]);
    }
    assert_directory_empty(staging.path());
}

#[test]
fn real_text_ca_derivation_promotes() {
    round_trip_text_object(false, 0);
}

#[test]
fn real_text_ca_reference_bearing_to_file_promotes() {
    round_trip_text_object(false, 1);
}

#[test]
fn real_text_ca_derivation_imports() {
    round_trip_text_object(true, 0);
}

#[test]
fn real_text_ca_reference_bearing_to_file_imports() {
    round_trip_text_object(true, 1);
}

#[test]
fn real_text_ca_rejects_inconsistent_content_address_references_and_executable_nar() {
    use sha2::{Digest, Sha256};
    let staging = TestDirectory::create();
    let path = text_paths().pop().unwrap();
    let (declared, nar, contents) = export_and_delete(&path);
    for case in [
        "malformed CA",
        "wrong CA hash",
        "wrong references",
        "executable",
        "wrong content",
    ] {
        let mut invalid = declared.clone();
        let mut invalid_nar = nar.clone();
        match case {
            "malformed CA" => invalid.content_address = Some("text:sha256:invalid".into()),
            "wrong CA hash" => {
                invalid.content_address = Some(format!("text:sha256:{}", "0".repeat(52)))
            }
            "wrong references" => invalid.references.clear(),
            "executable" => {
                invalid_nar.clear();
                for value in [
                    b"nix-archive-1".as_slice(),
                    b"(",
                    b"type",
                    b"regular",
                    b"executable",
                    b"",
                    b"contents",
                    contents.as_slice(),
                    b")",
                ] {
                    push_string(&mut invalid_nar, value);
                }
            }
            "wrong content" => invalid_nar = regular_nar(b"different text contents"),
            _ => unreachable!(),
        }
        // Match the NAR declaration so rejection must validate the text CA, not just the transport hash.
        invalid.nar_hash = Sha256::digest(&invalid_nar).into();
        invalid.nar_size = invalid_nar.len() as u64;
        let error = validate_and_promote_nar(
            Cursor::new(&invalid_nar),
            staging.path(),
            Path::new(STORE_DIRECTORY),
            &invalid,
            &mut backend(),
        )
        .expect_err(&format!("{case} must not register"));
        let expected_error = match case {
            "malformed CA" => "invalid text content address",
            "executable" => "text NAR root must be a non-executable regular file",
            _ => "promotion failed",
        };
        assert!(
            error.to_string().contains(expected_error),
            "{case}: {error}"
        );
        assert!(
            !backend().is_valid_path(&path).unwrap(),
            "{case} registered invalid path"
        );
        let error = import(&invalid, &invalid_nar).unwrap_err();
        assert!(
            error.to_string().contains(expected_error),
            "import {case}: {error}"
        );
        assert!(!backend().is_valid_path(&path).unwrap());
        assert_directory_empty(staging.path());
    }
}
