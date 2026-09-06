//! Measures allocation scaling and CPU work through real input transfer sessions.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use telchar::nomad::protocol::{
    BuildSpecification, Direction, Frame, FrameKind, InputManifest, InputTransferSession,
    NamedOutput, NarMetadata, PathManifestEntry, PathSet, ProtocolLimits, TransferSession,
    encode_metadata, read_frame, write_frame,
};

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

// All storage operations delegate unchanged to System; only this thread's enabled scope counts.
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

const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const CHUNK_BYTES: usize = 64 * 1024;
const CHUNKS: usize = 1024;
const NAR_BYTES: u64 = (CHUNK_BYTES * CHUNKS) as u64;
const METADATA_BYTES: usize = 4 * 1024 * 1024;

fn manifest(count: usize) -> InputManifest {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = |name: &str| format!("/nix/store/{}-{nonce}-{name}", "a".repeat(32));
    let output = path("output");
    let derivation_path = path("build.drv");
    let paths = (0..count)
        .map(|index| PathManifestEntry {
            path: path(&format!("input-{index:05}")),
            nar_hash: HASH.to_owned(),
            nar_size: NAR_BYTES,
            references: vec![],
            deriver: None,
            content_address: None,
        })
        .collect::<Vec<_>>();
    InputManifest {
        derivation_path: derivation_path.clone(),
        build: BuildSpecification {
            derivation_path: derivation_path.into_bytes(),
            outputs: vec![NamedOutput {
                name: b"out".to_vec(),
                path: output.clone().into_bytes(),
                hash_algorithm: vec![],
                hash: vec![],
            }],
            input_sources: vec![paths.last().unwrap().path.clone().into_bytes()],
            system: "x86_64-linux".to_owned(),
            required_system_features: vec![],
            builder: b"/bin/sh".to_vec(),
            arguments: vec![],
            environment: vec![(b"out".to_vec(), output.clone().into_bytes())],
        },
        paths,
        outputs: vec![output],
    }
}

fn metadata(path: &str, index: usize) -> NarMetadata {
    NarMetadata {
        path: path.to_owned(),
        nar_hash: HASH.to_owned(),
        nar_size: NAR_BYTES,
        offset: (index * CHUNK_BYTES) as u64,
        final_chunk: index + 1 == CHUNKS,
    }
}

fn requested(manifest: InputManifest) -> (InputTransferSession, String) {
    let count = manifest.paths.len();
    let path = manifest.paths.last().unwrap().path.clone();
    let valid = PathSet {
        paths: manifest.paths[..count - 1]
            .iter()
            .map(|entry| entry.path.clone())
            .collect(),
    };
    let mut session = InputTransferSession::new(manifest, count, NAR_BYTES, NAR_BYTES).unwrap();
    session.record_valid_paths(valid).unwrap();
    assert_eq!(
        session.request_unresolved().unwrap().paths,
        vec![path.clone()]
    );
    (session, path)
}

fn chunk_allocations(count: usize) -> usize {
    let (mut session, path) = requested(manifest(count));
    let chunk = metadata(&path, 0);
    ALLOCATIONS.with(|count| count.set(Some(0)));
    let result = session.receive_nar_chunk(chunk, CHUNK_BYTES as u64);
    let allocations = ALLOCATIONS.with(|count| count.replace(None).unwrap());
    result.unwrap();
    allocations
}

#[test]
fn chunk_allocations_do_not_scale_with_admitted_paths() {
    let small = chunk_allocations(1);
    let large = chunk_allocations(512);
    assert!(
        large <= small + 4,
        "per-chunk allocations: 1 path={small}, 512 paths={large}"
    );
}

fn frame(kind: FrameKind, value: &impl serde::Serialize) -> Frame {
    Frame::new(
        kind,
        encode_metadata(value, METADATA_BYTES).unwrap(),
        vec![],
    )
}

