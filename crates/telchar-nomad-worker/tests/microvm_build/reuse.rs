//! Connected local reuse contract. Only provider launch/hook delivery and TLS termination
//! are simulated; both Rust workers, ExecutionState, Nix stores and gateway transfers are real.
use super::*;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::time::Instant;
use telchar::microvm::{Capability, rendezvous::ExecutionSessions};
use telchar::nomad::protocol::{
    FrameKind, NarMetadata, OutputReceipt, PathSet, ProtocolLimits, decode_metadata, read_frame,
};
use telchar::persistence as db;
use telchar::service::singleton_ownership::SingletonOwnership;

const TEST: &str = "microvm_reuse::sequential_jobs_reuse_one_supervised_worker";
const BENCHMARK: &str = "microvm_reuse::cold_warm_connected_benchmark";

fn benchmark_control(name: &str, default: usize, maximum: usize) -> usize {
    let value = std::env::var(name).map_or(default, |value| value.parse().unwrap());
    assert!(
        (1..=maximum).contains(&value),
        "{name} must be 1..={maximum}"
    );
    value
}

/// Opt-in: every sample executes the connected production path, never a synthetic loop.
#[test]
#[ignore = "opt-in real Nix/TLS/PostgreSQL cold/warm benchmark; see tests/microvm/connected-reuse.md"]
fn cold_warm_connected_benchmark() {
    let samples = benchmark_control("MICROVM_BENCH_SAMPLES", 3, 10);
    let mib = benchmark_control("MICROVM_BENCH_INPUT_MIB", 32, 64);
    if std::env::var_os("MICROVM_TEST_STORE_URI").is_some() {
        let mode = std::env::var("MICROVM_BENCH_MODE").unwrap();
        connected_jobs(if mode == "cold" { 1 } else { samples + 1 }, mib, &mode);
        return;
    }
    // Fresh fixture/database/worker for every cold sample. Warm has one unmeasured
    // seed job then the same number of measured jobs, all in the same helper/root.
    for mode in std::iter::repeat_n("cold", samples).chain(std::iter::once("warm")) {
        run_fixture(BENCHMARK, Some(mode));
    }
}

fn run_fixture(test: &str, mode: Option<&str>) {
    let database = telchar_test_support::postgres::PostgresFixture::start();
    let mut command = Command::new("bash");
    command
        .env("MICROVM_TEST_DATABASE", database.url())
        .env("MICROVM_TEST_CASE", "supervised")
        .env("TELCHAR_GATEWAY_DISK_RESERVE_BYTES", "67108864")
        .arg(FIXTURE)
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .stdout(Stdio::inherit());
    if let Some(mode) = mode {
        command.env("MICROVM_BENCH_MODE", mode).arg("--ignored");
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

struct SupervisedWorker {
    child: Child,
    input: ChildStdin,
    reports: std::sync::mpsc::Receiver<serde_json::Value>,
}

impl SupervisedWorker {
    fn start(root: &std::path::Path, builder: &str) -> Self {
        let trust = root.join("reuse-trust.der");
        std::fs::write(&trust, include_bytes!("../fixtures/microvm-cert.der")).unwrap();
        let mut child = Command::new("bash")
            .env("RUST_LOG", "off")
            .arg(FIXTURE)
            .arg("/bin/sh")
            .args(["-c", "exec python3 \"$@\"", "supervised-worker"])
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/microvm/supervised_worker.py"
            ))
            .arg(env!("CARGO_BIN_EXE_telchar-microvm-worker"))
            .arg(root.join("reuse-binding.json"))
            .arg(trust)
            .arg(builder)
            .arg("--persistent")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let (sender, reports) = std::sync::mpsc::sync_channel(4);
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                let report = serde_json::from_str(&line.unwrap()).unwrap();
                if sender.send(report).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            input,
            reports,
        }
    }

    fn command(&mut self, command: serde_json::Value) -> serde_json::Value {
        writeln!(self.input, "{command}").unwrap();
        self.input.flush().unwrap();
        self.reports
            .recv_timeout(Duration::from_secs(60))
            .unwrap_or_else(|error| {
                if let Some(status) = self.child.try_wait().unwrap() {
                    let mut diagnostics = String::new();
                    std::io::Read::read_to_string(
                        self.child.stderr.as_mut().unwrap(),
                        &mut diagnostics,
                    )
                    .unwrap();
                    panic!("supervised worker exited {status}: {diagnostics}");
                }
                panic!("supervised worker report deadline: {error}");
            })
    }

    fn stop(mut self) {
        writeln!(self.input, "{{\"command\":\"stop\"}}").unwrap();
        self.input.flush().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "helper stop deadline");
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = self.child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn sequential_jobs_reuse_one_supervised_worker() {
    if std::env::var_os("MICROVM_TEST_STORE_URI").is_none() {
        run_fixture(TEST, None);
        return;
    }
    connected_jobs(2, 0, "regression");
}

