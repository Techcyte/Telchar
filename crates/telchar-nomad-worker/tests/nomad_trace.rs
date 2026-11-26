//! Connected Nomad tracing: scheduler allocation API is simulated; rendered task Env,
//! real worker process, authenticated callback, Nix build and gateway import are not.
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use telchar::store::daemon::{GatewayStoreConnection, GatewayStoreEndpoint};

#[allow(dead_code)]
#[path = "../../../tests/telemetry/collector.rs"]
mod collector;

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/worker-store.sh");
use std::io::{Read, Write};
use std::time::Instant;
use telchar::backend::{BackendKind, BackendTarget};
use telchar::nomad::backend::{
    deterministic_job_name, render_job_for_features_with_trace_context_at_attempt,
};
use telchar::persistence as db;
use telchar::service::config::ServiceConfig;

const TEST: &str = "rendered_environment_connects_real_worker_and_callback";

#[test]
fn rendered_environment_connects_real_worker_and_callback() {
    connected_callback(None);
}

#[test]
fn admitted_predecessor_output_cannot_complete_successor() {
    connected_callback(Some(false));
}

#[test]
fn admitted_predecessor_failure_cannot_fail_successor() {
    connected_callback(Some(true));
}

fn connected_callback(failed_build: Option<bool>) {
    let stale = failed_build.is_some();
    if std::env::var_os("TELCHAR_WORKER_TEST_STORE_URI").is_none() {
        let database = telchar_test_support::postgres::PostgresFixture::start();
        let collector = collector::start_collector();
        let status = Command::new("bash")
            .env("TELCHAR_WORKER_TEST_DATABASE", database.url())
            .env("OTEL_EXPORTER_OTLP_ENDPOINT", collector.endpoint())
            .env("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc")
            .arg(FIXTURE)
            .args([
                "/bin/sh",
                "-c",
                "export TELCHAR_GATEWAY_STORE_URI=\"$TELCHAR_WORKER_TEST_STORE_URI\" TELCHAR_GATEWAY_GC_ROOT_DIRECTORY=\"$TELCHAR_WORKER_STORE_ROOT/hook_delivery-roots\"; exec \"$@\"",
                "hook-trace",
            ])
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                match failed_build {
                    Some(true) => "admitted_predecessor_failure_cannot_fail_successor",
                    Some(false) => {
                        "admitted_predecessor_output_cannot_complete_successor"
                    }
                    None => TEST,
                },
                "--nocapture",
                "--test-threads=1",
            ])
            .status()
            .unwrap();
        assert!(status.success());
        if stale {
            assert!(collector.has_log_event("nomad.callback.connection_failed"));
            if failed_build == Some(true) {
                assert!(collector.has_log_event("nomad.callback.output_collection.completed"));
            }
            collector.assert_no_private_markers(&["private-nomad-trace-secret"]);
            return;
        }
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
        for operation in [
            "connect",
            "handshake",
            "query_path_info",
            "nar_from_path",
            "add_to_store_nar",
        ] {
            collector.assert_histogram_attributes(
                "telchar.store.operation.duration",
                &[("operation", operation), ("outcome", "success")],
                0.0,
            );
        }
        for side in ["source", "destination"] {
            collector.assert_histogram_attributes(
                "telchar.transfer.io.duration",
                &[("side", side), ("outcome", "success")],
                0.0,
            );
        }
        for name in [
            "telchar.post_build_hook.pending",
            "telchar.post_build_hook.oldest_age",
        ] {
            assert!(
                collector.metric_names().contains(name),
                "missing metric {name}"
            );
        }
        collector.assert_counter_attribute(
            "telchar.post_build_hook.abandoned",
            "failure_class",
            "retry-exhausted",
            1,
        );
        collector.assert_gauge("telchar.post_build_hook.pending", 0.0);
        collector.assert_gauge("telchar.post_build_hook.oldest_age", 0.0);
        collector.assert_counter("telchar.post_build_hook.runs", "succeeded", 1);
        collector.assert_histogram_sample("telchar.post_build_hook.duration", "succeeded", 0.0);
        collector.assert_no_private_markers(&["private-nomad-trace-secret"]);
        collector.assert_metric_attributes_are_bounded();
        if let Some(path) = std::env::var_os("TELCHAR_NOMAD_TRACE_ARTIFACT") {
            collector.write_trace_artifact(path);
        }
        return;
    }
    let telemetry = telchar_telemetry::Telemetry::initialize("telchar", "test").unwrap();
    let root = std::path::PathBuf::from(std::env::var_os("TELCHAR_WORKER_STORE_ROOT").unwrap());
    let database = std::env::var("TELCHAR_WORKER_TEST_DATABASE").unwrap();
    db::migrate(&database).unwrap();
    let callback = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let callback_address = callback.local_addr().unwrap();
    let scheduler = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let scheduler_address = scheduler.local_addr().unwrap();
    let secret = root.join("nomad-secret");
    std::fs::write(&secret, "private-nomad-trace-secret").unwrap();
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
    let store_uri = std::env::var("TELCHAR_WORKER_TEST_STORE_URI").unwrap();
    // Stable operator-selected endpoint, resolved to the private worker daemon by
    // the fake raw_exec launcher; the rendered Env itself is consumed unchanged.
    let worker_socket = root.join("nomad-worker.sock");
    let worker_store_uri = format!("unix://{}", worker_socket.display());
    let config_path = root.join("nomad.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"
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
            secret.display()
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
    let started = root.join("build-started");
    let release = root.join("build-release");
    let script = if stale {
        assert!(
            Command::new("mkfifo")
                .arg(&release)
                .status()
                .unwrap()
                .success()
        );
        format!(
            "> {}; read release < {}; {}",
            started.display(),
            release.display(),
            if failed_build == Some(true) {
                "exit 1"
            } else {
                "printf connected-nomad > $out"
            },
        )
    } else {
        "printf connected-nomad-log >&2; printf connected-nomad > $out".to_owned()
    };
    let derivation = format!(
        r#"Derive([("out","{output}","","")],[],["{input}"],"x86_64-linux","/bin/sh",["-e","-c","{script}"],[("out","{output}"),("system","x86_64-linux"),("builder","/bin/sh"),("name","nomad-trace")])"#
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
    let parent = tracing::info_span!(parent: None, "backend.execution");
    let entered = parent.enter();
    let context = telchar_telemetry::TraceContext::capture_current();
    let job = render_job_for_features_with_trace_context_at_attempt(
        backend,
        request.shared_build_key().as_bytes(),
        &[] as &[&str],
        2,
        &context,
    )
    .unwrap();
    let job_id = job["Job"]["ID"].as_str().unwrap().to_owned();
    let predecessor_job = deterministic_job_name(backend, request.shared_build_key().as_bytes());
    db::claim_shared_build_with_request(
        &database,
        drv,
        &request.shared_build_digest(),
        "trace-test",
        BackendKind::Nomad,
        BackendKind::Nomad.capabilities(),
        Some(&predecessor_job),
        &[output],
        &request,
    )
    .unwrap();
    db::start_shared_build(&database, drv).unwrap();
    db::retry_shared_build(
        &database,
        drv,
        &predecessor_job,
        &job_id,
        "nomad-infrastructure-lost",
        &serde_json::json!({"reason": "predecessor terminated"}),
    )
    .unwrap();
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
    let hook_delivery_roots = root.join("hook_delivery-roots");
    std::fs::create_dir(&hook_delivery_roots).unwrap();
    std::fs::set_permissions(&hook_delivery_roots, std::fs::Permissions::from_mode(0o700)).unwrap();
    let hook_delivery_store =
        telchar::store::runtime::GatewayStoreRuntime::from_environment().unwrap();
    let hook_delivery_database = db::Database::connect(&database)
        .unwrap()
        .with_post_build_hooks(true);
    let log_attempt = db::read_shared_build_attempt(&database, drv)
        .unwrap()
        .unwrap();
    let log_directory = root.join("logs");
    std::fs::create_dir(&log_directory).unwrap();
    let registry = telchar::shared_build::SharedBuildRegistry::new().with_attempt_logs(
        telchar::service::archive::AttemptLogs::new(
            log_directory.clone(),
            1048576,
            1048576,
            4,
            8388608,
        ),
    );
    let mut callback_service = telchar::nomad::callback_service::NomadCallbackService::start(
        callback,
        config.nomad_callback().unwrap().clone(),
        hook_delivery_database.clone(),
        config.nomad_backends().to_vec(),
        endpoint.clone(),
        Duration::from_secs(60),
        Arc::new(registry),
        Some(hook_delivery_store.clone()),
        telchar::service::transfer_limits::ObjectAdmissionState::new(&Default::default()),
    )
    .unwrap();
    let env = job["Job"]["TaskGroups"][0]["Tasks"][0]["Env"]
        .as_object()
        .unwrap();
    let mut worker = Command::new("bash");
    worker
        .arg(FIXTURE)
        .arg("/bin/sh")
        .args([
            "-c",
            "ln -s \"$NIX_DAEMON_SOCKET_PATH\" \"$2\"; exec \"$1\"",
            "nomad-worker",
        ])
        .arg(env!("CARGO_BIN_EXE_telchar-nomad-worker"))
        .arg(&worker_socket)
        .envs(
            env.iter()
                .map(|(name, value)| (name, value.as_str().unwrap())),
        )
        .env("NOMAD_ALLOC_ID", "trace-allocation")
        .env("NOMAD_JOB_ID", &job_id)
        .env("NOMAD_NAMESPACE", "telchar")
        .env("NOMAD_TASK_NAME", "build");
    worker
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut release_pipe = stale.then(|| {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&release)
            .unwrap()
    });
    let mut child = worker.spawn().unwrap();
    let successor = if stale {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !started.exists() {
            if Instant::now() >= deadline {
                let _ = child.kill();
                let output = child.wait_with_output().unwrap();
                panic!(
                    "worker build marker deadline: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        db::retry_shared_build(
            &database,
            drv,
            &job_id,
            "successor-job",
            "nomad-infrastructure-lost",
            &serde_json::json!({"reason": "fixture rotation"}),
        )
        .unwrap();
        let successor = db::read_shared_build(&database, drv).unwrap().unwrap();
        release_pipe
            .as_mut()
            .unwrap()
            .write_all(b"release\n")
            .unwrap();
        Some(successor)
    } else {
        None
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!(
                "worker exit deadline: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let result = child.wait_with_output().unwrap();
    if !stale {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let mut connection = postgres::Client::connect(&database, postgres::NoTls).unwrap();
            let row = connection.query_opt("SELECT object_key, complete FROM attempt_log_archives WHERE attempt_id = $1 AND state = 'available'", &[&log_attempt.attempt_id]).unwrap();
            if let Some(row) = row {
                assert!(row.get::<_, bool>(1));
                let path: String = row.get(0);
                let bytes = zstd::stream::decode_all(std::fs::File::open(path).unwrap()).unwrap();
                assert!(String::from_utf8_lossy(&bytes).contains("connected-nomad-log"));
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Nomad callback log was not archived"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    if let Some(successor) = successor {
        if failed_build == Some(true) {
            assert!(
                result.status.success(),
                "normal Nix failure completes worker transport"
            );
        } else {
            assert!(
                !result.status.success(),
                "stale worker must not receive successful output receipt"
            );
        }
        scheduler_thread.join().unwrap();
        callback_service.shutdown().unwrap();
        assert_eq!(
            db::read_shared_build(&database, drv).unwrap().unwrap(),
            successor
        );
        let mut store = GatewayStoreConnection::connect(&endpoint).unwrap();
        assert!(!store.is_valid_path(output.as_bytes()).unwrap());
        drop(entered);
        drop(parent);
        telemetry.shutdown();
        return;
    }
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    scheduler_thread.join().unwrap();
    callback_service.shutdown().unwrap();
    let mut store = GatewayStoreConnection::connect(&endpoint).unwrap();
    assert!(store.is_valid_path(output.as_bytes()).unwrap());
    assert_eq!(std::fs::read(output).unwrap(), b"connected-nomad");
    assert_eq!(
        db::read_shared_build(&database, drv)
            .unwrap()
            .unwrap()
            .state,
        db::SharedBuildState::Succeeded
    );
    assert_eq!(
        db::post_build_hooks_backlog(&hook_delivery_database)
            .unwrap()
            .0,
        1
    );
    assert_eq!(std::fs::read_dir(&hook_delivery_roots).unwrap().count(), 2);
    let uploaded = root.join("published-outputs");
    let hook = telchar::service::post_build_hook::PostBuildHook::new(
        "/bin/sh",
        ["-c", "cat > \"$1\"", "publish", uploaded.to_str().unwrap()],
        Duration::from_secs(5),
        1024,
    )
    .unwrap();
    let owner = Arc::new(
        telchar::service::singleton_ownership::SingletonOwnership::acquire(
            &database,
            Duration::from_secs(30),
        )
        .unwrap(),
    );
    let owned_database = hook_delivery_database.with_ownership(&owner);
    let mut publisher = telchar::service::daemon_services::HookDeliveryService::start(
        Arc::clone(&owner),
        owned_database.clone(),
        Some(hook),
        Default::default(),
        hook_delivery_store.clone(),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while db::post_build_hooks_backlog(&owned_database).unwrap().0 != 0
        || std::fs::read_dir(&hook_delivery_roots).unwrap().count() != 1
    {
        publisher.check().unwrap();
        assert!(Instant::now() < deadline, "owned publisher did not finish");
        std::thread::sleep(Duration::from_millis(10));
    }
    publisher.shutdown().unwrap();
    let outputs: Vec<String> = serde_json::from_slice(&std::fs::read(uploaded).unwrap()).unwrap();
    assert_eq!(outputs, [output]);

    let attempt = db::read_shared_build_attempt(&owned_database, drv)
        .unwrap()
        .unwrap();
    let lease = db::read_store_lease(
        &owned_database,
        &format!("recovery-{}-0", attempt.attempt_id),
    )
    .unwrap()
    .expect("completed callback must retain retrieval independently of hook delivery");
    assert_eq!(lease.store_path, output);
    assert_eq!(lease.purpose, db::StoreLeasePurpose::Output);
    assert_eq!(lease.state, db::StoreLeaseState::Active);
    assert!(lease.expires_at.unwrap() > std::time::SystemTime::now());
    assert_eq!(std::fs::read_dir(&hook_delivery_roots).unwrap().count(), 1);
    assert_eq!(db::post_build_hooks_backlog(&owned_database).unwrap().0, 0);
    let deletion = Command::new("nix-store")
        .args(["--store", "daemon", "--delete", output])
        .output()
        .unwrap();
    assert!(
        !deletion.status.success(),
        "retained callback output was collected"
    );
    let diagnostic = String::from_utf8_lossy(&deletion.stderr);
    assert!(diagnostic.contains("alive"), "{diagnostic}");
    let info = store.query_path_info(output.as_bytes()).unwrap().unwrap();
    let mut retrieved = Vec::new();
    store
        .nar_from_path(output.as_bytes(), info.nar_size(), &mut retrieved)
        .unwrap();
    assert!(
        retrieved
            .windows(b"connected-nomad".len())
            .any(|bytes| bytes == b"connected-nomad")
    );
    let expired_drv = "/nix/store/11111111111111111111111111111111-hook_delivery-expired.drv";
    db::claim_shared_build(
        &owned_database,
        expired_drv,
        &[2; 32],
        "local",
        BackendKind::Local,
        BackendKind::Local.capabilities(),
        None,
        &[output],
    )
    .unwrap();
    db::start_shared_build(&owned_database, expired_drv).unwrap();
    db::collect_shared_build(&owned_database, expired_drv).unwrap();
    telchar::service::hook_delivery::prepare_outputs(
        &owned_database,
        expired_drv,
        hook_delivery_store.retention().unwrap().as_mut(),
        &[output.to_owned()],
    )
    .unwrap();
    db::complete_shared_build_success(
        &owned_database,
        expired_drv,
        &serde_json::json!({}),
        Duration::from_secs(60),
    )
    .unwrap();
    let hook = telchar::service::post_build_hook::PostBuildHook::new(
        "/bin/false",
        [] as [&str; 0],
        Duration::from_secs(1),
        1024,
    )
    .unwrap();
    assert!(
        telchar::service::hook_delivery::run_attempt(
            &owned_database,
            &hook,
            Duration::from_secs(30),
            telchar::service::hook_delivery::HookDeliveryPolicy {
                maximum_age: Duration::from_nanos(1),
                ..Default::default()
            },
            None
        )
        .unwrap()
    );
    telchar::service::hook_delivery::reconcile_released_roots(
        &owned_database,
        hook_delivery_store.retention().unwrap().as_mut(),
    )
    .unwrap();
    assert_eq!(
        db::read_shared_build(&owned_database, expired_drv)
            .unwrap()
            .unwrap()
            .state,
        db::SharedBuildState::Succeeded
    );
    drop(entered);
    drop(parent);
    telemetry.shutdown();
}
