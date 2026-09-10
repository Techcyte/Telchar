//! Loads allocation-worker configuration, connects to Telchar, and reports a bounded terminal diagnostic.

use telchar_telemetry::Progress;

fn phase<T>(
    name: &'static str,
    operation: impl FnOnce() -> std::io::Result<T>,
) -> std::io::Result<T> {
    let started = std::time::Instant::now();
    tracing::info!(event = "worker.phase.started", phase = name);
    let progress = Progress::start(move |elapsed_ms| {
        tracing::info!(event = "worker.phase.running", phase = name, elapsed_ms, "worker phase still running");
    }).inspect_err(|error| {
        tracing::warn!(event = "worker.progress.unavailable", phase = name, error_kind = ?error.kind());
    }).ok();
    let result = operation();
    drop(progress);
    let elapsed_ms = started.elapsed().as_millis();
    opentelemetry::global::meter("telchar-nomad-worker")
        .f64_histogram("telchar.worker.phase.duration")
        .with_unit("s")
        .build()
        .record(
            started.elapsed().as_secs_f64(),
            &[
                opentelemetry::KeyValue::new("phase", name),
                opentelemetry::KeyValue::new(
                    "outcome",
                    if result.is_ok() { "success" } else { "failure" },
                ),
            ],
        );
    match &result {
        Ok(_) => tracing::info!(
            event = "worker.phase.completed",
            phase = name,
            elapsed_ms = elapsed_ms as u64
        ),
        Err(error) => {
            tracing::error!(event = "worker.phase.failed", phase = name, elapsed_ms = elapsed_ms as u64, error_kind = ?error.kind())
        }
    }
    result
}

fn main() -> std::process::ExitCode {
    let telemetry = match telchar_telemetry::Telemetry::initialize(
        "telchar-nomad-worker",
        env!("CARGO_PKG_VERSION"),
    ) {
        Ok(telemetry) => telemetry,
        Err(error) => {
            eprintln!("worker telemetry initialization failed: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let exit = run();
    telemetry.shutdown();
    exit
}

fn run() -> std::process::ExitCode {
    let configuration = tracing::info_span!("worker.configuration");
    let config = {
        let _entered = configuration.enter();
        match phase("configuration", || {
            telchar_nomad_worker::WorkerConfig::from_environment().inspect_err(|error| {
                tracing::error!(event = "worker.configuration.failed", reason = %error);
            })
        }) {
            Ok(config) => config,
            Err(error) => {
                tracing::error!(event = "worker.failed", error_kind = ?error.kind());
                return std::process::ExitCode::FAILURE;
            }
        }
    };
    let execution = tracing::info_span!("worker.execution");
    if let Err(error) = config.trace_context().set_parent(&execution) {
        tracing::error!(event = "worker.configuration.failed", reason = %error);
        return std::process::ExitCode::FAILURE;
    }
    let _entered = execution.enter();
    tracing::info!(
        event = "worker.started",
        version = env!("CARGO_PKG_VERSION")
    );
    match (|| {
        let store_uri = config.store_uri().to_owned();
        let mut session = phase("manifest", || {
            telchar_nomad_worker::receive_manifest(&config)
        })?;
        tracing::info!(
            event = "worker.manifest.received",
            input_count = session.manifest().paths.len(),
            output_count = session.manifest().outputs.len()
        );
        tracing::debug!(event = "worker.derivation.received", path = %session.manifest().derivation_path);
        let requested = phase("resolve-inputs", || session.resolve_inputs(&store_uri))?;
        tracing::info!(
            event = "worker.inputs.resolved",
            requested_count = requested.paths.len()
        );
        phase("import-inputs", || {
            session.import_requested_inputs(&store_uri, &requested)
        })?;
        let result = match phase("build", || session.build(&store_uri)) {
            Ok(result) => result,
            Err(error) => {
                phase("report-failure", || {
                    session.report_failure(&error, config.maximum_diagnostic_bytes())
                })?;
                return Err(error);
            }
        };
        phase("return-outputs", || {
            session.return_outputs(&store_uri, &result)
        })
    })() {
        Ok(()) => {
            tracing::info!(event = "worker.completed");
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!(event = "worker.failed", error_kind = ?error.kind());
            std::process::ExitCode::FAILURE
        }
    }
}
