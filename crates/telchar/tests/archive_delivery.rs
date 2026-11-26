//! Tests durable archive delivery through operator-owned destinations.

use telchar::service::archive::FilesystemArchive;

#[test]
fn filesystem_delivery_is_durable_idempotent_and_detects_conflicts() {
    let directory = tempfile::tempdir().unwrap();
    let archive = FilesystemArchive::new(directory.path().to_owned()).unwrap();
    let payload = b"{\"schema_version\":1,\"attempt_id\":42}\n";
    let receipt = archive.deliver("history/42.jsonl", payload).unwrap();
    assert_eq!(std::fs::read(&receipt.path).unwrap(), payload);
    assert_eq!(
        archive.deliver("history/42.jsonl", payload).unwrap(),
        receipt
    );
    assert!(archive.deliver("history/42.jsonl", b"different").is_err());
    assert_eq!(std::fs::read(&receipt.path).unwrap(), payload);
    assert!(archive.deliver("../escape", payload).is_err());
    assert!(archive.deliver("/absolute", payload).is_err());
    assert!(archive.deliver("history//other", payload).is_err());
}

#[test]
fn filesystem_delivery_does_not_follow_destination_symlinks() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    symlink(outside.path(), directory.path().join("history")).unwrap();
    let archive = FilesystemArchive::new(directory.path().to_owned()).unwrap();
    assert!(archive.deliver("history/42.jsonl", b"private").is_err());
    assert!(!outside.path().join("42.jsonl").exists());
}
