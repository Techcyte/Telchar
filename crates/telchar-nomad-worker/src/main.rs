//! Loads allocation-worker configuration, connects to Telchar, and reports a bounded terminal diagnostic.

fn phase<T>(
    name: &'static str,
    operation: impl FnOnce() -> std::io::Result<T>,
) -> std::io::Result<T> {
    let started = std::time::Instant::now();
    eprintln!("event=worker.phase.started phase={name}");
    let result = operation();
    let elapsed_ms = started.elapsed().as_millis();
    match &result {
        Ok(_) => eprintln!("event=worker.phase.completed phase={name} elapsed_ms={elapsed_ms}"),
        Err(error) => eprintln!(
            "event=worker.phase.failed phase={name} elapsed_ms={elapsed_ms} error_kind={:?}",
            error.kind()
        ),
    }
    result
}

fn main() -> std::process::ExitCode {
    match phase(
        "configuration",
        telchar_nomad_worker::WorkerConfig::from_environment,
    )
    .and_then(|config| {
        let store_uri = config.store_uri().to_owned();
        let mut session = phase("manifest", || {
            telchar_nomad_worker::receive_manifest(&config)
        })?;
        eprintln!(
            "event=worker.manifest.received input_count={} output_count={}",
            session.manifest().paths.len(),
            session.manifest().outputs.len()
        );
        let requested = phase("resolve-inputs", || session.resolve_inputs(&store_uri))?;
        eprintln!(
            "event=worker.inputs.resolved requested_count={}",
            requested.paths.len()
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
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("event=worker.failed error_kind={:?}", error.kind());
            std::process::ExitCode::FAILURE
        }
    }
}
