//! Measures producer allocations and real NAR parsing through the export transport.

use super::{ExportReader, ExportWriter};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::io::{self, Write};
use std::time::Instant;

struct AllocationCounter;
thread_local! {
    static ALLOCATIONS: Cell<Option<usize>> = const { Cell::new(None) };
}
fn record_allocation() {
    let _ = ALLOCATIONS.try_with(|count| {
        if let Some(value) = count.get() {
            count.set(Some(value + 1));
        }
    });
}
// Allocation behavior delegates to System; counting is enabled only on the producer thread.
unsafe impl GlobalAlloc for AllocationCounter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record_allocation();
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record_allocation();
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record_allocation();
        unsafe { System.realloc(pointer, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: AllocationCounter = AllocationCounter;

fn transfer(
    nar: &[u8],
    sink: &mut impl Write,
) -> (
    io::Result<super::super::nar::NarFingerprint>,
    io::Result<()>,
    usize,
    usize,
) {
    let (sender, receiver) = std::sync::mpsc::sync_channel(0);
    let (acknowledgement, result) = std::sync::mpsc::sync_channel(1);
    let reader = ExportReader {
        receiver,
        acknowledgement,
        pending: None,
        offset: 0,
    };
    let mut writer = ExportWriter { sender, result };
    std::thread::scope(|scope| {
        let producer = scope.spawn(move || {
            ALLOCATIONS.set(Some(0));
            let mut writes = 0;
            let result = nar.chunks(8192).try_for_each(|chunk| {
                writes += 1;
                writer.write_all(chunk)
            });
            let allocations = ALLOCATIONS.replace(None).unwrap();
            (result, allocations, writes)
        });
        let parsed = super::stage_nar(reader, sink);
        let (produced, allocations, writes) = producer.join().unwrap();
        (parsed, produced, allocations, writes)
    })
}

fn archive(files: usize, size: usize) -> Vec<u8> {
    let directory = tempfile::tempdir().unwrap();
    let nonce = directory.path().as_os_str().as_encoded_bytes();
    for index in 0..files {
        let bytes = (0..size)
            .map(|i| nonce[i % nonce.len()])
            .collect::<Vec<_>>();
        std::fs::write(directory.path().join(format!("{index:04}")), bytes).unwrap();
    }
    let output = std::process::Command::new("nix-store")
        .arg("--dump")
        .arg(directory.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    output.stdout
}

#[test]
fn transport_allocations_are_bounded_per_write() {
    let nar = archive(1, 128 * 1024);
    let mut output = Vec::new();
    let (parsed, produced, allocations, writes) = transfer(&nar, &mut output);
    produced.unwrap();
    assert_eq!(parsed.unwrap().size, nar.len() as u64);
    assert_eq!(output, nar);
    assert!(
        allocations <= writes + 4,
        "{allocations} allocations for {writes} writes"
    );
}

#[test]
fn transport_joins_after_invalid_nar_and_sink_failure() {
    let nar = archive(1, 128 * 1024);
    let mut malformed = nar.clone();
    malformed[8] ^= 1;
    let mut trailing = nar.clone();
    trailing.extend_from_slice(b"trailing");
    for (index, invalid) in [&malformed[..], &nar[..nar.len() - 1], &trailing[..]]
        .into_iter()
        .enumerate()
    {
        let (parsed, produced, _, _) = transfer(invalid, &mut io::sink());
        assert!(parsed.is_err());
        if index == 1 {
            produced.expect("producer sent complete truncated input before parser detects EOF");
        } else {
            assert!(produced.is_err());
        }
    }
    let mut full = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .unwrap();
    let (parsed, produced, _, writes) = transfer(&nar, &mut full);
    assert_eq!(parsed.unwrap_err().raw_os_error(), Some(libc::ENOSPC));
    assert_eq!(produced.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    assert_eq!(writes, 1, "failed write is not retried");
}

#[test]
#[ignore = "release export transport measurement"]
fn measure_export_transport() {
    use sha2::{Digest, Sha256};
    for (workload, files, size) in [
        ("small-files", 1000, 4096),
        ("large-file", 1, 16 * 1024 * 1024),
    ] {
        for repetition in 0..5 {
            let nar = archive(files, size);
            let hash = Sha256::digest(&nar);
            let direct = Instant::now();
            let expected = super::stage_nar(nar.as_slice(), io::sink()).unwrap();
            let direct_us = direct.elapsed().as_micros();
            let start = Instant::now();
            let (parsed, produced, allocations, writes) = transfer(&nar, &mut io::sink());
            let elapsed_us = start.elapsed().as_micros();
            produced.unwrap();
            let fingerprint = parsed.unwrap();
            assert_eq!(fingerprint, expected);
            assert_eq!(fingerprint.sha256.as_slice(), hash.as_slice());
            println!(
                "EXPORT_SYNC {{\"workload\":\"{workload}\",\"repetition\":{repetition},\"nar_bytes\":{},\"writes\":{writes},\"producer_allocations\":{allocations},\"elapsed_us\":{elapsed_us},\"direct_us\":{direct_us}}}",
                nar.len()
            );
        }
    }
}

#[test]
fn acknowledgements_do_not_cross_write_boundaries() {
    use std::io::Read;
    let (sender, receiver) = std::sync::mpsc::sync_channel(0);
    let (acknowledgement, result) = std::sync::mpsc::sync_channel(1);
    let mut reader = ExportReader {
        receiver,
        acknowledgement,
        pending: None,
        offset: 0,
    };
    let mut writer = ExportWriter { sender, result };
    std::thread::scope(|scope| {
        let producer = scope.spawn(move || {
            writer
                .write_all(b"first")
                .expect("first write acknowledged");
            let error = writer
                .write_all(b"second")
                .expect_err("reader drops during second write");
            assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
            let error = writer
                .write_all(b"third")
                .expect_err("closed reader cannot acknowledge another write");
            assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        });
        assert_eq!(reader.read(&mut []).unwrap(), 0);
        let mut first = [0; 5];
        reader.read_exact(&mut first).unwrap();
        assert_eq!(&first, b"first");
        let mut second = [0; 1];
        reader.read_exact(&mut second).unwrap();
        assert_eq!(&second, b"s");
        drop(reader);
        producer.join().unwrap();
    });
}

#[test]
fn empty_message_releases_waiting_writer_when_reader_stops() {
    use std::io::Read;
    let (sender, receiver) = std::sync::mpsc::sync_channel(0);
    let (acknowledgement, result) = std::sync::mpsc::sync_channel(1);
    let mut reader = ExportReader {
        receiver,
        acknowledgement,
        pending: None,
        offset: 0,
    };
    let mut writer = ExportWriter { sender, result };
    std::thread::scope(|scope| {
        let producer = scope.spawn(move || writer.write(&[]));
        assert_eq!(reader.read(&mut [0]).unwrap(), 0);
        drop(reader);
        assert_eq!(
            producer.join().unwrap().unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    });
}
