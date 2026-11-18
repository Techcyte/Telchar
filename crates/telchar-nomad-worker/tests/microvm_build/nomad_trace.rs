//! Connected Nomad tracing: scheduler allocation API is simulated; rendered task Env,
//! real worker process, authenticated callback, Nix build and gateway import are not.
use super::*;
use std::io::{Read, Write};
use std::time::Instant;
use telchar::backend::{BackendKind, BackendTarget};
use telchar::nomad::backend::render_job_with_trace_context;
use telchar::persistence as db;
use telchar::service::config::ServiceConfig;

const TEST: &str = "nomad_trace::rendered_environment_connects_real_worker_and_callback";

#[test]
fn rendered_environment_connects_real_worker_and_callback() {
    if let Some(path) = std::env::var_os("SIGNING_NOMAD_SEED") {
        seed_persistent_worker_store(std::path::Path::new(&path));
        return;
    }
    if std::env::var_os("MICROVM_TEST_STORE_URI").is_none() {
        for case in ["success", "warn", "fail-build", "already-valid"] {
            let database = telchar_test_support::postgres::PostgresFixture::start();
            let status = Command::new("bash")
                .env("MICROVM_TEST_DATABASE", database.url())
                .env("SIGNING_NOMAD_CASE", case)
                .arg(FIXTURE)
                .arg(std::env::current_exe().unwrap())
                .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
                .status()
                .unwrap();
            assert!(status.success(), "Nomad signing case {case}");
        }
        return;
    }
    let case = std::env::var("SIGNING_NOMAD_CASE").unwrap_or_else(|_| "success".to_owned());
    let signing_failure_policy = if case == "warn" { "warn" } else { "fail-build" };
    let collector = collector::start_collector();
    unsafe {
        std::env::set_var("OTEL_EXPORTER_OTLP_ENDPOINT", collector.endpoint());
        std::env::set_var("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc");
    }
    let telemetry = telchar_telemetry::Telemetry::initialize("telchar", "test").unwrap();
    let root = std::path::PathBuf::from(std::env::var_os("MICROVM_STORE_ROOT").unwrap());
    let database = std::env::var("MICROVM_TEST_DATABASE").unwrap();
    db::migrate(&database).unwrap();
    let callback = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let callback_address = callback.local_addr().unwrap();
    let scheduler = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let scheduler_address = scheduler.local_addr().unwrap();
    let secret = root.join("nomad-secret");
    std::fs::write(&secret, "private-nomad-trace-secret").unwrap();
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
    let store_uri = std::env::var("MICROVM_TEST_STORE_URI").unwrap();
    // Stable operator-selected endpoint, resolved to the private worker daemon by
    // the fake raw_exec launcher; the rendered Env itself is consumed unchanged.
    let worker_socket = root.join("nomad-worker.sock");
    let worker_store_uri = format!("unix://{}", worker_socket.display());
    let signing_key = root.join("executor-signing-key");
    let generated = Command::new("nix")
        .args([
            "--extra-experimental-features",
            "nix-command",
            "key",
            "generate-secret",
            "--key-name",
            "nomad-executor",
        ])
        .output()
        .unwrap();
    assert!(generated.status.success());
    std::fs::write(&signing_key, generated.stdout).unwrap();
    std::fs::set_permissions(&signing_key, std::fs::Permissions::from_mode(0o600)).unwrap();
    let config_path = root.join("nomad.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"
[signing]
key_file = "{signing_key}"
failure_policy = "{signing_failure_policy}"

[backends.nomad_callback]
bind = "{callback_address}"
public_url = "ws://{callback_address}/callback"

[[backends.nomad]]
[backends.nomad.trace-test]
system = "x86_64-linux"
maximum_concurrent_builds = 1
endpoint = "http://{scheduler_address}"
namespace = "telchar"
driver = "raw_exec"
job_name_scope = "trace-test"
poll_interval_seconds = 1
runtime_limit_seconds = 60
transfer_endpoint = "ws://{callback_address}/callback"

[backends.nomad.trace-test.driver_config]
command = "/bin/true"
[backends.nomad.trace-test.resources]
cpu_mhz = 100
memory_mb = 128
disk_mb = 128
[backends.nomad.trace-test.transfer_authentication]
mode = "hmac"
key_id = "trace-test"
secret_file = "{}"
[backends.nomad.trace-test.store]
mode = "daemon"
uri = "{worker_store_uri}"
"#,
            secret.display(),
            signing_key = signing_key.display()
        ),
    )
    .unwrap();
    let config = ServiceConfig::load_from_default(&config_path).unwrap();
    let backend = &config.nomad_backends()[0];
    let output = "/nix/store/00000000000000000000000000000000-nomad-trace-output";
    let drv = "/nix/store/00000000000000000000000000000000-nomad-trace.drv";
    let source = root.join("nomad-trace-input");
    std::fs::write(&source, "connected-nomad").unwrap();
    let added = Command::new("nix-store")
        .args(["--store", "local", "--add"])
        .arg(&source)
        .output()
        .unwrap();
    assert!(added.status.success());
    let input = String::from_utf8(added.stdout).unwrap().trim().to_owned();
    let build_marker = root.join("nomad-builder-invocations");
    let script = serde_json::to_string(&format!(
        "printf x >> '{}'; printf connected-nomad > $out",
        build_marker.display()
    ))
    .unwrap();
    let derivation = format!(
        r#"Derive([("out","{output}","","")],[],["{input}"],"x86_64-linux","/bin/sh",["-e","-c",{script}],[("out","{output}"),("system","x86_64-linux"),("builder","/bin/sh"),("name","nomad-trace")])"#
    );
    let request = telchar::build::BuildRequest::from_stored_derivation(
        drv.as_bytes(),
        derivation.as_bytes(),
        &[BackendTarget::new(
            "trace-test",
            BackendKind::Nomad,
            "x86_64-linux",
            [] as [&str; 0],
        )
        .unwrap()],
    )
    .unwrap();
    let seed_request = root.join("seed-request.json");
    let seed_input = root.join("seed-input.export");
    if case == "already-valid" {
        std::fs::write(&seed_request, serde_json::to_vec(&request).unwrap()).unwrap();
        let exported = Command::new("nix-store")
            .args(["--export", &input])
            .output()
            .unwrap();
        assert!(exported.status.success());
        std::fs::write(&seed_input, exported.stdout).unwrap();
    }
    let parent = tracing::info_span!(parent: None, "backend.execution");
    let entered = parent.enter();
    let context = telchar_telemetry::TraceContext::capture_current();
    let job =
        render_job_with_trace_context(backend, request.shared_build_key().as_bytes(), &context)
            .unwrap();
    let job_id = job["Job"]["ID"].as_str().unwrap().to_owned();
    db::claim_shared_build_with_request(
        &database,
        drv,
        &request.shared_build_digest(),
        "trace-test",
        BackendKind::Nomad,
        BackendKind::Nomad.capabilities(),
        Some(&job_id),
        &[output],
        &request,
    )
    .unwrap();
    db::start_shared_build(&database, drv).unwrap();
    db::record_shared_build_attempt_trace_context(&database, drv, &job_id, &context).unwrap();
    let scheduler_job = job_id.clone();
    let scheduler_thread = std::thread::spawn(move || {
        scheduler.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let (mut stream, _) = loop {
            match scheduler.accept() {
                Ok(connection) => break connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "allocation verification deadline"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("scheduler fixture: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            headers.push(byte[0]);
            assert!(headers.len() < 16384);
        }
        assert!(
            String::from_utf8(headers)
                .unwrap()
                .starts_with("GET /v1/allocation/trace-allocation?namespace=telchar ")
        );
        let body = serde_json::json!({"ID":"trace-allocation", "Namespace":"telchar", "JobID":scheduler_job,
            "TaskGroup":"build", "ClientStatus":"running", "TaskStates":{"build":{"State":"running"}}}).to_string();
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    });
    let endpoint = GatewayStoreEndpoint::parse(&store_uri).unwrap();
    let mut callback_service = telchar::nomad::callback_service::NomadCallbackService::start(
        callback,
        config.nomad_callback().unwrap().clone(),
        db::Database::connect(&database).unwrap(),
        config.nomad_backends().to_vec(),
        endpoint.clone(),
        Duration::from_secs(60),
        Arc::new(telchar::shared_build::SharedBuildRegistry::new()),
        Some(
            config
                .signing()
                .unwrap()
                .load(if matches!(case.as_str(), "success" | "already-valid") {
                    endpoint.clone()
                } else {
                    GatewayStoreEndpoint::parse("unix:///missing-nomad-signing-store.sock").unwrap()
                })
                .unwrap(),
        ),
    )
    .unwrap();
    let env = job["Job"]["TaskGroups"][0]["Tasks"][0]["Env"]
        .as_object()
        .unwrap();
    let mut worker = Command::new("bash");
    let launcher = if case == "already-valid" {
        "set -eu; ln -s \"$NIX_DAEMON_SOCKET_PATH\" \"$2\"; nix-store --import < \"$6\"; SIGNING_NOMAD_SEED=\"$3\" \"$4\" --exact \"$5\" --nocapture --test-threads=1; exec \"$1\""
    } else {
        "ln -s \"$NIX_DAEMON_SOCKET_PATH\" \"$2\"; exec \"$1\""
    };
    worker
        .arg(FIXTURE)
        .arg("/bin/sh")
        .args(["-c", launcher, "nomad-worker"])
        .arg(env!("CARGO_BIN_EXE_telchar-nomad-worker"))
        .arg(&worker_socket)
        .arg(&seed_request)
        .arg(std::env::current_exe().unwrap())
        .arg(TEST)
        .arg(&seed_input)
        .envs(
            env.iter()
                .map(|(name, value)| (name, value.as_str().unwrap())),
        )
        .env("NOMAD_ALLOC_ID", "trace-allocation")
        .env("NOMAD_JOB_ID", &job_id)
        .env("NOMAD_NAMESPACE", "telchar")
        .env("NOMAD_TASK_NAME", "build");
    assert!(
        GatewayStoreConnection::connect(&endpoint)
            .unwrap()
            .query_path_info(output.as_bytes())
            .unwrap()
            .is_none(),
        "gateway output absent before assigned worker transfer"
    );
    let result = worker.output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    if case == "already-valid" {
        let seed_output = String::from_utf8_lossy(&result.stderr);
        assert!(
            seed_output.contains("persistent Nomad worker store raw status: AlreadyValid"),
            "{seed_output}"
        );
        eprintln!(
            "persistent Nomad worker store Built -> AlreadyValid confirmed before real worker transfer"
        );
    }
    scheduler_thread.join().unwrap();
    callback_service.shutdown().unwrap();
    let mut store = GatewayStoreConnection::connect(&endpoint).unwrap();
    assert!(store.is_valid_path(output.as_bytes()).unwrap());
    let info = store.query_path_info(output.as_bytes()).unwrap().unwrap();
    if matches!(case.as_str(), "success" | "already-valid") {
        assert_eq!(info.signatures().len(), 1);
        assert!(info.signatures()[0].starts_with(b"nomad-executor:"));
        let public = Command::new("nix")
            .args([
                "--extra-experimental-features",
                "nix-command",
                "key",
                "convert-secret-to-public",
            ])
            .stdin(std::fs::File::open(&signing_key).unwrap())
            .output()
            .unwrap();
        assert!(public.status.success());
        let public = String::from_utf8(public.stdout).unwrap();
        let verified = Command::new("nix")
            .args([
                "--extra-experimental-features",
                "nix-command",
                "store",
                "verify",
                "--sigs-needed",
                "1",
                "--trusted-public-keys",
                public.trim(),
                output,
            ])
            .output()
            .unwrap();
        assert!(
            verified.status.success(),
            "{}",
            String::from_utf8_lossy(&verified.stderr)
        );
    } else {
        assert!(info.signatures().is_empty());
    }
    assert!(
        store
            .query_path_info(input.as_bytes())
            .unwrap()
            .unwrap()
            .signatures()
            .is_empty(),
        "inputs must not be signed"
    );
    assert_eq!(std::fs::read(output).unwrap(), b"connected-nomad");
    assert_eq!(
        std::fs::read(&build_marker).unwrap(),
        b"x",
        "assigned cache hit must not rerun builder after seeding"
    );

    assert_eq!(
        db::read_shared_build(&database, drv)
            .unwrap()
            .unwrap()
            .state,
        if case == "fail-build" {
            db::SharedBuildState::Failed
        } else {
            db::SharedBuildState::Succeeded
        }
    );
    let attempt = db::read_shared_build_attempt(&database, drv)
        .unwrap()
        .unwrap();
    assert_eq!(
        attempt.state,
        if case == "fail-build" {
            db::SharedBuildAttemptState::Failed
        } else {
            db::SharedBuildAttemptState::Succeeded
        }
    );
    drop(entered);
    drop(parent);
    telemetry.shutdown();
    let parent = collector.spans("telchar", "backend.execution");
    let execution = collector.spans("telchar-nomad-worker", "worker.execution");
    assert_eq!(parent.len(), 1);
    assert_eq!(execution.len(), 1);
    collector::Collector::assert_child(&parent[0], &execution[0]);
    let phases = collector.spans("telchar-nomad-worker", "worker.phase");
    for name in [
        "manifest",
        "resolve-inputs",
        "import-inputs",
        "build",
        "return-outputs",
    ] {
        let phase = phases
            .iter()
            .find(|span| collector::Collector::has_attribute(&span.attributes, "phase", name))
            .unwrap();
        collector::Collector::assert_child(&execution[0], phase);
    }
    let manifest = phases
        .iter()
        .find(|span| collector::Collector::has_attribute(&span.attributes, "phase", "manifest"))
        .unwrap();
    let callbacks = collector.spans("telchar", "nomad.callback.execution");
    assert_eq!(callbacks.len(), 1);
    collector::Collector::assert_child(manifest, &callbacks[0]);
    let exports = collector.spans("telchar", "nomad.gateway.export_inputs");
    let builds = collector.spans("telchar", "nomad.gateway.build_and_collect");
    let collections = collector.spans("telchar", "nomad.gateway.collect_outputs");
    assert_eq!(exports.len(), 1);
    assert_eq!(builds.len(), 1);
    assert_eq!(collections.len(), 1);
    collector::Collector::assert_child(&callbacks[0], &exports[0]);
    collector::Collector::assert_child(&callbacks[0], &builds[0]);
    collector::Collector::assert_child(&builds[0], &collections[0]);
    collector.assert_no_private_markers(&["private-nomad-trace-secret"]);
    collector.assert_metric_attributes_are_bounded();
    if let Some(path) = std::env::var_os("TELCHAR_NOMAD_TRACE_ARTIFACT") {
        collector.write_trace_artifact(path);
    }
}

/// Two raw requests use the very same daemon subsequently consumed by the real allocation worker.
fn seed_persistent_worker_store(path: &std::path::Path) {
    use nix_worker_protocol::{
        BuildDerivationClientRequest, BuildDerivationOutputRequest, WorkerBuildStatus,
    };
    let build: telchar::build::BuildRequest =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let outputs = build
        .output_authorities()
        .iter()
        .map(|output| BuildDerivationOutputRequest {
            name: output.name(),
            path: output.path(),
            hash_algorithm: output.hash_algorithm(),
            hash: output.hash(),
        })
        .collect::<Vec<_>>();
    let request = BuildDerivationClientRequest {
        drv_path: build.derivation_path(),
        outputs: &outputs,
        input_sources: build.input_sources(),
        platform: build.system().as_bytes(),
        builder: build.builder(),
        arguments: build.arguments(),
        environment: build.environment(),
    };
    let endpoint =
        GatewayStoreEndpoint::parse(&std::env::var("MICROVM_TEST_STORE_URI").unwrap()).unwrap();
    for expected in [WorkerBuildStatus::Built, WorkerBuildStatus::AlreadyValid] {
        let result = GatewayStoreConnection::connect_for_build(&endpoint)
            .unwrap()
            .build_derivation(&request, &mut |_| Ok(()))
            .unwrap();
        assert_eq!(result.status(), expected);
        eprintln!(
            "persistent Nomad worker store raw status: {:?}",
            result.status()
        );
    }
}
