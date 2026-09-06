//! Checks connection ownership and failure handling against an isolated real daemon.

use crate::fixture::nix::{NixFixture, TrustMode};
use std::net::Shutdown;

#[test]
fn failed_import_operation_discards_stream_without_replay() {
    for _ in 0..50 {
        check_operation();
    }
}

fn check_operation() {
    let fixture = NixFixture::create().expect("fixture creates");
    let mut daemon = fixture
        .start_daemon(TrustMode::Trusted)
        .expect("real daemon starts");
    let mut importer = daemon.promotion_backend().expect("importer creates");
    importer
        .with_connection(|connection| connection.query_missing(&[]))
        .expect("real query completes");
    assert!(importer.connection.is_some());
    let mut attempts = 0;
    let error = importer
        .with_connection(|connection| {
            attempts += 1;
            connection.shutdown_handle()?.shutdown(Shutdown::Both)?;
            connection.query_missing(&[])
        })
        .expect_err("closed socket fails");
    assert_eq!(error.to_string(), "gateway Nix daemon connection failed");
    assert_eq!(attempts, 1, "failed operation is not replayed");
    assert!(importer.connection.is_none(), "failed stream is discarded");
    importer
        .with_connection(|connection| connection.query_missing(&[]))
        .expect("next operation establishes a working connection");
    assert!(importer.connection.is_some());
    drop(importer);
    daemon.stop().expect("daemon stops");
    fixture.cleanup().expect("fixture cleans");
}

fn regular_nar() -> Vec<u8> {
    let mut nar = Vec::new();
    for value in [
        b"nix-archive-1".as_slice(),
        b"(",
        b"type",
        b"regular",
        b"contents",
        b"staged content",
        b")",
    ] {
        nar.extend_from_slice(&(value.len() as u64).to_le_bytes());
        nar.extend_from_slice(value);
        nar.resize(nar.len().next_multiple_of(8), 0);
    }
    nar
}

#[cfg(target_os = "linux")]
fn write_syscalls() -> u64 {
    std::fs::read_to_string("/proc/thread-self/io")
        .expect("thread I/O counters readable")
        .lines()
        .find_map(|line| line.strip_prefix("syscw: "))
        .expect("write syscall counter present")
        .parse()
        .expect("write syscall counter numeric")
}

#[cfg(target_os = "linux")]
#[test]
fn staging_coalesces_small_nar_writes_and_publishes_complete_bytes() {
    use sha2::{Digest, Sha256};
    let nar = regular_nar();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("archive");
    let mut file = std::fs::File::create(&path).unwrap();
    let before = write_syscalls();
    let fingerprint = super::stage_nar_to_file(nar.as_slice(), &mut file).unwrap();
    let writes = write_syscalls() - before;
    assert_eq!(std::fs::read(path).unwrap(), nar);
    assert_eq!(fingerprint.size, nar.len() as u64);
    assert_eq!(
        fingerprint.sha256.as_slice(),
        Sha256::digest(&nar).as_slice()
    );
    assert_eq!(
        writes, 1,
        "small NAR must reach file in one write, observed {writes}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn staging_reports_pending_write_failure() {
    let nar = regular_nar();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .unwrap();
    let before = write_syscalls();
    let error = super::stage_nar_to_file(nar.as_slice(), &mut file)
        .expect_err("failed staging write cannot report success");
    let writes = write_syscalls() - before;
    assert_eq!(error.raw_os_error(), Some(libc::ENOSPC));
    assert_eq!(writes, 1, "failed write must not be retried during drop");
}
