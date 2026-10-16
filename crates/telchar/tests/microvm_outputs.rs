use sha2::{Digest, Sha256};
use telchar::microvm::outputs::Outputs;
use telchar::nomad::protocol::{BuildOutcome, BuildResultMetadata, NarMetadata, PathManifestEntry};
use telchar::store::daemon::{GatewayStoreConnection, GatewayStoreEndpoint};

#[test]
fn receipts_require_authoritative_registration() {
    let Ok(uri) = std::env::var("MICROVM_TEST_STORE_URI") else {
        let output = std::process::Command::new("bash")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/microvm/canonical-store.sh"
            ))
            .arg(std::env::current_exe().unwrap())
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
    };
    let endpoint = GatewayStoreEndpoint::parse(&uri).unwrap();
    let staging = tempfile::tempdir().unwrap();
    let mut nar = Vec::new();
    for value in [
        b"nix-archive-1".as_slice(),
        b"(",
        b"type",
        b"regular",
        b"contents",
        b"microvm-output",
        b")",
    ] {
        nar.extend_from_slice(&(value.len() as u64).to_le_bytes());
        nar.extend_from_slice(value);
        nar.resize(nar.len().next_multiple_of(8), 0);
    }
    let path = "/nix/store/cccccccccccccccccccccccccccccccc-microvm-output";
    let metadata = PathManifestEntry {
        path: path.into(),
        nar_hash: Sha256::digest(&nar)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
        nar_size: nar.len() as u64,
        references: vec![],
        deriver: None,
        content_address: None,
    };
    let result = BuildResultMetadata {
        outcome: BuildOutcome::Built,
        diagnostic: None,
    };
    let mut store = GatewayStoreConnection::connect(&endpoint).unwrap();
    assert!(!store.is_valid_path(path.as_bytes()).unwrap());
    for corrupt in [true, false] {
        let mut outputs = Outputs::new(
            vec![path.into()],
            endpoint.clone(),
            staging.path().into(),
            1024,
            2048,
        )
        .unwrap();
        outputs.declare(metadata.clone()).unwrap();
        assert!(outputs.finish(&result).is_err());
        let chunk = NarMetadata {
            path: path.into(),
            nar_hash: metadata.nar_hash.clone(),
            nar_size: metadata.nar_size,
            offset: 0,
            final_chunk: true,
        };
        let mut bytes = nar.clone();
        if corrupt {
            let index = bytes
                .windows(14)
                .position(|value| value == b"microvm-output")
                .unwrap();
            bytes[index] ^= 1;
        }
        let receipt = outputs.receive(&chunk, &bytes);
        if corrupt {
            assert_eq!(receipt.unwrap_err().to_string(), "NAR hash mismatch");
            assert!(!store.is_valid_path(path.as_bytes()).unwrap());
            assert!(outputs.finish(&result).is_err());
        } else {
            assert!(receipt.unwrap().unwrap().accepted);
            let info = store.query_path_info(path.as_bytes()).unwrap().unwrap();
            assert_eq!(info.nar_hash_hex(), metadata.nar_hash);
            assert_eq!(info.nar_size(), metadata.nar_size);
            outputs.finish(&result).unwrap();
        }
        assert_eq!(std::fs::read_dir(staging.path()).unwrap().count(), 0);
    }
}
