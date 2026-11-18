//! Real live local completion writes signatures accepted by the pinned stock Nix verifier.
use super::*;

#[test]
fn live_executor_outputs_are_signed_before_success_and_verify_with_pinned_nix() {
    super::content_address_upload::assert_isolated_store();
    let root = temporary_root();
    let socket = root.join("daemon.sock");
    let database = PostgresFixture::start();
    let mut command = daemon_command(&socket, 5000, false, database.url());
    let key = root.join("key");
    let generated = nix()
        .args(["key", "generate-secret", "--key-name", "telchar-executor"])
        .output()
        .unwrap();
    assert!(generated.status.success());
    fs::write(&key, generated.stdout).unwrap();
    fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
    let public = nix()
        .args(["key", "convert-secret-to-public"])
        .stdin(fs::File::open(&key).unwrap())
        .output()
        .unwrap();
    assert!(public.status.success());
    let public = String::from_utf8(public.stdout).unwrap();
    let config = socket.with_extension("toml");
    let mut contents = fs::read_to_string(&config).unwrap();
    contents.push_str(&format!(
        "\n[signing]\nkey_file = {:?}\nfailure_policy = 'fail-build'\n",
        key
    ));
    fs::write(config, contents).unwrap();
    let roots = root.join("gc-roots");
    fs::create_dir(&roots).unwrap();
    command
        .env(
            "TELCHAR_GATEWAY_STORE_URI",
            "unix:///nix/var/nix/daemon-socket/socket",
        )
        .env("TELCHAR_GATEWAY_GC_ROOT_DIRECTORY", &roots);
    let mut daemon = command.spawn().unwrap();
    wait_for_socket(&socket, &mut daemon);
    let expression = format!(
        "derivation {{ name = \"telchar-signing-{}\"; system = builtins.currentSystem; builder = \"/bin/sh\"; args = [ \"-c\" \"printf signed > $out\" ]; }}",
        std::process::id()
    );
    let instantiated = Command::new("nix-instantiate")
        .args(["--expr", &expression])
        .output()
        .unwrap();
    assert!(
        instantiated.status.success(),
        "{}",
        String::from_utf8_lossy(&instantiated.stderr)
    );
    let drv = String::from_utf8(instantiated.stdout).unwrap();
    let queried = Command::new("nix-store")
        .args(["-q", "--outputs", drv.trim()])
        .output()
        .unwrap();
    assert!(queried.status.success());
    let output = String::from_utf8(queried.stdout).unwrap().trim().to_owned();
    let version = nix().arg("--version").output().unwrap();
    eprintln!(
        "real signing/verification: {}",
        String::from_utf8_lossy(&version.stdout).trim()
    );
    for index in 0..2 {
        let mut stream = UnixStream::connect(&socket).unwrap();
        IpcListener::send_envelope(
            &mut stream,
            &authenticated_envelope(&format!("signing-{index}")),
        )
        .unwrap();
        let mut client = nix_worker_protocol::WorkerClient::connect(stream).unwrap();
        client
            .build_paths_with_results(&[format!("{}!*", drv.trim()).into_bytes()])
            .unwrap();
        let state = telchar::persistence::read_shared_build(database.url(), drv.trim())
            .unwrap()
            .unwrap();
        assert_eq!(
            state.state,
            telchar::persistence::SharedBuildState::Succeeded
        );
        let endpoint =
            telchar::store::GatewayStoreEndpoint::parse("unix:///nix/var/nix/daemon-socket/socket")
                .unwrap();
        let info = telchar::store::GatewayStoreConnection::connect(&endpoint)
            .unwrap()
            .query_path_info(output.as_bytes())
            .unwrap()
            .unwrap();
        assert_eq!(info.signatures().len(), 1);
        assert!(info.signatures()[0].starts_with(b"telchar-executor:"));
        let verified = nix()
            .args([
                "store",
                "verify",
                "--sigs-needed",
                "1",
                "--trusted-public-keys",
                public.trim(),
                &output,
            ])
            .output()
            .unwrap();
        assert!(
            verified.status.success(),
            "{}",
            String::from_utf8_lossy(&verified.stderr)
        );
        // Independent stock signer is deterministic and additive: same bytes, no second signature.
        let signed = nix()
            .args([
                "store",
                "sign",
                "--key-file",
                key.to_str().unwrap(),
                &output,
            ])
            .output()
            .unwrap();
        assert!(
            signed.status.success(),
            "{}",
            String::from_utf8_lossy(&signed.stderr)
        );
        let after = telchar::store::GatewayStoreConnection::connect(&endpoint)
            .unwrap()
            .query_path_info(output.as_bytes())
            .unwrap()
            .unwrap();
        assert_eq!(info.signatures(), after.signatures());
    }
    // A separately realized gateway-only result must remain unsigned, even with
    // the same AlreadyValid client result status as an assigned executor hit.
    let cached_expression = expression.replace("telchar-signing-", "telchar-gateway-reuse-");
    let cached_drv = Command::new("nix-instantiate")
        .args(["--expr", &cached_expression])
        .output()
        .unwrap();
    assert!(cached_drv.status.success());
    let cached_drv = String::from_utf8(cached_drv.stdout).unwrap();
    let cached_output = Command::new("nix-store")
        .args(["--realise", cached_drv.trim()])
        .output()
        .unwrap();
    assert!(cached_output.status.success());
    let cached_output = String::from_utf8(cached_output.stdout).unwrap();
    let mut stream = UnixStream::connect(&socket).unwrap();
    IpcListener::send_envelope(&mut stream, &authenticated_envelope("gateway-only-reuse")).unwrap();
    let mut client = nix_worker_protocol::WorkerClient::connect(stream).unwrap();
    client
        .build_paths_with_results(&[format!("{}!*", cached_drv.trim()).into_bytes()])
        .unwrap();
    drop(client);
    let endpoint =
        telchar::store::GatewayStoreEndpoint::parse("unix:///nix/var/nix/daemon-socket/socket")
            .unwrap();
    assert!(
        telchar::store::GatewayStoreConnection::connect(&endpoint)
            .unwrap()
            .query_path_info(cached_output.trim().as_bytes())
            .unwrap()
            .unwrap()
            .signatures()
            .is_empty(),
        "gateway-only reuse must not acquire executor signature"
    );
    assert_eq!(
        telchar::persistence::read_shared_build(database.url(), cached_drv.trim())
            .unwrap()
            .unwrap()
            .result_metadata
            .unwrap()["status"],
        "already-valid"
    );
    // Retain the actual executor signature, fingerprint and NAR before resetting only
    // this path. This qualifies sequential producer/receiver operation in one isolated
    // canonical store, not simultaneous gateways or a stock-client SSH upload.
    let signed_info = super::signed_upload::path_info(&root, &output);
    assert!(signed_info["ca"].is_null());
    assert_eq!(signed_info["references"], serde_json::json!([]));
    let exported = super::signed_upload::nix_stdout(&root, &["store", "dump-path", &output]);
    assert!(exported.len() < 4096);
    let mut upload = super::signed_upload::Upload {
        path: output.clone(),
        hash_hex: super::signed_upload::hash_hex(&root, signed_info["narHash"].as_str().unwrap()),
        nar_size: signed_info["narSize"].as_u64().unwrap(),
        signatures: Vec::new(),
    };
    assert_eq!(upload.nar_size, exported.len() as u64);
    let executor_signatures = signed_info["signatures"]
        .as_array()
        .unwrap()
        .iter()
        .map(|signature| signature.as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(executor_signatures.len(), 1);

    // An independently generated developer key can sign the same bytes but is not
    // trusted by the receiving gateway. Obtain that signature from real Nix too.
    let developer_key = root.join("developer-key");
    let generated = nix()
        .args(["key", "generate-secret", "--key-name", "developer"])
        .output()
        .unwrap();
    assert!(generated.status.success());
    fs::write(&developer_key, generated.stdout).unwrap();
    fs::set_permissions(&developer_key, fs::Permissions::from_mode(0o600)).unwrap();
    let signed = nix()
        .args(["store", "sign", "--key-file"])
        .arg(&developer_key)
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        signed.status.success(),
        "{}",
        String::from_utf8_lossy(&signed.stderr)
    );
    let developer_signatures = super::signed_upload::path_info(&root, &output)["signatures"]
        .as_array()
        .unwrap()
        .iter()
        .map(|signature| signature.as_str().unwrap().to_owned())
        .filter(|signature| signature.starts_with("developer:"))
        .collect::<Vec<_>>();
    assert_eq!(developer_signatures.len(), 1);

    daemon.kill().unwrap();
    let result = daemon.wait_with_output().unwrap();
    assert!(
        !String::from_utf8_lossy(&result.stderr).contains("executor.output_signing.failed"),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    super::content_address_upload::assert_isolated_store();
    // The exact producer is stopped. Release only roots in this test's own directory;
    // nix-store --delete must succeed without force or unrelated garbage collection.
    fs::remove_dir_all(&roots).unwrap();
    let deleted = Command::new("nix-store")
        .args(["--store", "daemon", "--delete", &output])
        .output()
        .unwrap();
    assert!(
        deleted.status.success(),
        "{}",
        String::from_utf8_lossy(&deleted.stderr)
    );
    assert!(!Path::new(&output).exists());
    assert!(
        telchar::store::GatewayStoreConnection::connect(&endpoint)
            .unwrap()
            .query_path_info(output.as_bytes())
            .unwrap()
            .is_none(),
        "receiver must start with an unregistered output"
    );

    let receiver = root.join("receiver");
    fs::create_dir(&receiver).unwrap();
    fs::set_permissions(&receiver, fs::Permissions::from_mode(0o700)).unwrap();
    // Enable signing on the receiver as well, to detect accidental signing of uploads.
    fs::copy(&key, receiver.join("executor-signing-key")).unwrap();
    let receiver_database = PostgresFixture::start();
    for (session, signatures) in [
        ("executor-upload-unsigned", Vec::new()),
        ("executor-upload-developer", developer_signatures),
        ("executor-upload-trusted", executor_signatures.clone()),
    ] {
        super::content_address_upload::assert_isolated_store();
        assert!(
            telchar::store::GatewayStoreConnection::connect(&endpoint)
                .unwrap()
                .query_path_info(output.as_bytes())
                .unwrap()
                .is_none()
        );
        upload.signatures = signatures;
        let event = super::signed_upload::upload_once(
            &receiver,
            &receiver_database,
            session,
            Some(public.trim()),
            &upload,
            &exported,
        );
        assert_eq!(event["already_valid"], false);
        if session == "executor-upload-trusted" {
            assert_eq!(event["event"], "upload.accepted");
            assert_eq!(event["verification"]["validSignatures"], 1);
        } else {
            assert_eq!(event["event"], "upload.rejected");
            assert_eq!(event["verification"]["reason"], "insufficient_signatures");
            assert_eq!(event["verification"]["validSignatures"], 0);
        }
    }
    let received = super::signed_upload::path_info(&receiver, &output);
    assert_eq!(received["narHash"], signed_info["narHash"]);
    assert_eq!(received["signatures"], signed_info["signatures"]);
    assert_eq!(fs::read(&output).unwrap(), b"signed");

    // Even with a signing key configured, an accepted unsigned client upload must
    // stay unsigned. Disabling upload verification is not an executor provenance grant.
    upload.path = format!(
        "/nix/store/11111111111111111111111111111111-client-upload-{}",
        std::process::id()
    );
    upload.signatures.clear();
    assert!(!Path::new(&upload.path).exists());
    let event = super::signed_upload::upload_once(
        &receiver,
        &receiver_database,
        "client-upload-not-signed",
        None,
        &upload,
        &exported,
    );
    assert_eq!(event["event"], "upload.accepted");
    assert_eq!(event["already_valid"], false);
    assert_eq!(
        super::signed_upload::path_info(&receiver, &upload.path)["signatures"],
        serde_json::json!([])
    );
    fs::remove_dir_all(root).unwrap();
}

fn nix() -> Command {
    let mut command = Command::new("nix");
    command.args(["--extra-experimental-features", "nix-command"]);
    command
}

#[test]
fn malformed_protected_signing_key_fails_startup_before_admission_without_secret_diagnostic() {
    let root = temporary_root();
    let socket = root.join("daemon.sock");
    let mut command = daemon_command_without_database(&socket, 5000, false);
    let key = root.join("key");
    fs::write(&key, "private-signing-key-sentinel").unwrap();
    fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
    let config = socket.with_extension("toml");
    let mut contents = fs::read_to_string(&config).unwrap();
    contents.push_str(&format!("\n[signing]\nkey_file = {:?}\n", key));
    fs::write(config, contents).unwrap();
    command.env("TELCHAR_GATEWAY_STORE_URI", "unix:///missing-store.sock");
    let output = command.output().unwrap();
    assert!(!output.status.success());
    assert!(!socket.exists());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("signing key file is invalid"), "{stderr}");
    assert!(!stderr.contains("private-signing-key-sentinel"));
    assert!(!stderr.contains("database.migration.started"));
    fs::remove_dir_all(root).unwrap();
}
