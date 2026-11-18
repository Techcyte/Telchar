//! Real Nix, not CA metadata presence, decides whether an unsigned upload is exempt.
//!
//! Run under tests/canonical-store.sh. All additions and deletions target its disposable
//! canonical daemon; no build or shell-based import is used to qualify promotion.

use super::*;
use std::os::unix::fs::MetadataExt;
use telchar::store::daemon::GatewayStoreEndpoint;
use telchar::store::promotion::{GatewayStorePromotionBackend, StorePromotionBackend};

const STORE_URI: &str = "unix:///nix/var/nix/daemon-socket/socket";

fn assert_isolated_store() {
    let root = PathBuf::from(
        std::env::var_os("TELCHAR_STORE_ROOT").expect("isolated canonical store required"),
    );
    assert!(root.is_absolute() && root != Path::new("/"));
    assert_eq!(
        std::env::var_os("NIX_STATE_DIR").map(PathBuf::from),
        Some(root.join("state")),
        "store state must belong to the canonical-store fixture"
    );
    assert_eq!(
        std::env::var_os("NIX_DAEMON_SOCKET_PATH").map(PathBuf::from),
        Some(PathBuf::from("/nix/var/nix/daemon-socket/socket")),
        "Nix CLI must use the isolated canonical endpoint"
    );
    let private = fs::metadata(root.join("socket-dir/socket")).unwrap();
    let canonical = fs::metadata("/nix/var/nix/daemon-socket/socket").unwrap();
    assert!(private.file_type().is_socket() && canonical.file_type().is_socket());
    assert_eq!(
        (private.dev(), private.ino()),
        (canonical.dev(), canonical.ino()),
        "canonical socket must be the fixture's bind-mounted socket"
    );
}

