//! Client upload verification rejects unsigned input-addressed paths before import.

use super::*;

#[test]
fn unsigned_client_upload_is_rejected_without_import() {
    assert!(std::env::var_os("TELCHAR_STORE_ROOT").is_some());
    let root = temporary_root();
    let socket = root.join("daemon.sock");
    let database = PostgresFixture::start();
    let mut command = daemon_command(&socket, 5000, true, database.url());
    let config = socket.with_extension("toml");
    let mut contents = fs::read_to_string(&config).unwrap();
    contents.push_str("\n[upload_verification]\nenabled = true\n");
    fs::write(&config, contents).unwrap();
    command.env(
        "TELCHAR_GATEWAY_STORE_URI",
        "unix:///nix/var/nix/daemon-socket/socket",
    );
    let mut daemon = command.spawn().unwrap();
    wait_for_socket(&socket, &mut daemon);
    let path = format!(
        "/nix/store/11111111111111111111111111111111-unsigned-{}",
        std::process::id()
    );
    let mut stream = UnixStream::connect(&socket).unwrap();
    IpcListener::send_envelope(&mut stream, &authenticated_envelope("unsigned-upload")).unwrap();
    let client = nix_worker_protocol::WorkerClient::connect(stream.try_clone().unwrap()).unwrap();
    let mut nar = Vec::new();
    for value in [
        b"nix-archive-1".as_slice(),
        b"(",
        b"type",
        b"regular",
        b"contents",
        b"unsigned",
        b")",
    ] {
        nix_worker_protocol::write_worker_byte_string(&mut nar, value);
    }
    let source = root.join("nar");
    fs::write(&source, &nar).unwrap();
    let output = Command::new("nix")
        .args([
            "--extra-experimental-features",
            "nix-command",
            "hash",
            "file",
            "--type",
            "sha256",
            "--base16",
        ])
        .arg(&source)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let hash = String::from_utf8(output.stdout).unwrap();
    let mut logical = Vec::new();
    nix_worker_protocol::write_worker_integer(&mut logical, 1);
    for text in [path.as_bytes(), b"", hash.trim().as_bytes()] {
        nix_worker_protocol::write_worker_byte_string(&mut logical, text);
    }
    for value in [0, 0, nar.len() as u64, 0, 0] {
        nix_worker_protocol::write_worker_integer(&mut logical, value);
    }
    nix_worker_protocol::write_worker_byte_string(&mut logical, b"");
    logical.extend_from_slice(&nar);
    let mut wire = Vec::new();
    for value in [44, 0, 1, logical.len() as u64] {
        nix_worker_protocol::write_worker_integer(&mut wire, value);
    }
    wire.extend_from_slice(&logical);
    nix_worker_protocol::write_worker_integer(&mut wire, 0);
    stream.write_all(&wire).unwrap();
    stream.flush().unwrap();
    assert_eq!(
        read_word(&mut stream),
        nix_worker_protocol::STDERR_ERROR,
        "unsigned input accepted"
    );
    assert_eq!(read_string(&mut stream), b"Error");
    assert_eq!(read_word(&mut stream), 0);
    assert_eq!(read_string(&mut stream), b"Error");
    let reason = String::from_utf8(read_string(&mut stream)).unwrap();
    assert!(
        reason.contains(&path) && reason.contains("InsufficientSignatures"),
        "{reason}"
    );
    assert_eq!(read_word(&mut stream), 0);
    assert_eq!(read_word(&mut stream), 0);
    drop(client);
    drop(stream);
    let output = daemon.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(!Path::new(&path).exists());
    fs::remove_dir_all(root).unwrap();
}
