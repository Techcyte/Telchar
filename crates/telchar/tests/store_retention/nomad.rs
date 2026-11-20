//! Exercises retained Nomad input roots across durable cleanup boundaries.

use super::*;

#[test]
fn unresolved_nomad_input_survives_restart_until_cleanup_resolves() {
    let isolated =
        std::env::var("TELCHAR_STORE_ROOT").expect("canonical isolated store runner required");
    assert!(std::path::Path::new(&isolated).join("socket-dir").is_dir());
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("input");
    fs::write(&source, "retained Nomad input").unwrap();
    let added = Command::new("nix-store")
        .args(["--store", "daemon", "--add"])
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    let input = std::path::PathBuf::from(String::from_utf8(added.stdout).unwrap().trim());
    let roots = directory.path().join("nomad-roots");
    fs::create_dir(&roots).unwrap();
    fs::set_permissions(&roots, fs::Permissions::from_mode(0o700)).unwrap();
    let mut retention = NixStoreRetentionBackend::new_with_store_directory(
        "unix:///nix/var/nix/daemon-socket/socket",
        "/nix/store",
        &roots,
    )
    .unwrap();
    retention
        .retain(&[RetentionEntry::new("nomad-input", input.to_string_lossy())])
        .unwrap();
    let mut database = PostgresFixture::start();
    telchar::persistence::migrate(database.url()).unwrap();
    let derivation = "/nix/store/11111111111111111111111111111111-nomad-retained.drv";
    telchar::persistence::open_protocol_session(
        database.url(),
        "nomad-session",
        "f3d3e3c63821a33f175cbe0dc4288e6e906ec8fe000df17c91d6ae616cc4ab1e",
        "ssh-pubkey:SHA256:test",
        "audit",
        "quota",
    )
    .unwrap();
    telchar::persistence::create_build_request(
        database.url(),
        "nomad-request",
        derivation,
        "x86_64-linux",
        "audit",
        "quota",
    )
    .unwrap();
    telchar::persistence::attach_request(database.url(), "nomad-session", "nomad-request").unwrap();
    telchar::persistence::create_store_lease(
        database.url(),
        "nomad-input",
        telchar::persistence::StoreLeaseOwnerKind::Request,
        "nomad-request",
        input.to_str().unwrap(),
        telchar::persistence::StoreLeasePurpose::Input,
    )
    .unwrap();
    let kind = telchar::backend::BackendKind::Nomad;
    telchar::persistence::claim_shared_build(
        database.url(),
        derivation,
        &[4; 32],
        "nomad",
        kind,
        kind.capabilities(),
        Some("retained-job"),
        &["/nix/store/22222222222222222222222222222222-result"],
    )
    .unwrap();
    telchar::persistence::start_shared_build(database.url(), derivation).unwrap();
    let attempt = telchar::persistence::retain_nomad_execution_obligation(
        database.url(),
        derivation,
        "retained-job",
    )
    .unwrap();
    assert!(
        telchar::persistence::detach_request_and_release_leases(
            database.url(),
            "nomad-session",
            "nomad-request"
        )
        .unwrap()
        .leases
        .is_empty()
    );
    telchar::persistence::close_protocol_session(database.url(), "nomad-session").unwrap();
    database.restart();
    telchar::store::retention::reconcile_output_retention(
        database.url(),
        &mut retention,
        SystemTime::now(),
    )
    .unwrap();
    assert!(roots.join("nomad-input").exists());
    let gc = Command::new("nix-store")
        .args(["--store", "daemon", "--delete"])
        .arg(&input)
        .output()
        .unwrap();
    assert!(!gc.status.success());
    assert!(
        String::from_utf8_lossy(&gc.stderr).contains("is still alive"),
        "{}",
        String::from_utf8_lossy(&gc.stderr)
    );
    assert!(input.exists());
    telchar::persistence::resolve_nomad_execution_obligation(database.url(), attempt).unwrap();
    telchar::store::retention::reconcile_output_retention(
        database.url(),
        &mut retention,
        SystemTime::now(),
    )
    .unwrap();
    assert!(!roots.join("nomad-input").exists());
    let gc = Command::new("nix-store")
        .args(["--store", "daemon", "--delete"])
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        gc.status.success(),
        "{}",
        String::from_utf8_lossy(&gc.stderr)
    );
    assert!(!input.exists());
}