fn nix(program: &str, arguments: &[&str]) -> Vec<u8> {
    // Check before *every* command, particularly add/delete: never fall back to a user store.
    assert_isolated_store();
    let output = Command::new(program)
        .args([
            "--quiet",
            "--store",
            "daemon",
            "--option",
            "builders",
            "",
            "--option",
            "post-build-hook",
            "",
            "--option",
            "substituters",
            "",
        ])
        .args(arguments)
        .output()
        .expect("isolated Nix command runs");
    assert!(
        output.status.success(),
        "{program} {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "{program} {arguments:?}: unexpected stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn hash_file(path: &Path, encoding: &str) -> String {
    String::from_utf8(nix(
        "nix",
        &[
            "--extra-experimental-features",
            "nix-command",
            "hash",
            "file",
            "--type",
            "sha256",
            encoding,
            path.to_str().unwrap(),
        ],
    ))
    .unwrap()
    .trim()
    .to_owned()
}

fn store_backend() -> GatewayStorePromotionBackend {
    assert_isolated_store();
    GatewayStorePromotionBackend::new(GatewayStoreEndpoint::parse(STORE_URI).unwrap())
}

struct Archive {
    bytes: Vec<u8>,
    hash: String,
    flat_hash: String,
}

impl Archive {
    fn from_file(root: &Path, source: &Path) -> Self {
        let bytes = nix("nix-store", &["--dump", source.to_str().unwrap()]);
        assert!(bytes.len() < 4096, "keep qualification payloads bounded");
        let nar_file = root.join("archive.nar");
        fs::write(&nar_file, &bytes).unwrap();
        Self {
            bytes,
            // Independently computed by Nix, not the upload verifier or staging implementation.
            hash: hash_file(&nar_file, "--base16"),
            flat_hash: hash_file(source, "--base32"),
        }
    }
}

struct ContentAddressedObject {
    path: String,
    address: String,
    archive: Archive,
    contents: Vec<u8>,
}

impl ContentAddressedObject {
    fn capture_and_delete(root: &Path, path: String) -> Self {
        let info = store_backend().query_path_info(Path::new(&path)).unwrap();
        assert!(info.references.is_empty());
        let archive = Archive::from_file(root, Path::new(&path));
        let expected_hash: String = info
            .nar_hash
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(archive.hash, expected_hash);
        assert_eq!(archive.bytes.len() as u64, info.nar_size);
        let contents = fs::read(&path).unwrap();
        nix("nix-store", &["--delete", &path]);
        assert!(!store_backend().is_valid_path(Path::new(&path)).unwrap());
        assert!(!Path::new(&path).exists());
        Self {
            path,
            address: info.content_address.expect("real object has a CA"),
            archive,
            contents,
        }
    }
}

struct Gateway {
    fixture: Fixture,
    stderr_reader: Option<thread::JoinHandle<Vec<u8>>>,
}

impl Gateway {
    fn start() -> Self {
        assert_isolated_store();
        let root = temporary_root();
        let socket = root.join("daemon.sock");
        let database = PostgresFixture::start();
        let mut command = daemon_command(&socket, 5000, false, database.url());
        let config = socket.with_extension("toml");
        let mut contents = fs::read_to_string(&config).unwrap();
        // Deliberately leave exempt_content_addressed unspecified: qualify its default true.
        contents.push_str("\n[upload_verification]\nenabled = true\n\n[audit]\nenabled = true\n");
        fs::write(config, contents).unwrap();
        let mut daemon = command
            .env("TELCHAR_GATEWAY_STORE_URI", STORE_URI)
            .env("RUST_LOG", "info")
            .spawn()
            .unwrap();
        wait_for_socket(&socket, &mut daemon);
        // Drain diagnostics while requests run, rather than filling the daemon's stderr pipe.
        let mut stderr = daemon.stderr.take().unwrap();
        let stderr_reader = thread::spawn(move || {
            let mut output = Vec::new();
            stderr.read_to_end(&mut output).unwrap();
            output
        });
        Self {
            stderr_reader: Some(stderr_reader),
            fixture: Fixture {
                root,
                socket,
                daemon,
                database,
            },
        }
    }

    fn upload(
        &self,
        session: &str,
        path: &str,
        archive: &Archive,
        address: Option<&str>,
        accepted: bool,
    ) {
        assert_isolated_store();
        assert!(!store_backend().is_valid_path(Path::new(path)).unwrap());
        let mut stream = UnixStream::connect(&self.fixture.socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        IpcListener::send_envelope(&mut stream, &authenticated_envelope(session)).unwrap();
        let client =
            nix_worker_protocol::WorkerClient::connect(stream.try_clone().unwrap()).unwrap();
        let mut logical = Vec::new();
        nix_worker_protocol::write_worker_integer(&mut logical, 1);
        for value in [path.as_bytes(), b"", archive.hash.as_bytes()] {
            nix_worker_protocol::write_worker_byte_string(&mut logical, value);
        }
        // No references, registration time, ultimate bit or signatures. The declared NAR
        // hash and size match even forged payloads, so transport integrity cannot mask CA checks.
        for value in [0, 0, archive.bytes.len() as u64, 0, 0] {
            nix_worker_protocol::write_worker_integer(&mut logical, value);
        }
        nix_worker_protocol::write_worker_byte_string(
            &mut logical,
            address.unwrap_or("").as_bytes(),
        );
        logical.extend_from_slice(&archive.bytes);
        let mut wire = Vec::new();
        // Request dontCheckSigs=true as a hostile client would; gateway policy still applies.
        for value in [44, 0, 1, logical.len() as u64] {
            nix_worker_protocol::write_worker_integer(&mut wire, value);
        }
        wire.extend_from_slice(&logical);
        nix_worker_protocol::write_worker_integer(&mut wire, 0);
        stream.write_all(&wire).unwrap();
        stream.flush().unwrap();
        assert_eq!(
            read_word(&mut stream),
            if accepted {
                nix_worker_protocol::STDERR_LAST
            } else {
                nix_worker_protocol::STDERR_ERROR
            },
            "{session}"
        );
        if !accepted {
            // A normal structured worker error, not EOF, timeout or a process crash.
            assert_eq!(read_string(&mut stream), b"Error");
            assert_eq!(read_word(&mut stream), 0);
            assert_eq!(read_string(&mut stream), b"Error");
            let reason = read_string(&mut stream);
            if address.is_none() {
                let reason = String::from_utf8(reason).unwrap();
                assert!(
                    reason.contains(path) && reason.contains("InsufficientSignatures"),
                    "{reason}"
                );
            } else {
                assert_eq!(reason, b"invalid AddMultipleToStore request");
            }
            assert_eq!(read_word(&mut stream), 0);
            assert_eq!(read_word(&mut stream), 0);
        }
        drop(client);
        drop(stream);
        assert_eq!(
            store_backend().is_valid_path(Path::new(path)).unwrap(),
            accepted,
            "{session}: authoritative registration"
        );
        // Nix moves a hash-valid NAR into place before registerValidPath checks the
        // CA-derived path. A wrong-path rejection can leave *unregistered* bytes on
        // disk. Validity above, not filesystem existence, is the acceptance boundary.
    }

    fn finish(&mut self) -> String {
        // Match upload_audit's bounded wait for the asynchronous audit writer.
        thread::sleep(Duration::from_millis(1200));
        self.fixture.daemon.kill().unwrap();
        self.fixture.daemon.wait().unwrap();
        let mut stdout = Vec::new();
        self.fixture
            .daemon
            .stdout
            .take()
            .unwrap()
            .read_to_end(&mut stdout)
            .unwrap();
        assert!(stdout.is_empty());
        String::from_utf8(self.stderr_reader.take().unwrap().join().unwrap()).unwrap()
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        let _ = self.fixture.daemon.kill();
        let _ = self.fixture.daemon.wait();
        if let Some(reader) = self.stderr_reader.take() {
            let _ = reader.join();
        }
        let _ = fs::remove_dir_all(&self.fixture.root);
    }
}

fn assert_decision(stderr: &str, session: &str, accepted: bool, exempt: bool) {
    let events = super::audit::audit_events(stderr);
    let uploads = events
        .iter()
        .filter(|event| {
            event["session_id"] == session
                && matches!(
                    event["event"].as_str(),
                    Some("upload.accepted" | "upload.rejected")
                )
        })
        .collect::<Vec<_>>();
    assert_eq!(uploads.len(), 1, "{session}: {stderr}");
    let event = uploads[0];
    assert_eq!(
        event["event"],
        if accepted {
            "upload.accepted"
        } else {
            "upload.rejected"
        }
    );
    assert_eq!(event["already_valid"], false);
    assert_eq!(event["signatures"], serde_json::json!([]));
    assert_eq!(
        event["verification"]["contentAddressedExemptionEligible"],
        exempt
    );
    assert_eq!(event["verification"]["allowed"], exempt);
    assert_eq!(
        event["verification"]["reason"],
        if exempt {
            "content_addressed_exemption"
        } else {
            "insufficient_signatures"
        }
    );
}

fn assert_nix_rejections(stderr: &str, count: usize) {
    // Typed daemon I/O redacts Nix's detailed path-vs-hash diagnostic. Establish that
    // the CA-eligible cases reached real AddToStoreNar, not a staging/policy rejection.
    let reasons = stderr.lines().filter(|line| {
        line.contains("worker.operation.rejected")
            && line.contains(
                "store input promotion failed: promotion failed: gateway Nix daemon AddToStoreNar was rejected"
            )
    }).count();
    assert_eq!(reasons, count, "{stderr}");
    assert!(!stderr.contains("NAR hash mismatch"), "{stderr}");
    assert!(!stderr.contains("NAR size mismatch"), "{stderr}");
}

#[test]
fn fixed_ca_forgery_cannot_bypass_client_upload_verification() {
    assert_isolated_store();
    let mut gateway = Gateway::start();
    let root = &gateway.fixture.root;
    let source = root.join("original-fixed-ca");
    fs::write(
        &source,
        format!("private fixed CA fixture {}", root.display()),
    )
    .unwrap();
    let path = String::from_utf8(nix(
        "nix",
        &[
            "--extra-experimental-features",
            "nix-command",
            "store",
            "add-file",
            source.to_str().unwrap(),
        ],
    ))
    .unwrap()
    .trim()
    .to_owned();
    let original = ContentAddressedObject::capture_and_delete(root, path);
    assert_eq!(
        original.address,
        format!("fixed:sha256:{}", original.archive.flat_hash)
    );
    let fake_file = root.join("forged-file");
    fs::write(&fake_file, b"private forged payload, never a build output").unwrap();
    let fake = Archive::from_file(root, &fake_file);
    assert_ne!(fake.flat_hash, original.archive.flat_hash);
    let fake_address = format!("fixed:sha256:{}", fake.flat_hash);
    // Same name as the real CA path, arbitrary IA-looking hash component.
    let name = original.path.rsplit_once('/').unwrap().1.get(33..).unwrap();
    let ia_path = format!("/nix/store/11111111111111111111111111111111-{name}");
    // Check hash and signature rejection before wrong-path imports can leave Nix's
    // unregistered residue at either path (LocalStore::addToStore/addValidPath).
    for (session, path, address, reject_before_move) in [
        (
            "fixed-ca-wrong-content",
            original.path.as_str(),
            Some(original.address.as_str()),
            true,
        ),
        ("ia-no-ca", ia_path.as_str(), None, true),
        (
            "fixed-ca-wrong-path",
            original.path.as_str(),
            Some(fake_address.as_str()),
            false,
        ),
        (
            "ia-forged-fixed-ca",
            ia_path.as_str(),
            Some(fake_address.as_str()),
            false,
        ),
    ] {
        gateway.upload(session, path, &fake, address, false);
        if reject_before_move {
            assert!(!Path::new(path).exists(), "{session}: rejected before move");
        } else if Path::new(path).exists() {
            // Diagnose Nix's known late path check without treating residue as registration.
            assert_eq!(
                fs::read(path).unwrap(),
                fs::read(&fake_file).unwrap(),
                "{session}"
            );
        }
    }
    // Positive control: the very same unsigned protocol request succeeds with a genuine CA.
    gateway.upload(
        "fixed-ca-valid",
        &original.path,
        &original.archive,
        Some(&original.address),
        true,
    );
    assert_eq!(fs::read(&original.path).unwrap(), original.contents);
    nix("nix-store", &["--verify-path", &original.path]);
    nix("nix-store", &["--delete", &original.path]);
    let stderr = gateway.finish();
    for session in [
        "fixed-ca-wrong-path",
        "fixed-ca-wrong-content",
        "ia-forged-fixed-ca",
    ] {
        assert_decision(&stderr, session, false, true);
    }
    assert_decision(&stderr, "ia-no-ca", false, false);
    assert_decision(&stderr, "fixed-ca-valid", true, true);
    assert_nix_rejections(&stderr, 3);
    assert!(stderr.contains("InsufficientSignatures"), "{stderr}");
}

#[test]
fn text_ca_exemption_requires_real_text_hash_and_store_path() {
    assert_isolated_store();
    let mut gateway = Gateway::start();
    let root = &gateway.fixture.root;
    let name = format!("text-ca-{}", root.file_name().unwrap().to_str().unwrap());
    let text = "private text CA fixture";
    // Existing text_content_address fixtures use builtins.toFile; this is not a derivation build.
    let expression = format!("builtins.toFile {name:?} {text:?}");
    let path: String = serde_json::from_slice(&nix(
        "nix-instantiate",
        &[
            "--eval",
            "--json",
            "--read-write-mode",
            "--expr",
            &expression,
        ],
    ))
    .unwrap();
    let original = ContentAddressedObject::capture_and_delete(root, path);
    assert_eq!(
        original.address,
        format!("text:sha256:{}", original.archive.flat_hash)
    );
    let fake_file = root.join("forged-text");
    fs::write(&fake_file, b"different private text CA contents").unwrap();
    let fake = Archive::from_file(root, &fake_file);
    assert_ne!(fake.flat_hash, original.archive.flat_hash);
    let fake_address = format!("text:sha256:{}", fake.flat_hash);
    for (session, address) in [
        ("text-ca-wrong-content", original.address.as_str()),
        ("text-ca-wrong-path", fake_address.as_str()),
    ] {
        gateway.upload(session, &original.path, &fake, Some(address), false);
        if session == "text-ca-wrong-content" {
            assert!(
                !Path::new(&original.path).exists(),
                "text hash rejected before move"
            );
        } else if Path::new(&original.path).exists() {
            assert_eq!(
                fs::read(&original.path).unwrap(),
                fs::read(&fake_file).unwrap()
            );
        }
    }
    gateway.upload(
        "text-ca-valid",
        &original.path,
        &original.archive,
        Some(&original.address),
        true,
    );
    assert_eq!(fs::read(&original.path).unwrap(), original.contents);
    nix("nix-store", &["--verify-path", &original.path]);
    nix("nix-store", &["--delete", &original.path]);
    let stderr = gateway.finish();
    for session in ["text-ca-wrong-content", "text-ca-wrong-path"] {
        assert_decision(&stderr, session, false, true);
    }
    assert_decision(&stderr, "text-ca-valid", true, true);
    assert_nix_rejections(&stderr, 2);
}
