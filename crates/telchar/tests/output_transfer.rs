use std::io::{Read, Write};
use std::path::PathBuf;

use nix_worker_protocol::{
    write_query_path_info_response, PathInfoResponse, LATEST_WORKER_VERSION, STDERR_LAST,
};
use telchar::nix_fixture::{NixFixture, TrustMode};
use telchar::store_export::query_path_info;

#[test]
fn real_gateway_store_metadata_encodes_for_stock_nix() {
    let helper = std::env::var_os("TELCHAR_NIX_STORE_EXPORT")
        .map(PathBuf::from)
        .expect("flake-built export helper is configured");
    let fixture = NixFixture::create().expect("fixture creates");
    let mut daemon = fixture
        .start_daemon(TrustMode::Trusted)
        .expect("daemon starts");
    let path = daemon.build_classic_derivation().expect("path builds");
    let mut backend = daemon.export_backend(helper);

    let info = query_path_info(&path, &mut backend)
        .expect("path info queries")
        .unwrap();
    let references = info
        .references
        .iter()
        .map(|path| path.as_os_str().as_encoded_bytes().to_vec())
        .collect::<Vec<_>>();
    let deriver = info
        .deriver
        .as_ref()
        .map(|path| path.as_os_str().as_encoded_bytes());
    let nar_hash_hex = info
        .nar_hash
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let mut response = STDERR_LAST.to_le_bytes().to_vec();
    write_query_path_info_response(
        &mut response,
        LATEST_WORKER_VERSION,
        Some(PathInfoResponse {
            deriver,
            nar_hash_hex: &nar_hash_hex,
            references: &references,
            registration_time: 0,
            nar_size: info.nar_size,
            ultimate: false,
            signatures: &[],
            content_address: info.content_address.as_deref(),
        }),
    )
    .expect("path info response encodes");

    assert_eq!(
        u64::from_le_bytes(response[0..8].try_into().unwrap()),
        STDERR_LAST
    );
    assert_eq!(u64::from_le_bytes(response[8..16].try_into().unwrap()), 1);

    daemon.stop().expect("daemon stops");
    fixture.cleanup().expect("fixture cleans");
}

#[test]
fn missing_gateway_path_encodes_absence() {
    let helper = std::env::var_os("TELCHAR_NIX_STORE_EXPORT")
        .map(PathBuf::from)
        .expect("flake-built export helper is configured");
    let fixture = NixFixture::create().expect("fixture creates");
    let mut daemon = fixture
        .start_daemon(TrustMode::Trusted)
        .expect("daemon starts");
    let missing = PathBuf::from("/nix/store/00000000000000000000000000000000-missing");
    let mut backend = daemon.export_backend(helper);

    assert!(query_path_info(&missing, &mut backend)
        .expect("absence is not failure")
        .is_none());

    daemon.stop().expect("daemon stops");
    fixture.cleanup().expect("fixture cleans");
}

#[test]
fn raw_nar_response_has_terminal_frame_then_unframed_nar() {
    let mut output = STDERR_LAST.to_le_bytes().to_vec();
    output.write_all(&13_u64.to_le_bytes()).unwrap();
    output.write_all(b"nix-archive-1").unwrap();
    let mut input = output.as_slice();
    let mut word = [0_u8; 8];
    input.read_exact(&mut word).unwrap();
    assert_eq!(u64::from_le_bytes(word), STDERR_LAST);
    input.read_exact(&mut word).unwrap();
    assert_eq!(u64::from_le_bytes(word), 13);
}