#[test]
#[ignore = "release-mode protocol CPU benchmark; no timing assertion"]
fn measure_input_chunk_validation() {
    let limits = ProtocolLimits::new(METADATA_BYTES, CHUNK_BYTES);
    for count in [1, 100, 10_000] {
        for repetition in 0..5 {
            let manifest = manifest(count);
            let valid = PathSet {
                paths: manifest.paths[..count - 1]
                    .iter()
                    .map(|entry| entry.path.clone())
                    .collect(),
            };
            let mut gateway = TransferSession::new(
                manifest.clone(),
                count,
                NAR_BYTES,
                NAR_BYTES,
                NAR_BYTES,
                NAR_BYTES,
                CHUNK_BYTES,
                METADATA_BYTES,
            )
            .unwrap();
            gateway
                .accept(
                    Direction::WorkerToGateway,
                    frame(FrameKind::ValidPaths, &valid),
                )
                .unwrap();
            let (mut worker, path) = requested(manifest);
            gateway
                .accept(
                    Direction::WorkerToGateway,
                    frame(
                        FrameKind::InputRequest,
                        &PathSet {
                            paths: vec![path.clone()],
                        },
                    ),
                )
                .unwrap();
            // Payload has fresh session identity. This protocol benchmark validates framing and
            // metadata, not NAR structure, content hashing, daemon import, or network transport.
            let mut payload = vec![0; CHUNK_BYTES];
            payload[..path.len()].copy_from_slice(path.as_bytes());
            let frames = (0..CHUNKS)
                .map(|index| {
                    Frame::new(
                        FrameKind::InputNar,
                        encode_metadata(&metadata(&path, index), METADATA_BYTES).unwrap(),
                        payload.clone(),
                    )
                })
                .collect::<Vec<_>>();
            let started = Instant::now();
            for outgoing in frames {
                gateway
                    .accept(Direction::GatewayToWorker, outgoing.clone())
                    .unwrap();
                let mut wire = Vec::new();
                write_frame(&mut wire, &outgoing, limits).unwrap();
                let incoming = read_frame(&mut wire.as_slice(), limits).unwrap();
                let metadata =
                    telchar::nomad::protocol::decode_metadata(incoming.metadata(), METADATA_BYTES)
                        .unwrap();
                worker
                    .receive_nar_chunk(metadata, incoming.payload().len() as u64)
                    .unwrap();
            }
            let seconds = started.elapsed().as_secs_f64();
            worker.ready_to_build().unwrap();
            println!(
                "input_chunks paths={count} repetition={repetition} bytes={NAR_BYTES} chunks={CHUNKS} endpoints=2 seconds={seconds:.6}"
            );
        }
    }
}

#[test]
fn rejects_invalid_chunk_metadata_without_advancing_input() {
    for case in [
        "foreign",
        "hash",
        "hash-syntax",
        "size",
        "limit",
        "offset",
        "empty",
        "oversize",
        "final",
        "available",
    ] {
        let manifest = manifest(3);
        let available = manifest.paths[0].path.clone();
        let (mut session, path) = requested(manifest);
        let mut chunk = metadata(&path, 0);
        let mut length = CHUNK_BYTES as u64;
        match case {
            "foreign" => chunk.path.push_str("-foreign"),
            "hash" => chunk.nar_hash = "a".repeat(64),
            "hash-syntax" => chunk.nar_hash = "z".repeat(64),
            "size" => chunk.nar_size -= 1,
            "limit" => chunk.nar_size += 1,
            "offset" => chunk.offset = 1,
            "empty" => length = 0,
            "oversize" => length = NAR_BYTES + 1,
            "final" => chunk.final_chunk = true,
            "available" => chunk.path = available,
            _ => unreachable!(),
        }
        let error = session.receive_nar_chunk(chunk, length).expect_err(case);
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData, "{case}");
        assert!(session.ready_to_build().is_err(), "{case}");
        let complete = NarMetadata {
            offset: 0,
            final_chunk: true,
            ..metadata(&path, 0)
        };
        session
            .receive_nar_chunk(complete.clone(), NAR_BYTES)
            .unwrap();
        session.ready_to_build().unwrap();
        assert!(
            session.receive_nar_chunk(complete, NAR_BYTES).is_err(),
            "duplicate after {case}"
        );
    }
}