fn connected_jobs(jobs: usize, input_mib: usize, mode: &str) {
    let outputs: Vec<_> = (0..jobs)
        .map(|index| format!("/nix/store/{index:032}-dedicated-output"))
        .collect();
    let drvs: Vec<_> = (0..jobs)
        .map(|index| format!("/nix/store/{index:032}-dedicated.drv"))
        .collect();
    let root = std::path::PathBuf::from(std::env::var_os("MICROVM_STORE_ROOT").unwrap());
    let database = std::env::var("MICROVM_TEST_DATABASE").unwrap();
    db::migrate(&database).unwrap();
    let owner = Arc::new(SingletonOwnership::acquire(&database, Duration::from_secs(600)).unwrap());
    let sessions = Arc::new(ExecutionSessions::new(1).unwrap());
    let gateway = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let gateway_address = gateway.local_addr().unwrap();
    let mut callback = telchar::microvm::callback_service::CallbackService::start(
        gateway,
        owner.clone(),
        sessions.clone(),
        1,
        1,
        Duration::from_secs(5),
    )
    .unwrap();
    let tls = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = tls.local_addr().unwrap();
    let endpoint =
        GatewayStoreEndpoint::parse(&std::env::var("MICROVM_TEST_STORE_URI").unwrap()).unwrap();
    let mut store = GatewayStoreConnection::connect(&endpoint).unwrap();
    let source = root.join("shared-input");
    if input_mib == 0 {
        std::fs::write(&source, b"shared-input\n").unwrap();
    } else {
        // Kernel random bytes: large shared closure with no compressibility shortcut.
        let mut random = std::fs::File::open("/dev/urandom").unwrap();
        let mut file = std::fs::File::create(&source).unwrap();
        std::io::copy(
            &mut std::io::Read::take(&mut random, (input_mib * 1024 * 1024) as u64),
            &mut file,
        )
        .unwrap();
    }
    let added = Command::new("nix-store")
        .args(["--store", "local", "--add"])
        .arg(&source)
        .output()
        .unwrap();
    assert!(added.status.success());
    assert!(added.stderr.is_empty());
    let input = String::from_utf8(added.stdout).unwrap().trim().to_owned();
    let builder = std::fs::canonicalize(
        Command::new("bash")
            .args(["-c", "command -v bash"])
            .output()
            .unwrap()
            .stdout
            .strip_suffix(b"\n")
            .map(|path| std::str::from_utf8(path).unwrap())
            .unwrap(),
    )
    .unwrap();
    let builder = builder.to_str().unwrap();
    let package = std::path::Path::new(builder)
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let kind = telchar::backend::BackendKind::Microvm;
    let hasher = Command::new("bash")
        .args(["-c", "command -v sha256sum"])
        .output()
        .unwrap();
    // Keep the sha256sum basename: coreutils may use argv[0] for multicall dispatch.
    let hasher = std::path::PathBuf::from(String::from_utf8(hasher.stdout).unwrap().trim());
    let hash_package = hasher.parent().unwrap().parent().unwrap();
    let hash = Command::new(&hasher).arg(&source).output().unwrap();
    assert!(
        hash.status.success(),
        "{}",
        String::from_utf8_lossy(&hash.stderr)
    );
    let hash = String::from_utf8(hash.stdout)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();
    let expected_input = if input_mib == 0 {
        "shared-input".to_owned()
    } else {
        format!("{hash}  -")
    };
    let requests: Vec<_> = (0..jobs).map(|index| {
        let output = &outputs[index];
        let isolation = outputs.iter().filter(|path| *path != output)
            .map(|path| format!("test ! -e {path}; ")).collect::<String>();
        let read = if input_mib == 0 {
            format!("read -r value < {input}")
        } else {
            format!("value=$({} < {input})", hasher.display())
        };
        let script = format!("{isolation}{read}; printf '%s-job-{index}\\n' \"$value\" > \"$out\"");
        let script = serde_json::to_string(&script).unwrap();
        let system = format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS);
        let hash_input = if input_mib == 0 { String::new() } else { format!(",\"{}\"", hash_package.display()) };
        let derivation = format!(r#"Derive([("out","{output}","","")],[],["{input}","{}"{hash_input}],"{system}","{builder}",["-e","-c",{script}],[("out","{output}"),("system","{system}"),("builder","{builder}"),("name","dedicated")])"#, package.display());
        telchar::build::BuildRequest::from_stored_derivation(drvs[index].as_bytes(), derivation.as_bytes(), &[telchar::backend::BackendTarget::new("build", kind, &system, [] as [&str;0]).unwrap()]).unwrap()
    }).collect();
    let limits = Limits {
        maximum_paths: 500,
        maximum_nar_bytes: 320 * 1024 * 1024,
        maximum_total_bytes: 512 * 1024 * 1024,
        metadata_bytes: 8192,
        chunk_bytes: 256 * 1024,
    };
    let cold_started = Instant::now();
    let mut worker = SupervisedWorker::start(&root, builder);
    let mut previous_report = None;
    let mut previous_bearer = None;
    let mut previous_verifier = None;
    for index in 0..jobs {
        let job_started = if index == 0 {
            cold_started
        } else {
            Instant::now()
        };
        let epoch = index as i64 + 1;
        let request = &requests[index];
        db::claim_shared_build_with_request(
            owner.database(),
            &drvs[index],
            &request.shared_build_digest(),
            "build",
            kind,
            kind.capabilities(),
            None,
            &[outputs[index].as_str()],
            request,
        )
        .unwrap();
        db::start_shared_build(owner.database(), &drvs[index]).unwrap();
        let attempt = db::read_shared_build_attempt(owner.database(), &drvs[index])
            .unwrap()
            .unwrap()
            .attempt_id;
        let capability = Capability::generate().unwrap();
        if let Some(previous) = &previous_bearer {
            assert_ne!(previous, capability.bearer());
        }
        previous_bearer = Some(capability.bearer().to_owned());
        let mut payload: serde_json::Value = serde_json::from_str(
            &capability
                .launch_payload(
                    &format!("wss://localhost:{}/microvm", address.port()),
                    address,
                    "build-intent",
                    60,
                )
                .unwrap(),
        )
        .unwrap();
        payload["assignment_epoch"] = epoch.into();
        let payload = payload.to_string();
        let execution_owner = owner.clone();
        let execution_sessions = sessions.clone();
        let execution_store = endpoint.clone();
        let staging = root.clone();
        // Simulated provider control plane, not an AWS reuse acceptance claim.
        let gateway_thread = if index == 0 {
            let intent = db::MicrovmLaunchIntent {
                intent_id: "build-intent".into(),
                attempt_id: attempt,
                backend_name: "build".into(),
                account_id: "116392764028".into(),
                region: "us-west-2".into(),
                image_arn: "fixture".into(),
                image_version: "1".into(),
                vpc_egress_connector_arn:
                    "arn:aws:lambda:us-west-2:116392764028:network-connector:builder".into(),
                configuration_digest: [1; 32],
                request_digest: telchar::microvm::launch_request_digest(
                    "fixture",
                    "1",
                    "arn:aws:lambda:us-west-2:116392764028:network-connector:builder",
                    "build-token",
                    &payload,
                    60,
                ),
                client_token: "build-token".into(),
                credential_verifier: capability.verifier(),
                admission_cutoff: std::time::SystemTime::now() + Duration::from_secs(60),
                maximum_duration_seconds: 60,
            };
            assert!(db::reserve_microvm_launch(owner.database(), &intent, 1).unwrap());
            let payload = payload.clone();
            std::thread::spawn(move || {
                let (provider, response) = provider_response(None);
                let result = telchar::microvm::execution::Execution {
                    provider: &provider,
                    owner: &execution_owner,
                    sessions: &execution_sessions,
                    store: execution_store,
                    staging,
                    limits,
                }
                .run_reserved(
                    &intent,
                    &payload,
                    Instant::now() + Duration::from_secs(30),
                    |_| {},
                );
                response.join().unwrap();
                result
            })
        } else {
            assert_eq!(
                db::select_microvm_worker(
                    owner.database(),
                    attempt,
                    &[1; 32],
                    Duration::from_secs(30),
                    &capability.verifier()
                )
                .unwrap(),
                Some(("build-intent".into(), "microvm-build".into(), epoch, 60))
            );
            assert!(
                !db::admit_microvm_worker_session(
                    owner.database(),
                    "build-intent",
                    epoch - 1,
                    "microvm-build",
                    &previous_verifier.unwrap()
                )
                .unwrap(),
                "old epoch cannot be admitted"
            );
            // Equivalent to Execution::run_until's selected-worker branch: register before
            // hook delivery, receive authenticated assignment, run the production session.
            let (ready, registered) = std::sync::mpsc::sync_channel(1);
            let thread = std::thread::spawn(move || {
                let waiting = execution_sessions
                    .register("build-intent", Instant::now() + Duration::from_secs(30))
                    .unwrap();
                ready.send(()).unwrap();
                let connection = waiting.receive_active(&execution_owner).unwrap();
                telchar::microvm::session::run_admitted(
                    connection,
                    &execution_owner,
                    execution_store,
                    staging,
                    limits,
                    |_| {},
                )
            });
            registered.recv_timeout(Duration::from_secs(5)).unwrap();
            thread
        };
        previous_verifier = Some(capability.verifier());
        let hook_started = Instant::now();
        let started = worker.command(serde_json::json!({"command":"start","hook":{"microvmId":"microvm-build","runHookPayload":payload}}));
        assert_eq!(started["assignment_epoch"], epoch);
        assert_eq!(started["status"], "running");
        let hook_ms = hook_started.elapsed().as_secs_f64() * 1000.0;
        let transfer = relay(&tls, gateway_address, &input, index == 0, &mut store);
        let reset_started = Instant::now();
        assert_eq!(transfer.receipts, vec![outputs[index].clone()]);
        let result = gateway_thread.join().unwrap().unwrap();
        assert_eq!(result.outcome, BuildOutcome::Built);
        assert!(store.is_valid_path(outputs[index].as_bytes()).unwrap());
        assert_eq!(
            std::fs::read_to_string(&outputs[index]).unwrap(),
            format!("{expected_input}-job-{index}\n")
        );
        assert!(
            !db::reset_microvm_worker_until(
                owner.database(),
                "build-intent",
                epoch,
                attempt,
                Duration::from_secs(30),
                100
            )
            .unwrap(),
            "runtime completion alone cannot reset an unfinished gateway attempt"
        );
        db::collect_shared_build(owner.database(), &drvs[index]).unwrap();
        db::complete_shared_build_success(
            owner.database(),
            &drvs[index],
            &serde_json::to_value(&result).unwrap(),
            Duration::from_secs(60),
        )
        .unwrap();
        let report = worker.command(serde_json::json!({"command":"join"}));
        assert_eq!(report["status"], "succeeded");
        assert_eq!(report["reset_complete"], true);
        assert_eq!(report["binding_absent"], true);
        assert_eq!(report["private_outputs_absent"], true);
        assert_eq!(report["private_jobs_absent"], true);
        assert_eq!(report["assignment_epoch"], epoch);
        assert_eq!(report["intent_id"], "build-intent");
        assert_eq!(report["provider_id"], "microvm-build");
        if let Some(previous) = &previous_report {
            let previous: &serde_json::Value = previous;
            assert_eq!(report["helper_pid"], previous["helper_pid"]);
            assert_eq!(report["retained_root"], previous["retained_root"]);
        }
        previous_report = Some(report);
        assert_eq!(
            db::completed_microvm_worker(owner.database(), "build-intent").unwrap(),
            Some((epoch, attempt))
        );
        assert!(
            db::reset_microvm_worker_until(
                owner.database(),
                "build-intent",
                epoch,
                attempt,
                Duration::from_secs(30),
                100
            )
            .unwrap()
        );
        if input_mib > 0 {
            println!(
                "MICROVM_BENCH {}",
                serde_json::json!({
                    "mode": if mode == "warm" && index == 0 { "seed" } else { mode },
                    "epoch": epoch, "input_mib": input_mib,
                    "job_reset_ms": job_started.elapsed().as_secs_f64() * 1000.0,
                    "hook_start_ms": hook_ms,
                    "connect_auth_ms": transfer.connect_auth_ms,
                    "input_transfer_ms": transfer.input_transfer_ms,
                    "after_input_through_result_ms": transfer.after_input_through_result_ms,
                    "reset_ms": reset_started.elapsed().as_secs_f64() * 1000.0,
                    "input_nar_bytes": transfer.input_bytes, "output_nar_bytes": transfer.output_bytes
                })
            );
        }
        println!(
            "epoch {epoch}: registered distinct output, receipt accepted, shared input requested={}, clean production reset",
            index == 0
        );
    }
    worker.stop();
    callback.shutdown().unwrap();
    owner.verify().unwrap();
    let mut control = postgres::Client::connect(&database, postgres::NoTls).unwrap();
    let row = control.query_one("SELECT count(*),bool_and(provider_id='microvm-build' AND phase='bound' AND reservation_released_at IS NULL) FROM microvm_launches", &[]).unwrap();
    assert_eq!(
        row.get::<_, i64>(0),
        1,
        "one lifetime reservation, no second launch"
    );
    assert!(row.get::<_, bool>(1));
    let rows = control.query("SELECT assignment_epoch,admitted_at IS NOT NULL,completed_at IS NOT NULL,credential_verifier FROM microvm_assignments ORDER BY assignment_epoch", &[]).unwrap();
    assert_eq!(rows.len(), jobs);
    for (index, row) in rows.iter().enumerate() {
        assert_eq!(row.get::<_, i64>(0), index as i64 + 1);
        assert!(row.get::<_, bool>(1));
        assert!(row.get::<_, bool>(2));
    }
    for pair in rows.windows(2) {
        assert_ne!(pair[0].get::<_, Vec<u8>>(3), pair[1].get::<_, Vec<u8>>(3));
    }
    let row = control.query_one("SELECT state,completed_jobs,owner_generation FROM microvm_workers WHERE intent_id='build-intent'", &[]).unwrap();
    assert_eq!(row.get::<_, String>(0), "idle");
    assert_eq!(row.get::<_, i64>(1), jobs as i64);
    let generation = control
        .query_one(
            "SELECT generation FROM singleton_ownership WHERE owner_kind='daemon'",
            &[],
        )
        .unwrap()
        .get::<_, i64>(0);
    assert_eq!(row.get::<_, i64>(2), generation);
}

