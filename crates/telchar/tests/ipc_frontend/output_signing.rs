//! Real live local completion writes signatures accepted by the pinned stock Nix verifier.
use super::*;

#[test]
fn live_executor_outputs_are_signed_before_success_and_verify_with_pinned_nix() {
    assert!(
        std::env::var_os("TELCHAR_STORE_ROOT").is_some(),
        "isolated canonical store required"
    );
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
    daemon.kill().unwrap();
    let result = daemon.wait_with_output().unwrap();
    assert!(
        !String::from_utf8_lossy(&result.stderr).contains("executor.output_signing.failed"),
        "{}",
        String::from_utf8_lossy(&result.stderr)
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
