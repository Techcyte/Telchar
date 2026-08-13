fn main() -> std::process::ExitCode {
    match telchar_nomad_worker::WorkerConfig::from_environment().and_then(|config| {
        let store_uri = config.store_uri().to_owned();
        let mut session = telchar_nomad_worker::receive_manifest(&config)?;
        let requested = session.resolve_inputs(&store_uri)?;
        session.import_requested_inputs(&store_uri, &requested)?;
        let result = session.build(&store_uri)?;
        session.return_outputs(&store_uri, &result)
    }) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("telchar-nomad-worker: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
