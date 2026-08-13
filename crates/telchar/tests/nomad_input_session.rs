use telchar::nomad_transfer_protocol::{
    InputManifest, InputTransferSession, NarMetadata, PathManifestEntry, PathSet,
};

const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn path(hash: char, name: &str) -> String {
    format!("/nix/store/{}-{name}", hash.to_string().repeat(32))
}

fn manifest() -> InputManifest {
    let first = path('a', "first");
    let second = path('b', "second");
    InputManifest {
        derivation_path: path('c', "build.drv"),
        paths: vec![
            PathManifestEntry {
                path: first.clone(),
                nar_hash: HASH.to_owned(),
                nar_size: 10,
                references: vec![],
                deriver: None,
            },
            PathManifestEntry {
                path: second,
                nar_hash: HASH.to_owned(),
                nar_size: 20,
                references: vec![first],
                deriver: None,
            },
        ],
        outputs: vec![path('d', "output")],
    }
}

#[test]
fn requests_only_unresolved_admitted_inputs_and_starts_when_complete() {
    let manifest = manifest();
    let first = manifest.paths[0].path.clone();
    let second = manifest.paths[1].path.clone();
    let mut session = InputTransferSession::new(manifest, 8, 32, 32).expect("session creates");

    session
        .record_valid_paths(PathSet { paths: vec![first] })
        .expect("valid subset records");
    let request = session.request_unresolved().expect("request creates");
    assert_eq!(request.paths, vec![second.clone()]);
    session
        .receive_nar(
            NarMetadata {
                path: second,
                nar_hash: HASH.to_owned(),
                nar_size: 20,
            },
            20,
        )
        .expect("requested NAR receives");
    session.ready_to_build().expect("all inputs resolve");
}

#[test]
fn rejects_foreign_duplicate_unrequested_and_mismatched_inputs() {
    let manifest = manifest();
    let first = manifest.paths[0].path.clone();
    let mut session = InputTransferSession::new(manifest, 8, 32, 32).expect("session creates");
    assert!(session
        .record_valid_paths(PathSet {
            paths: vec![path('e', "foreign")],
        })
        .is_err());
    session
        .record_valid_paths(PathSet { paths: vec![] })
        .expect("empty valid set records");
    assert!(session
        .receive_nar(
            NarMetadata {
                path: first.clone(),
                nar_hash: HASH.to_owned(),
                nar_size: 10,
            },
            10,
        )
        .is_err());
    session.request_unresolved().expect("requests inputs");
    assert!(session
        .receive_nar(
            NarMetadata {
                path: first.clone(),
                nar_hash: HASH.to_owned(),
                nar_size: 10,
            },
            9,
        )
        .is_err());
    session
        .receive_nar(
            NarMetadata {
                path: first.clone(),
                nar_hash: HASH.to_owned(),
                nar_size: 10,
            },
            10,
        )
        .expect("exact NAR receives");
    assert!(session
        .receive_nar(
            NarMetadata {
                path: first,
                nar_hash: HASH.to_owned(),
                nar_size: 10,
            },
            10,
        )
        .is_err());
    assert!(session.ready_to_build().is_err());
}

#[test]
fn enforces_aggregate_input_limit_before_requesting_transfer() {
    let manifest = manifest();
    let mut session = InputTransferSession::new(manifest, 8, 32, 29).expect("session creates");
    session
        .record_valid_paths(PathSet { paths: vec![] })
        .expect("valid set records");
    assert!(session.request_unresolved().is_err());
}