#[derive(Default)]
struct TransferMeasurement {
    receipts: Vec<String>,
    input_bytes: u64,
    output_bytes: u64,
    connect_auth_ms: f64,
    input_transfer_ms: f64,
    after_input_through_result_ms: f64,
}

fn relay(
    listener: &std::net::TcpListener,
    gateway: std::net::SocketAddr,
    input: &str,
    cold: bool,
    store: &mut GatewayStoreConnection,
) -> TransferMeasurement {
    let started = Instant::now();
    let mut measurement = TransferMeasurement::default();
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "worker TLS connect deadline");
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("TLS accept: {error}"),
        }
    };
    let mut socket = super::tls_worker_socket(stream);
    use tungstenite::client::IntoClientRequest;
    let mut request = format!("ws://{gateway}/microvm")
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "sec-websocket-protocol",
        "telchar-microvm-v1".parse().unwrap(),
    );
    let stream = std::net::TcpStream::connect(gateway).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let (mut upstream, _) = tungstenite::client(request, stream).unwrap();
    upstream.send(socket.read().unwrap()).unwrap();
    socket.send(upstream.read().unwrap()).unwrap();
    let mut requested_input = false;
    measurement.connect_auth_ms = started.elapsed().as_secs_f64() * 1000.0;
    let mut inputs_finished = Instant::now();
    loop {
        let message = socket.read().unwrap();
        let frame = read_frame(
            &mut message.clone().into_data().as_ref(),
            ProtocolLimits::new(8192, 256 * 1024),
        )
        .unwrap();
        upstream.send(message).unwrap();
        match frame.kind() {
            FrameKind::InputRequest => {
                let inputs_started = Instant::now();
                let requested: PathSet = decode_metadata(frame.metadata(), 8192).unwrap();
                requested_input = requested.paths.iter().any(|path| path == input);
                for _ in requested.paths {
                    loop {
                        let message = upstream.read().unwrap();
                        let frame = read_frame(
                            &mut message.clone().into_data().as_ref(),
                            ProtocolLimits::new(8192, 256 * 1024),
                        )
                        .unwrap();
                        let chunk: NarMetadata = decode_metadata(frame.metadata(), 8192).unwrap();
                        measurement.input_bytes += frame.payload().len() as u64;
                        socket.send(message).unwrap();
                        if chunk.final_chunk {
                            break;
                        }
                    }
                }
                measurement.input_transfer_ms = inputs_started.elapsed().as_secs_f64() * 1000.0;
                inputs_finished = Instant::now();
            }
            FrameKind::OutputNar => {
                measurement.output_bytes += frame.payload().len() as u64;
                let chunk: NarMetadata = decode_metadata(frame.metadata(), 8192).unwrap();
                if chunk.final_chunk {
                    let message = upstream.read().unwrap();
                    let frame = read_frame(
                        &mut message.clone().into_data().as_ref(),
                        ProtocolLimits::new(8192, 256 * 1024),
                    )
                    .unwrap();
                    assert_eq!(frame.kind(), FrameKind::OutputReceipt);
                    let receipt: OutputReceipt = decode_metadata(frame.metadata(), 8192).unwrap();
                    assert!(receipt.accepted);
                    assert!(
                        store
                            .query_path_info(receipt.path.as_bytes())
                            .unwrap()
                            .is_some(),
                        "receipt follows authoritative gateway registration"
                    );
                    measurement.receipts.push(receipt.path);
                    socket.send(message).unwrap();
                }
            }
            FrameKind::BuildResult => break,
            _ => {}
        }
    }
    assert_eq!(requested_input, cold, "only cold job requests shared input");
    measurement.after_input_through_result_ms = inputs_finished.elapsed().as_secs_f64() * 1000.0;
    measurement
}
