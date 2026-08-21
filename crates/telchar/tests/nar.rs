//! Tests nar contracts and failure boundaries, including stages one valid nar and reports its fingerprint.

use std::io::Cursor;

use telchar::store::nar::{read_regular_nar, stage_nar};

const NAR_MAGIC: &[u8] = b"nix-archive-1";
const CONTENT: &[u8] = b"telchar-classic-fixture";
const CONTENT_OFFSET: usize = 96;

#[test]
fn extracts_one_bounded_regular_file() {
    let contents = b"Derive([],[],[],\"x86_64-linux\",\"/bin/sh\",[],[])";
    assert_eq!(
        read_regular_nar(Cursor::new(regular_nar(contents)), 4096).unwrap(),
        contents
    );
}

#[test]
fn rejects_non_regular_and_oversized_regular_extraction() {
    let directory = nar_with_root(directory_node(&[(b"entry", regular_node(b"value"))]));
    assert!(read_regular_nar(Cursor::new(directory), 4096).is_err());
    assert!(read_regular_nar(Cursor::new(regular_nar(b"oversized")), 4).is_err());
}

#[test]
fn stages_one_valid_nar_and_reports_its_fingerprint() {
    let nar = regular_nar(CONTENT);
    let mut staged = Vec::new();

    let fingerprint = stage_nar(Cursor::new(&nar), &mut staged).expect("valid NAR stages");

    assert_eq!(staged, nar);
    assert_eq!(fingerprint.size, 136);
    assert_eq!(
        fingerprint.sha256,
        [
            0x6c, 0x2b, 0xe2, 0xf1, 0x2a, 0x16, 0x86, 0x05, 0xeb, 0xcb, 0xa3, 0x78, 0x22, 0x86,
            0xc3, 0x8e, 0xaf, 0x3f, 0x5d, 0x78, 0x7b, 0x7a, 0x8b, 0xb2, 0x4a, 0x54, 0x0d, 0x22,
            0x67, 0xff, 0x68, 0xe1,
        ]
    );
}

#[test]
fn stages_mutated_valid_nar_with_a_different_fingerprint() {
    let original = regular_nar(CONTENT);
    let mut mutated = original.clone();
    mutated[CONTENT_OFFSET] ^= 0xff;
    let mut original_staged = Vec::new();
    let mut mutated_staged = Vec::new();

    let original_fingerprint =
        stage_nar(Cursor::new(&original), &mut original_staged).expect("original NAR stages");
    let mutated_fingerprint = stage_nar(Cursor::new(&mutated), &mut mutated_staged)
        .expect("mutated but structurally valid NAR stages");

    assert_eq!(mutated_staged, mutated);
    assert_ne!(mutated_fingerprint.sha256, original_fingerprint.sha256);
    assert_eq!(mutated_fingerprint.size, original_fingerprint.size);
}

#[test]
fn rejects_truncated_nar_without_staging_success() {
    let nar = regular_nar(CONTENT);
    let mut staged = Vec::new();

    let error = stage_nar(Cursor::new(&nar[..nar.len() - 1]), &mut staged)
        .expect_err("truncated NAR must fail");

    assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
}

#[test]
fn rejects_declared_content_length_above_available_input() {
    let mut nar = regular_nar(CONTENT);
    nar[88..96].copy_from_slice(&(CONTENT.len() as u64 + 8).to_le_bytes());
    let mut staged = Vec::new();

    let error =
        stage_nar(Cursor::new(nar), &mut staged).expect_err("oversized declared content must fail");

    assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
}

#[test]
fn rejects_bytes_after_complete_nar() {
    let mut nar = regular_nar(CONTENT);
    nar.extend_from_slice(b"trailing");
    let mut staged = Vec::new();

    let error = stage_nar(Cursor::new(nar), &mut staged).expect_err("trailing bytes must fail");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn accepts_executable_regular_file() {
    let nar = nar_with_root(executable_regular_node(CONTENT));
    let mut staged = Vec::new();

    stage_nar(Cursor::new(&nar), &mut staged).expect("executable regular NAR stages");

    assert_eq!(staged, nar);
}

#[test]
fn accepts_directory_with_sorted_entries_and_symlink() {
    let nar = nar_with_root(directory_node(&[
        (b"file", regular_node(CONTENT)),
        (b"link", symlink_node(b"file")),
    ]));
    let mut staged = Vec::new();

    stage_nar(Cursor::new(&nar), &mut staged).expect("directory NAR stages");

    assert_eq!(staged, nar);
}

#[test]
fn rejects_invalid_symlink_targets() {
    for (label, target) in [
        ("empty", b"".as_slice()),
        ("nul", b"bad\0target".as_slice()),
        ("too long", &[b'x'; 4096]),
    ] {
        let nar = nar_with_root(symlink_node(target));
        match stage_nar(Cursor::new(nar), Vec::new()) {
            Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::InvalidData, "{label}"),
            Ok(fingerprint) => panic!("{label} symlink target must fail: {fingerprint:?}"),
        }
    }
}

