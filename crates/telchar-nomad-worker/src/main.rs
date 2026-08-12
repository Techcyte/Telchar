fn main() -> std::process::ExitCode {
    match telchar_nomad_worker::WorkerConfig::from_environment()
        .and_then(|config| telchar_nomad_worker::authenticate(&config))
    {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("telchar-nomad-worker: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
