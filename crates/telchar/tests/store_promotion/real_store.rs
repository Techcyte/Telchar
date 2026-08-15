//! Focused real store contracts.

use super::*;

#[test]
#[ignore = "private fixture paths are outside the production /nix/store namespace"]
fn real_store_promotes_matching_nar_with_explicit_metadata() {
    let source_fixture = NixFixture::create().expect("source fixture creates");
    let mut source_daemon = source_fixture
        .start_daemon(TrustMode::Trusted)
        .expect("source daemon starts");
    let path = source_daemon
        .build_classic_derivation()
        .expect("source path builds");
    let expected = source_daemon
        .query_path_info(&path)
        .expect("source metadata queries");
    let mut exported = Vec::new();
    source_daemon
        .export_path(&path, &mut exported)
        .expect("source exports");
    let nar = raw_nar_from_export(&exported);
    source_daemon
        .delete_path(&path)
        .expect("source path deletes before promotion");
    assert!(!source_daemon.is_valid_path(&path).expect("path absent"));

    let declared = DeclaredPathInfo {
        path: path.clone(),
        nar_hash: sri_sha256(&expected.nar_hash),
        nar_size: expected.nar_size,
        references: expected.references.clone(),
        deriver: expected.deriver.clone(),
        content_address: expected.content_address.clone(),
        signatures: Vec::new(),
        ultimate: false,
    };
    let mut backend = source_daemon
        .promotion_backend()
        .expect("gateway promotion backend creates");

    let registered = validate_and_promote_nar(
        Cursor::new(nar),
        source_daemon.temp_dir(),
        source_daemon.store_dir(),
        &declared,
        &mut backend,
    )
    .expect("validated NAR promotes");

    assert_eq!(registered.path, path);
    assert_eq!(registered.nar_hash, declared.nar_hash);
    assert_eq!(registered.nar_size, declared.nar_size);
    assert_eq!(registered.references, declared.references);
    assert_eq!(registered.deriver, declared.deriver);
    assert_eq!(registered.content_address, declared.content_address);

    source_daemon.stop().expect("source daemon stops");
    source_fixture.cleanup().expect("source fixture cleans");
}

#[test]
fn real_store_rejects_mutated_nar_before_authoritative_registration() {
    let source_fixture = NixFixture::create().expect("source fixture creates");
    let mut source_daemon = source_fixture
        .start_daemon(TrustMode::Trusted)
        .expect("source daemon starts");
    let path = source_daemon
        .build_classic_derivation()
        .expect("source path builds");
    let expected = source_daemon
        .query_path_info(&path)
        .expect("source metadata queries");
    let mut exported = Vec::new();
    source_daemon
        .export_path(&path, &mut exported)
        .expect("source exports");
    let mut nar = raw_nar_from_export(&exported);
    nar[96] ^= 0xff;
    source_daemon
        .delete_path(&path)
        .expect("source path deletes before rejection");

    let declared = DeclaredPathInfo {
        path: path.clone(),
        nar_hash: sri_sha256(&expected.nar_hash),
        nar_size: expected.nar_size,
        references: expected.references,
        deriver: expected.deriver,
        content_address: expected.content_address,
        signatures: Vec::new(),
        ultimate: false,
    };
    let mut backend = source_daemon
        .promotion_backend()
        .expect("gateway promotion backend creates");

    let error = validate_and_promote_nar(
        Cursor::new(nar),
        source_daemon.temp_dir(),
        source_daemon.store_dir(),
        &declared,
        &mut backend,
    )
    .expect_err("mutated NAR must fail");

    assert!(
        error.to_string().contains("NAR hash mismatch"),
        "mutation did not reach staged hash comparison: {error}"
    );
    assert!(!source_daemon
        .is_valid_path(&path)
        .expect("authoritative path query"));

    source_daemon.stop().expect("source daemon stops");
    source_fixture.cleanup().expect("source fixture cleans");
}