#[test]
fn rejects_invalid_directory_names() {
    for (label, name) in [
        ("empty", b"".as_slice()),
        ("dot", b".".as_slice()),
        ("dot-dot", b"..".as_slice()),
        ("slash", b"with/slash".as_slice()),
        ("nul", b"with\0nul".as_slice()),
        ("too long", &[b'x'; 256]),
    ] {
        let nar = nar_with_root(directory_node(&[(name, regular_node(CONTENT))]));
        match stage_nar(Cursor::new(nar), Vec::new()) {
            Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::InvalidData, "{label}"),
            Ok(fingerprint) => panic!("{label} directory name must fail: {fingerprint:?}"),
        }
    }
}

#[test]
fn rejects_unsorted_directory_entries() {
    let nar = nar_with_root(directory_node(&[
        (b"z", regular_node(CONTENT)),
        (b"a", regular_node(CONTENT)),
    ]));

    let error =
        stage_nar(Cursor::new(nar), Vec::new()).expect_err("unsorted directory entries must fail");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn rejects_directory_nesting_at_nix_limit() {
    let mut node = regular_node(CONTENT);
    for _ in 0..64 {
        node = directory_node(&[(b"child", node)]);
    }
    let nar = nar_with_root(node);

    let error = stage_nar(Cursor::new(nar), Vec::new()).expect_err("Nix depth limit must fail");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
}

fn regular_nar(contents: &[u8]) -> Vec<u8> {
    let mut nar = Vec::new();
    push_string(&mut nar, NAR_MAGIC);
    push_string(&mut nar, b"(");
    push_string(&mut nar, b"type");
    push_string(&mut nar, b"regular");
    push_string(&mut nar, b"contents");
    push_string(&mut nar, contents);
    push_string(&mut nar, b")");
    nar
}

fn nar_with_root(root: Vec<u8>) -> Vec<u8> {
    let mut nar = Vec::new();
    push_string(&mut nar, NAR_MAGIC);
    nar.extend(root);
    nar
}

fn regular_node(contents: &[u8]) -> Vec<u8> {
    let mut node = Vec::new();
    push_string(&mut node, b"(");
    push_string(&mut node, b"type");
    push_string(&mut node, b"regular");
    push_string(&mut node, b"contents");
    push_string(&mut node, contents);
    push_string(&mut node, b")");
    node
}

fn executable_regular_node(contents: &[u8]) -> Vec<u8> {
    let mut node = Vec::new();
    push_string(&mut node, b"(");
    push_string(&mut node, b"type");
    push_string(&mut node, b"regular");
    push_string(&mut node, b"executable");
    push_string(&mut node, b"");
    push_string(&mut node, b"contents");
    push_string(&mut node, contents);
    push_string(&mut node, b")");
    node
}

fn symlink_node(target: &[u8]) -> Vec<u8> {
    let mut node = Vec::new();
    push_string(&mut node, b"(");
    push_string(&mut node, b"type");
    push_string(&mut node, b"symlink");
    push_string(&mut node, b"target");
    push_string(&mut node, target);
    push_string(&mut node, b")");
    node
}

fn directory_node(entries: &[(&[u8], Vec<u8>)]) -> Vec<u8> {
    let mut node = Vec::new();
    push_string(&mut node, b"(");
    push_string(&mut node, b"type");
    push_string(&mut node, b"directory");
    for (name, child) in entries {
        push_string(&mut node, b"entry");
        push_string(&mut node, b"(");
        push_string(&mut node, b"name");
        push_string(&mut node, name);
        push_string(&mut node, b"node");
        node.extend(child);
        push_string(&mut node, b")");
    }
    push_string(&mut node, b")");
    node
}

fn push_string(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&(value.len() as u64).to_le_bytes());
    output.extend_from_slice(value);
    output.resize(output.len().next_multiple_of(8), 0);
}
