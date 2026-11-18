//! Real Nix signatures qualify client reuploads, independently of staged NAR integrity.

use super::*;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

const STORE_URI: &str = "unix:///nix/var/nix/daemon-socket/socket";
const CONTENTS: &[u8] = b"signed-upload-original-content";

struct PrivateRoot(PathBuf);

impl Drop for PrivateRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct UploadDaemon(Child);

impl Drop for UploadDaemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Upload {
    path: String,
    hash_hex: String,
    nar_size: u64,
    signatures: Vec<String>,
}

fn nix_command(root: &Path) -> Command {
    let mut command = Command::new("nix");
    command
        .args([
            "--extra-experimental-features",
            "nix-command",
            "--store",
            STORE_URI,
            "--offline",
        ])
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("NIX_CONF_DIR", root.join("conf"))
        .env("NIX_USER_CONF_FILES", "/dev/null")
        .env("NIX_CONFIG", "substituters =\nbuilders =\n")
        .stdin(Stdio::null());
    command
}

fn nix_stdout(root: &Path, arguments: &[&str]) -> Vec<u8> {
    let output = nix_command(root).args(arguments).output().unwrap();
    assert!(
        output.status.success(),
        "Nix qualification command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "unexpected Nix diagnostics: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn path_info(root: &Path, path: &str) -> serde_json::Value {
    let output = nix_stdout(
        root,
        &["path-info", "--json", "--json-format", "1", "--sigs", path],
    );
    assert!(
        output.len() <= 64 * 1024,
        "path-info response exceeds bound"
    );
    let entries: serde_json::Value = serde_json::from_slice(&output).unwrap();
    let info = entries.get(path).expect("path-info contains the upload");
    assert!(info.is_object(), "uploaded path is registered");
    info.clone()
}

fn hash_hex(root: &Path, hash: &str) -> String {
    String::from_utf8(nix_stdout(
        root,
        &["hash", "convert", "--to", "base16", hash],
    ))
    .unwrap()
    .trim()
    .to_owned()
}

fn upload_once(
    root: &Path,
    database: &PostgresFixture,
    session: &str,
    public_key: Option<&str>,
    upload: &Upload,
    nar: &[u8],
) -> serde_json::Value {
    let socket = root.join("daemon.sock");
    // Each request uses the same database and store, but a fresh configuration snapshot.
    if socket.with_extension("toml").exists() {
        database.expire_singleton_ownership("daemon");
    }
    if socket.exists() {
        fs::remove_file(&socket).unwrap();
    }
    let mut command = daemon_command(&socket, 5000, true, database.url());
    let config = socket.with_extension("toml");
    let mut contents = fs::read_to_string(&config).unwrap();
    contents.push_str("\n[audit]\nenabled = true\n");
    if let Some(public_key) = public_key {
        contents.push_str(&format!(
            "\n[upload_verification]\nenabled = true\nmin_signatures = 1\ntrusted_public_keys = [{public_key:?}]\n"
        ));
    }
    fs::write(&config, contents).unwrap();
    command.env("TELCHAR_GATEWAY_STORE_URI", STORE_URI);
    let mut daemon = UploadDaemon(command.spawn().unwrap());
    wait_for_socket(&socket, &mut daemon.0);
    let mut stream = UnixStream::connect(&socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    IpcListener::send_envelope(&mut stream, &authenticated_envelope(session)).unwrap();
    let client = nix_worker_protocol::WorkerClient::connect(stream.try_clone().unwrap()).unwrap();

    let mut logical = Vec::new();
    nix_worker_protocol::write_worker_integer(&mut logical, 1);
    for value in [upload.path.as_bytes(), b"", upload.hash_hex.as_bytes()] {
        nix_worker_protocol::write_worker_byte_string(&mut logical, value);
    }
    // No references, registration time, NAR size, not ultimate, then signatures.
    for value in [0, 0, upload.nar_size, 0, upload.signatures.len() as u64] {
        nix_worker_protocol::write_worker_integer(&mut logical, value);
    }
    for signature in &upload.signatures {
        nix_worker_protocol::write_worker_byte_string(&mut logical, signature.as_bytes());
    }
    // No content address: this fixture is deliberately input-addressed, not CA-exempt.
    nix_worker_protocol::write_worker_byte_string(&mut logical, b"");
    logical.extend_from_slice(nar);
    let mut wire = Vec::new();
    // AddMultipleToStore, repair=false, dontCheckSigs=true, one bounded frame.
    for value in [44, 0, 1, logical.len() as u64] {
        nix_worker_protocol::write_worker_integer(&mut wire, value);
    }
    wire.extend_from_slice(&logical);
    nix_worker_protocol::write_worker_integer(&mut wire, 0);
    stream.write_all(&wire).unwrap();
    stream.flush().unwrap();
    let wire_result = read_word(&mut stream);
    drop(client);
    drop(stream);
    let output = wait_with_deadline(&mut daemon.0, Duration::from_secs(10));
    assert!(output.status.success(), "upload daemon failed: {}", String::from_utf8_lossy(&output.stderr));
    let stderr = String::from_utf8(output.stderr).unwrap();
    let events = super::audit::audit_events(&stderr);
    let uploads = events
        .into_iter()
        .filter(|event| {
            event["session_id"] == session
                && event["store_path"] == upload.path
                && matches!(
                    event["event"].as_str(),
                    Some("upload.accepted" | "upload.rejected")
                )
        })
        .collect::<Vec<_>>();
    assert_eq!(uploads.len(), 1, "one per-path upload decision: {stderr}");
    let event = uploads.into_iter().next().unwrap();
    assert_eq!(
        wire_result,
        if event["event"] == "upload.accepted" {
            nix_worker_protocol::STDERR_LAST
        } else {
            nix_worker_protocol::STDERR_ERROR
        },
        "wire result agrees with per-path audit for {session}"
    );
    event
}

#[test]
fn nix_signed_reupload_requires_signatures_and_matching_staged_nar() {
    assert!(
        std::env::var_os("TELCHAR_STORE_ROOT").is_some(),
        "isolated canonical store required; run under tests/canonical-store.sh"
    );
    let root = PrivateRoot(temporary_root());
    fs::DirBuilder::new().mode(0o700).create(&root.0).unwrap();
    for directory in ["home", "config", "cache", "conf"] {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(root.0.join(directory))
            .unwrap();
    }
    let database = PostgresFixture::start();
    let path = format!(
        "/nix/store/11111111111111111111111111111111-signed-{}",
        root.0.file_name().unwrap().to_str().unwrap()
    );
    assert!(!Path::new(&path).exists(), "upload path must be fresh");
    let mut nar = Vec::new();
    for value in [
        b"nix-archive-1".as_slice(),
        b"(",
        b"type",
        b"regular",
        b"contents",
        CONTENTS,
        b")",
    ] {
        nix_worker_protocol::write_worker_byte_string(&mut nar, value);
    }
    let nar_file = root.0.join("original.nar");
    fs::write(&nar_file, &nar).unwrap();
    let original_hash = String::from_utf8(nix_stdout(
        &root.0,
        &[
            "hash",
            "file",
            "--type",
            "sha256",
            "--base16",
            nar_file.to_str().unwrap(),
        ],
    ))
    .unwrap()
    .trim()
    .to_owned();
    let mut upload = Upload {
        path,
        hash_hex: original_hash,
        nar_size: nar.len() as u64,
        signatures: Vec::new(),
    };
    let seed = upload_once(
        &root.0,
        &database,
        "signed-upload-seed",
        None,
        &upload,
        &nar,
    );
    assert_eq!(seed["event"], "upload.accepted");
    assert_eq!(seed["already_valid"], false);
    assert!(seed["verification"].is_null());
    let unsigned_info = path_info(&root.0, &upload.path);
    assert!(unsigned_info["ca"].is_null());
    assert_eq!(unsigned_info["references"], serde_json::json!([]));
    assert_eq!(unsigned_info["signatures"], serde_json::json!([]));

    let secret_path = root.0.join("signing-key");
    let secret = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&secret_path)
        .unwrap();
    let status = nix_command(&root.0)
        .args([
            "key",
            "generate-secret",
            "--key-name",
            "telchar-upload-test-1",
        ])
        .stderr(Stdio::null())
        .stdout(Stdio::from(secret))
        .status()
        .unwrap();
    assert!(status.success(), "Nix secret-key generation failed");
    assert_eq!(
        fs::metadata(&secret_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let public = nix_command(&root.0)
        .args(["key", "convert-secret-to-public"])
        .stderr(Stdio::null())
        .stdin(Stdio::from(fs::File::open(&secret_path).unwrap()))
        .output()
        .unwrap();
    assert!(public.status.success(), "Nix public-key conversion failed");
    let public_key = String::from_utf8(public.stdout).unwrap();
    let public_key = public_key.trim();
    assert!(public_key.starts_with("telchar-upload-test-1:"));
    let status = nix_command(&root.0)
        .args(["store", "sign", "-k"])
        .arg(&secret_path)
        .arg(&upload.path)
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "Nix store signing failed");
    fs::remove_file(&secret_path).unwrap();

    // Read the signature and fingerprint fields back from Nix, not a test signer.
    let signed_info = path_info(&root.0, &upload.path);
    assert!(signed_info["ca"].is_null());
    assert_eq!(signed_info["references"], serde_json::json!([]));
    upload.hash_hex = hash_hex(&root.0, signed_info["narHash"].as_str().unwrap());
    upload.nar_size = signed_info["narSize"].as_u64().unwrap();
    upload.signatures = signed_info["signatures"]
        .as_array()
        .unwrap()
        .iter()
        .map(|signature| signature.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(upload.signatures.len(), 1);
    assert!(upload.signatures[0].starts_with("telchar-upload-test-1:"));
    assert_eq!(upload.hash_hex, seed["nar_hash"].as_str().unwrap());
    assert_eq!(upload.nar_size, nar.len() as u64);
    let exported = nix_stdout(&root.0, &["store", "dump-path", &upload.path]);
    assert_eq!(exported, nar, "Nix exports the imported NAR unchanged");

    let signed = upload_once(
        &root.0,
        &database,
        "signed-upload-verified",
        Some(public_key),
        &upload,
        &exported,
    );
    assert_eq!(signed["event"], "upload.accepted");
    assert_eq!(signed["already_valid"], true);
    assert_eq!(signed["verification"]["reason"], "signatures_verified");
    assert_eq!(signed["verification"]["validSignatures"], 1);
    assert_eq!(signed["verification"]["signatures"][0]["status"], "valid");

    let signatures = std::mem::take(&mut upload.signatures);
    let unsigned = upload_once(
        &root.0,
        &database,
        "signed-upload-unsigned-reupload",
        Some(public_key),
        &upload,
        &exported,
    );
    assert_eq!(unsigned["event"], "upload.rejected");
    assert_eq!(unsigned["already_valid"], true);
    assert_eq!(unsigned["verification"]["allowed"], false);
    assert_eq!(
        unsigned["verification"]["reason"],
        "insufficient_signatures"
    );
    assert_eq!(unsigned["verification"]["validSignatures"], 0);
    upload.signatures = signatures;

    // Preserve a valid NAR shape and length, but alter file contents without changing
    // the signed hash/size/path. A valid metadata signature must not bypass staging.
    let mut altered = exported.clone();
    let offset = altered
        .windows(CONTENTS.len())
        .position(|window| window == CONTENTS)
        .unwrap();
    altered[offset] ^= 1;
    let mismatch = upload_once(
        &root.0,
        &database,
        "signed-upload-altered-nar",
        Some(public_key),
        &upload,
        &altered,
    );
    assert_eq!(mismatch["event"], "upload.rejected");
    assert_eq!(mismatch["already_valid"], true);
    assert_eq!(mismatch["verification"]["allowed"], true);
    assert_eq!(mismatch["verification"]["validSignatures"], 1);
    assert_eq!(mismatch["reason"], "NAR hash mismatch");
    assert_eq!(fs::read(&upload.path).unwrap(), CONTENTS);
    let final_info = path_info(&root.0, &upload.path);
    assert_eq!(final_info["narHash"], signed_info["narHash"]);
    assert_eq!(final_info["signatures"], signed_info["signatures"]);
}
