use std::time::{Duration, Instant};
use telchar::service::archive::LogCapture;

#[test]
fn capture_rejects_symlinked_spool_ancestor() {
    let parent = tempfile::tempdir().unwrap();
    let foreign = tempfile::tempdir().unwrap();
    std::fs::create_dir(foreign.path().join("spool")).unwrap();
    std::os::unix::fs::symlink(foreign.path(), parent.path().join("redirected")).unwrap();
    let (capture, finished) =
        LogCapture::start(parent.path().join("redirected/spool"), 46, 1024, 1024).unwrap();
    capture.push(b"private output");
    capture.finish();
    assert!(
        finished
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .is_err()
    );
    assert_eq!(
        std::fs::read_dir(foreign.path().join("spool"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn capture_publication_stays_in_opened_directory() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("spool");
    let retained = parent.path().join("retained");
    let foreign = tempfile::tempdir().unwrap();
    std::fs::create_dir(&root).unwrap();
    let (capture, finished) = LogCapture::start(root.clone(), 45, 1024, 1024).unwrap();
    capture.push(b"private builder output");
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::fs::read_dir(&root).unwrap().next().is_none() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    std::fs::rename(&root, &retained).unwrap();
    std::os::unix::fs::symlink(foreign.path(), &root).unwrap();
    capture.finish();
    let receipt = finished
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    assert!(receipt.complete);
    assert_eq!(std::fs::read_dir(foreign.path()).unwrap().count(), 0);
    assert_eq!(
        zstd::stream::decode_all(std::fs::File::open(retained.join("45.log.zst")).unwrap())
            .unwrap(),
        b"private builder output"
    );
}
