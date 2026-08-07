use std::io::Cursor;

use telchar::nar::stage_nar;

const NAR_MAGIC: &[u8] = b"nix-archive-1";
const CONTENT: &[u8] = b"telchar-classic-fixture";
const CONTENT_OFFSET: usize = 96;

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

fn push_string(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&(value.len() as u64).to_le_bytes());
    output.extend_from_slice(value);
    output.resize(output.len().next_multiple_of(8), 0);
}
