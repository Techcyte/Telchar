//! Loads allocation-worker configuration, connects to Telchar, and reports a bounded terminal diagnostic.

fn phase<T>(
    name: &'static str,
    operation: impl FnOnce() -> std::io::Result<T>,
) -> std::io::Result<T> {
    let started = std::time::Instant::now();
    tracing::info!(event = "worker.phase.started", phase = name);
    let result = operation();
    let elapsed_ms = started.elapsed().as_millis();
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
    let execution = tracing::info_span!("worker.execution");
    let _entered = execution.enter();
    tracing::info!(
        event = "worker.started",
        version = env!("CARGO_PKG_VERSION")
    );
    match phase("configuration", || {
        telchar_nomad_worker::WorkerConfig::from_environment().inspect_err(|error| {
            tracing::error!(event = "worker.configuration.failed", reason = %error);
        })
    })
    .and_then(|config| {
        let store_uri = config.store_uri().to_owned();
        let mut session = phase("manifest", || {
            telchar_nomad_worker::receive_manifest(&config)
        })?;
        tracing::info!(
            event = "worker.manifest.received",
            input_count = session.manifest().paths.len(),
            output_count = session.manifest().outputs.len()
        );
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
    }) {
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
